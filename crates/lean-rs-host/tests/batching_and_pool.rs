//! Integration tests for prompt 20 — bulk session methods,
//! `LeanSession::call_capability`, and the `SessionPool` /
//! `PooledSession` pair.
//!
//! These tests live in `tests/` (not in `src/host/tests.rs`) because
//! every test imports a fresh `Lean.Environment`, and accumulating
//! dozens of imports in a single test process exhausts the host's
//! resident-set budget. Integration tests run as a separate binary, so
//! their imports do not compound with the lower-layer unit tests.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::PathBuf;
use std::time::Instant;

use lean_rs::module::LeanIo;
use lean_rs::{HostStage, LeanError, LeanRuntime};
use lean_rs_host::{LeanCapabilities, LeanElabOptions, LeanHost, LeanSession, SessionPool};

// -- fixture setup -------------------------------------------------------

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
}

fn session_over_handles<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.Handles"])
        .expect("session imports cleanly")
}

fn session_over_elaboration<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsHostShims.Elaboration"])
        .expect("session imports cleanly")
}

// -- query_declarations_bulk --------------------------------------------

#[test]
fn query_declarations_bulk_returns_all_for_existing_names() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let baseline = session.stats();
    // The Lean shim only round-trips `axiom`/`definition`/`theorem`/
    // `opaque` declarations; pick three fixture defs so the bulk path
    // exercises N == 3 without tripping the inductive/constructor
    // exclusion baked into the singular shim.
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];
    let decls = session
        .query_declarations_bulk(&names)
        .expect("bulk query succeeds for fully-resolvable name list");

    assert_eq!(decls.len(), 3, "bulk returns one slot per input name");
    drop(decls);

    let after = session.stats();
    // make_name dispatches three times (one per input name) + one bulk
    // dispatch = 4 new FFI calls. batch_items records 3 (the per-item
    // count is the batch length, not name_from_string invocations).
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        4,
        "bulk path costs N + 1 FFI calls (one bulk + N name_from_string), got delta {}",
        after.ffi_calls - baseline.ffi_calls,
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        3,
        "batch_items records the per-item batch length",
    );
}

#[test]
fn query_declarations_bulk_errors_on_missing_name() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let err = session
        .query_declarations_bulk(&[
            "LeanRsFixture.Handles.nameAnonymous",
            "This.Name.Does.Not.Exist",
            "LeanRsFixture.Handles.nameMkStr",
        ])
        .expect_err("bulk must error when any input name is missing");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Conversion);
            assert!(
                failure.message().contains("This.Name.Does.Not.Exist"),
                "diagnostic must name the missing entry, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Conversion), got LeanException {exc:?}"),
        _ => panic!("LeanError gained a new variant; update this match"),
    }
}

#[test]
fn query_declarations_bulk_empty_input_is_no_op() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let baseline = session.stats();
    let decls = session
        .query_declarations_bulk(&[])
        .expect("empty input returns empty vec");
    assert!(decls.is_empty(), "empty input yields empty output");
    assert_eq!(session.stats(), baseline, "empty bulk must not record an FFI call");
}

// -- elaborate_bulk -----------------------------------------------------

#[test]
fn elaborate_bulk_returns_per_source_results() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let baseline = session.stats();
    let opts = LeanElabOptions::new();
    let outcomes = session
        .elaborate_bulk(&["(1 + 2 : Nat)", "1 +", "(1 + \"hi\" : Nat)"], &opts)
        .expect("bulk elaborate routes through the host stack cleanly");

    assert_eq!(outcomes.len(), 3, "bulk returns one slot per input source");
    assert!(outcomes[0].is_ok(), "first source elaborates successfully");
    assert!(outcomes[1].is_err(), "second source is a parse failure");
    assert!(outcomes[2].is_err(), "third source is a type-mismatch failure");

    let after = session.stats();
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        1,
        "elaborate_bulk dispatches once regardless of batch size",
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        3,
        "batch_items records the per-source batch length",
    );
}

#[test]
fn elaborate_bulk_empty_input_is_no_op() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let baseline = session.stats();
    let outcomes = session
        .elaborate_bulk(&[], &LeanElabOptions::new())
        .expect("empty input returns empty vec");
    assert!(outcomes.is_empty(), "empty input yields empty output");
    assert_eq!(session.stats(), baseline, "empty bulk must not record an FFI call");
}

// -- call_capability ----------------------------------------------------

