//! Correctness smoke tests for prompt 24 — concurrent use of the safe
//! `lean-rs` surface.
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

use std::path::PathBuf;
use std::sync::Barrier;
use std::thread;

use lean_rs::{LeanRuntime, LeanThreadGuard};
use lean_rs_host::{LeanHost, SessionPool};

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
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
    let barrier = std::sync::Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::with_capacity(WORKERS);
    for worker_id in 0..WORKERS {
        let barrier = std::sync::Arc::clone(&barrier);
        let lake_root = lake_root.clone();
        handles.push(thread::spawn(move || {
            let runtime = LeanRuntime::init().expect("worker init must succeed");
            let _guard = LeanThreadGuard::attach(runtime);
            let host = LeanHost::from_lake_project(runtime, lake_root).expect("worker host opens");
            let caps = host
                .load_capabilities("lean_rs_fixture", "LeanRsFixture")
                .expect("worker loads capabilities");
            // Stagger session imports past a barrier so several threads
            // do Lean work simultaneously.
            barrier.wait();
            let mut session = caps
                .session(&["LeanRsFixture.Handles"])
                .expect("worker imports its own session");

            let decl = session
                .query_declaration("LeanRsFixture.Handles.nameAnonymous")
                .expect("query_declaration on a fixture name succeeds");
            drop(decl);

            let names = session
                .list_declarations()
                .expect("list_declarations succeeds on imported session");
            assert!(
                !names.is_empty(),
                "worker {worker_id}: list_declarations must see at least one name",
            );
            drop(names);
        }));
    }
    for handle in handles {
        handle.join().expect("worker thread must not panic");
    }
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
            let caps = host
                .load_capabilities("lean_rs_fixture", "LeanRsFixture")
                .expect("loads capabilities");
            let pool = SessionPool::with_capacity(runtime, 2);
            barrier.wait();
            for _ in 0..CHECKOUTS_PER_WORKER {
                let mut session = pool
                    .acquire(&caps, &["LeanRsFixture.Handles"])
                    .expect("pool acquire returns a session");
                let kind = session
                    .declaration_kind("LeanRsFixture.Handles.nameAnonymous")
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
