//! Correctness smoke tests for concurrent use of the safe `lean-rs`
//! surface.
//!
//! These tests do **not** measure throughput or parallel speedup; they
//! verify that the documented concurrency contract holds:
//!
//! 1. `LeanRuntime::init` is idempotent across many calling threads.
//! 2. A worker thread that holds a `LeanThreadGuard` for the duration of
//!    its Lean work can build its own host/capabilities/session chain
//!    and run host calls independently of other worker threads.
//! 3. Per-thread `SessionPool` instances live and die with their owning
//!    worker, never crossing a thread boundary.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

#[path = "support/fixed_fixture.rs"]
mod fixture;

use std::path::PathBuf;
use std::sync::{Barrier, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use lean_rs::{LeanRuntime, LeanThreadGuard};
use lean_rs_host::{LeanCapabilities, LeanHost, LeanProgressEvent, LeanProgressSink, SessionPool};

fn fixture_lake_root() -> PathBuf {
    fixture::native_fixture_root()
}

fn initialized_fixture<'h>(host: &'h LeanHost<'static>) -> LeanCapabilities<'static, 'h> {
    // Native setup rejects racing initialization, so serialize capability setup.
    static FIXTURE_INIT: Mutex<()> = Mutex::new(());
    let _guard = FIXTURE_INIT.lock().expect("fixture initialization lock");
    fixture::native_capabilities(host).expect("worker loads capabilities")
}

#[test]
fn many_threads_init_idempotently() {
    // Drive the OnceLock contract from many concurrent writers. All
    // callers must observe `Ok(_)`; the underlying initializer runs at
    // most once thanks to `OnceLock::get_or_init`.
    let _ = LeanRuntime::init().expect("main-thread init must succeed");

    const WORKERS: usize = 16;
    let barrier = std::sync::Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            // The `&'static LeanRuntime` borrow is `!Send`, so we cannot
            // ship it back across the join. Reduce to its address (a
            // plain `usize`) for cross-thread comparison.
            let runtime = LeanRuntime::init().expect("worker init must succeed");
            std::ptr::from_ref(runtime).cast::<()>() as usize
        }));
    }
    let first = handles
        .pop()
        .expect("at least one worker")
        .join()
        .expect("first worker must not panic");
    for handle in handles {
        let next = handle.join().expect("worker must not panic");
        assert_eq!(first, next, "every concurrent init() must return the same ZST address");
    }
}

#[test]
fn independent_worker_sessions_run_in_parallel() {
    // Each worker builds its own host/capabilities/session chain on a
    // freshly-attached OS thread, runs a `query_declaration` and a
    // `list_declarations`, and drops everything in scope before its
    // attach guard.
    let _ = LeanRuntime::init().expect("main-thread init must succeed");
    let lake_root = fixture_lake_root();

    const WORKERS: usize = 4;

    #[derive(Default)]
    struct WorkerBarrier {
        arrived: Mutex<usize>,
        all_arrived: Condvar,
    }

    impl WorkerBarrier {
        fn wait(&self, phase: &str) {
            let count = {
                let mut arrived = self.arrived.lock().expect("worker barrier lock");
                *arrived = arrived.saturating_add(1);
                *arrived
            };
            if count == WORKERS {
                self.all_arrived.notify_all();
            }
            // A failed worker must not leave the others blocked indefinitely.
            let count = *self
                .all_arrived
                .wait_timeout_while(
                    self.arrived.lock().expect("worker barrier lock"),
                    Duration::from_secs(180),
                    |count| *count < WORKERS,
                )
                .expect("worker barrier wait")
                .0;
            assert_eq!(count, WORKERS, "every worker must reach {phase}");
        }
    }

    impl LeanProgressSink for WorkerBarrier {
        fn report(&self, event: LeanProgressEvent) {
            if event.phase == "declaration_kind_bulk" && event.current == 1 {
                self.wait("the first lookup inside Lean");
            }
        }
    }

    let setup = Mutex::new(());
    let sessions_ready = WorkerBarrier::default();
    let queries_in_flight = WorkerBarrier::default();
    thread::scope(|scope| {
        for worker_id in 0..WORKERS {
            let lake_root = lake_root.clone();
            let (setup, sessions_ready, queries_in_flight) = (&setup, &sessions_ready, &queries_in_flight);
            scope.spawn(move || {
                let runtime = LeanRuntime::init().expect("worker init must succeed");
                let _guard = LeanThreadGuard::attach(runtime);
                // Imports take the production process-wide lock. Set up one
                // session at a time on its own worker before starting queries.
                let setup_guard = setup.lock().expect("session setup lock");
                let host = LeanHost::from_lake_project(runtime, lake_root).expect("worker host opens");
                let caps = initialized_fixture(&host);
                let mut session = caps
                    .session(&fixture::native_imports(&caps, "LeanRsFixture.Handles"), None, None)
                    .expect("worker imports its own session");
                drop(setup_guard);
                sessions_ready.wait("session setup");

                // The first progress event runs inside the Lean call. Keep
                // each second lookup pending until all four calls are in flight.
                let kinds = session
                    .declaration_kind_bulk(
                        &["LeanRsFixture.Handles.nameAnonymous", "LeanRsFixture.Handles.nameMkStr"],
                        None,
                        Some(queries_in_flight),
                    )
                    .expect("overlapping bulk queries succeed");
                assert_eq!(kinds, ["definition", "definition"]);

                let decl = session
                    .query_declaration("LeanRsFixture.Handles.nameAnonymous", None)
                    .expect("query_declaration on a fixture name succeeds");
                drop(decl);

                let names = session
                    .list_declarations(None)
                    .expect("list_declarations succeeds on imported session");
                assert!(
                    !names.is_empty(),
                    "worker {worker_id}: list_declarations must see at least one name",
                );
                drop(names);
            });
        }
    });
    assert_eq!(
        queries_in_flight.arrived.into_inner().expect("worker barrier lock"),
        WORKERS,
        "every query must report progress from Lean"
    );
}

#[test]
fn per_worker_session_pool_under_concurrency() {
    // `SessionPool` is `!Send + !Sync`; the supported deployment is one
    // pool per worker thread, all anchored to the shared `'static`
    // runtime. Verify that two workers can drive independent pools
    // concurrently against the same Lake project.
    let _ = LeanRuntime::init().expect("main-thread init must succeed");
    let lake_root = fixture_lake_root();

    const WORKERS: usize = 2;
    const CHECKOUTS_PER_WORKER: usize = 6;
    let barrier = std::sync::Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let barrier = std::sync::Arc::clone(&barrier);
        let lake_root = lake_root.clone();
        handles.push(thread::spawn(move || {
            let runtime = LeanRuntime::init().expect("worker init must succeed");
            let _guard = LeanThreadGuard::attach(runtime);
            let host = LeanHost::from_lake_project(runtime, lake_root).expect("host opens");
            let caps = initialized_fixture(&host);
            let pool = SessionPool::with_capacity(runtime, 2);
            barrier.wait();
            let imports = fixture::native_imports(&caps, "LeanRsFixture.Handles");
            for _ in 0..CHECKOUTS_PER_WORKER {
                let mut session = pool
                    .acquire(&caps, &imports, None, None)
                    .expect("pool acquire returns a session");
                let kind = session
                    .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
                    .expect("declaration_kind succeeds");
                drop(kind);
            }
            let stats = pool.stats();
            assert!(
                stats.acquired >= CHECKOUTS_PER_WORKER as u64,
                "pool stats must record every acquire",
            );
        }));
    }
    for handle in handles {
        handle.join().expect("worker thread must not panic");
    }
}