#[test]
fn call_capability_dispatches_pure_fixture_export() {
    // `lean_rs_fixture_u64_mul : UInt64 -> UInt64 -> UInt64 := (·*·)` is
    // not a session-fixed symbol; routing through `call_capability`
    // proves the generic Args/R path resolves and dispatches against an
    // arbitrary capability-dylib export.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let baseline = session.stats();
    let product: u64 = session
        .call_capability::<(u64, u64), u64>("lean_rs_fixture_u64_mul", (3, 4))
        .expect("call_capability dispatches the pure export");
    assert_eq!(product, 12);

    let after = session.stats();
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        1,
        "call_capability records exactly one FFI dispatch",
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        0,
        "call_capability is not a bulk operation",
    );
}

#[test]
fn call_capability_dispatches_io_fixture_export() {
    // `lean_rs_fixture_io_success_unit : IO Unit := pure ()` exercises
    // the `R = LeanIo<T>` path end-to-end: fused decode_io +
    // T::try_from_lean. Unit instead of Nat because Lean's `Nat`
    // encoding is `lean_object`-boxed and the `Obj` decoder lives
    // pub(crate); the simpler IO Unit return covers the same fused
    // decoding path the bulk and singular IO methods rely on.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    session
        .call_capability::<(), LeanIo<()>>("lean_rs_fixture_io_success_unit", ())
        .expect("call_capability dispatches the IO export");
}

#[test]
fn call_capability_unknown_symbol_is_link_error() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let err = session
        .call_capability::<(), LeanIo<u64>>("lean_rs_fixture_no_such_export", ())
        .expect_err("missing symbol must surface as a host link error");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Link);
            assert!(
                failure.message().contains("lean_rs_fixture_no_such_export"),
                "diagnostic must name the missing symbol, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Link) failure, got LeanException {exc:?}"),
        _ => panic!("LeanError gained a new variant; update this match"),
    }
}

// -- SessionPool / PooledSession ----------------------------------------

#[test]
fn session_pool_reuses_session() {
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime, 4);
    let imports = ["LeanRsFixture.Handles"];

    {
        let mut sess = pool.acquire(&caps, &imports).expect("first acquire imports fresh");
        let kind = sess
            .declaration_kind("LeanRsFixture.Handles.nameAnonymous")
            .expect("query");
        assert_eq!(kind, "definition");
    }
    {
        let mut sess = pool
            .acquire(&caps, &imports)
            .expect("second acquire reuses the released env");
        let kind = sess
            .declaration_kind("LeanRsFixture.Handles.nameAnonymous")
            .expect("query");
        assert_eq!(kind, "definition");
    }

    let stats = pool.stats();
    assert_eq!(stats.imports_performed, 1, "second acquire must reuse, not re-import");
    assert_eq!(stats.reused, 1, "one acquire was a cache hit");
    assert_eq!(stats.acquired, 2, "both acquires accounted for");
    assert_eq!(stats.released_to_pool, 2, "both Drops returned to the pool");
    assert_eq!(stats.released_dropped, 0, "capacity 4 is well above 1 live session");
}

#[test]
fn session_pool_capacity_caps_storage() {
    // Two concurrent sessions, capacity 1: on release, the first drop
    // pushes onto the free list and the second drop overflows. Keeps
    // the test's peak memory bounded at two imported environments.
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime, 1);
    let imports = ["LeanRsFixture.Handles"];

    let s1 = pool.acquire(&caps, &imports).expect("acquire #1");
    let s2 = pool.acquire(&caps, &imports).expect("acquire #2");
    drop(s1);
    drop(s2);

    assert_eq!(pool.len(), 1, "free list must not exceed capacity");
    let stats = pool.stats();
    assert_eq!(
        stats.imports_performed, 2,
        "no entries on free list during acquire phase"
    );
    assert_eq!(stats.released_to_pool, 1, "first release fits under capacity");
    assert_eq!(stats.released_dropped, 1, "second release drops the env (pool full)");
}

#[test]
fn session_pool_distinct_imports_do_not_match() {
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime, 4);

    drop(pool.acquire(&caps, &["LeanRsFixture.Handles"]).expect("acquire A"));
    drop(
        pool.acquire(&caps, &["LeanRsHostShims.Elaboration"])
            .expect("acquire B"),
    );

    let stats = pool.stats();
    assert_eq!(
        stats.imports_performed, 2,
        "different import lists are different cache keys; both must import fresh",
    );
    assert_eq!(stats.reused, 0, "no key collision possible across distinct imports");
    assert_eq!(pool.len(), 2, "both envs sit on the free list");
}

#[test]
fn session_pool_zero_capacity_never_reuses() {
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime, 0);
    let imports = ["LeanRsFixture.Handles"];

    drop(pool.acquire(&caps, &imports).expect("acquire #1"));
    drop(pool.acquire(&caps, &imports).expect("acquire #2"));

    let stats = pool.stats();
    assert_eq!(stats.imports_performed, 2, "capacity 0 degenerates to always-import");
    assert_eq!(stats.released_dropped, 2, "every release drops");
    assert_eq!(pool.len(), 0, "free list never holds anything");
}

#[test]
fn session_pool_drain_drops_cached_entries() {
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime, 4);
    let imports = ["LeanRsFixture.Handles"];

    drop(pool.acquire(&caps, &imports).expect("warm pool"));
    assert_eq!(pool.len(), 1, "warm pool has one cached environment");

    let drained = pool.drain();
    assert_eq!(drained, 1, "drain returns the cached-entry count");
    assert_eq!(pool.len(), 0, "drain empties the free list");

    let stats = pool.stats();
    assert_eq!(stats.imports_performed, 1);
    assert_eq!(stats.reused, 0);
    assert_eq!(stats.released_to_pool, 1);
    assert_eq!(stats.released_dropped, 0);
    assert_eq!(stats.drains, 1);
    assert_eq!(stats.drained, 1);

    drop(
        pool.acquire(&caps, &imports)
            .expect("drained pool must import fresh on next acquire"),
    );
    let stats = pool.stats();
    assert_eq!(
        stats.imports_performed, 2,
        "drain removes the only reusable entry, so the next acquire imports fresh",
    );
    assert_eq!(stats.reused, 0);
    assert_eq!(stats.released_to_pool, 2);
    assert_eq!(pool.len(), 1, "the second release repopulates the pool");
}

#[test]
fn session_pool_drain_leaves_checked_out_sessions_valid() {
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime, 2);
    let imports = ["LeanRsFixture.Handles"];

    let mut sess = pool.acquire(&caps, &imports).expect("checked-out session");
    assert_eq!(pool.drain(), 0, "no free-list entries while the session is checked out");

    let kind = sess
        .declaration_kind("LeanRsFixture.Handles.nameAnonymous")
        .expect("checked-out session remains usable after drain");
    assert_eq!(kind, "definition");

    drop(sess);
    assert_eq!(pool.len(), 1, "dropping the checked-out session returns it to the pool");
    assert_eq!(pool.drain(), 1, "a later drain can release the returned entry");
    assert_eq!(pool.len(), 0);

    let stats = pool.stats();
    assert_eq!(stats.imports_performed, 1);
    assert_eq!(stats.reused, 0);
    assert_eq!(stats.released_to_pool, 1);
    assert_eq!(stats.released_dropped, 0);
    assert_eq!(stats.drains, 2);
    assert_eq!(stats.drained, 1);
}

// -- timing note: bulk vs singular --------------------------------------
//
// Informational only. Per the project's "no performance claim without
// numbers" rule, this test prints the numbers and asserts only the
// inequality direction.

#[test]
fn bulk_vs_singular_timing_note() {
    const ITEMS: usize = 8;
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    // Round-trippable fixture defs only (axiom/def/theorem/opaque). The
    // set is intentionally homogeneous so the singular vs. bulk timing
    // comparison stays apples-to-apples.
    let names: [&str; ITEMS] = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.nameMkNum",
        "LeanRsFixture.Handles.nameToString",
        "LeanRsFixture.Handles.nameBeq",
        "LeanRsFixture.Handles.levelZero",
        "LeanRsFixture.Handles.levelSucc",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let start_singular = Instant::now();
    for name in names {
        session.query_declaration(name).expect("singular query for known name");
    }
    let singular_elapsed = start_singular.elapsed();

    let start_bulk = Instant::now();
    let decls = session
        .query_declarations_bulk(&names)
        .expect("bulk query for known names");
    let bulk_elapsed = start_bulk.elapsed();

    assert_eq!(decls.len(), ITEMS);

    println!(
        "bulk_vs_singular_timing_note: \
         {ITEMS} singular queries took {singular_elapsed:?}; \
         one bulk query took {bulk_elapsed:?}",
    );
    // No threshold asserted — for the tiny fixture queries used here
    // (each is a microsecond-scale `env.find?` lookup), bulk's
    // single-Vec allocation overhead can exceed the per-call FFI cost
    // it saves. The amortisation win is asymptotic and shows up
    // clearly only when each item carries real Lean-side work (e.g.
    // pretty-printing, kernel checking). The numbers are recorded for
    // reference, not asserted.
    //
    // The `query_declarations_bulk_returns_all_for_existing_names`
    // test does pin the *FFI-call count* contract — bulk is one
    // dispatch regardless of N — which is the structural guarantee
    // worth asserting.
}
