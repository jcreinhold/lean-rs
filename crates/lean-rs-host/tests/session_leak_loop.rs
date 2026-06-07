//! Session create/drop and pool acquire/release loops.
//!
//! These tests exist to make leak regressions visible. The Rust side has
//! no oracle for "did `Drop` actually release everything Lean
//! allocated"; the test loops a session lifecycle long enough that any
//! per-iteration leak compounds into something `AddressSanitizer` or
//! `LeakSanitizer` can flag on the dedicated CI job. On stable
//! `cargo test` the same loops run at a tiny iteration count so the
//! suite stays fast and the long variants live behind `#[ignore]` /
//! `--include-ignored`.
//!
//! The `PoolStats` / `SessionStats` assertions are not leak detection
//! per se—they pin the wrapper's bookkeeping so a future refactor
//! cannot silently change how acquires, releases, and drops are
//! counted. The sanitizer run is what actually proves the absence of
//! leaks.

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use lean_rs::LeanRuntime;
use lean_rs_host::{LeanCapabilities, LeanHost, LeanSession, LeanSessionImportProfile, SessionPool};

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
    caps.session(&["LeanRsFixture.Handles"], None, None)
        .expect("session imports cleanly")
}

/// Iteration counts. Stable `cargo test` uses the "small" defaults and
/// `LEAN_RS_LEAK_LOOP_ITERS` overrides them for the sanitizer job.
fn iters(default: usize) -> usize {
    std::env::var("LEAN_RS_LEAK_LOOP_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(default)
}

#[test]
fn session_create_drop_loop_small() {
    // Eight session lifecycles is enough to exercise the import +
    // Environment drop path on every supported platform under the
    // normal test budget. Each create/drop is independent—sessions do
    // not share state on the Rust side—so a leak would multiply
    // linearly with `n` under the sanitizer's longer override.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");

    for _ in 0..iters(8) {
        let session = session_over_handles(&caps);
        drop(session);
    }
}

#[test]
fn session_leak_loop_import_stats_reports_fixture_shape() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let session = session_over_handles(&caps);
    let stats = session.import_stats();

    assert!(
        stats
            .direct_import_names
            .iter()
            .any(|name| name == "LeanRsFixture.Handles"),
        "direct imports should include the requested fixture module: {:?}",
        stats.direct_import_names
    );
    assert!(
        stats.effective_module_count > 0,
        "effective module count must be positive"
    );
    assert!(
        stats.compacted_region_count > 0,
        "compacted region count must be positive"
    );
    assert!(stats.imported_bytes > 0, "imported bytes must be positive");
    assert!(
        stats.imported_constant_count > 0,
        "imported constant count must be positive"
    );
    assert_eq!(stats.import_level, "private");
    assert!(!stats.import_all);
    assert!(stats.load_exts);
}

#[test]
fn session_leak_loop_import_stats_reports_explicit_full_private_compat() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let session = caps
        .session_with_profile(
            &["LeanRsFixture.Handles"],
            LeanSessionImportProfile::FullPrivateCompat,
            None,
            None,
        )
        .expect("compat session imports cleanly");
    let stats = session.import_stats();

    assert_eq!(stats.import_level, "private");
    assert!(stats.import_all);
    assert!(stats.load_exts);
}

/// Long-form session lifecycle loop. Off by default; run via
/// `cargo test --tests -- --ignored` or under the sanitizer CI job
/// where the leak signal compounds enough to be detected.
#[test]
#[ignore = "long-running leak surface; run under sanitizer CI"]
fn session_create_drop_loop_long() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");

    let n = iters(512);
    for _ in 0..n {
        let session = session_over_handles(&caps);
        drop(session);
    }
}

#[test]
fn pool_acquire_release_loop_small() {
    // Reuse a four-slot pool across many acquires. After the warm-up
    // imports, every subsequent acquire must be a cache hit, so the
    // total import count stays bounded even as the acquire count grows.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime(), 4);
    let imports = ["LeanRsFixture.Handles"];

    let n = iters(16);
    for _ in 0..n {
        let sess = pool
            .acquire(&caps, &imports, None, None)
            .expect("acquire from warm pool");
        drop(sess);
    }

    let stats = pool.stats();
    let n_u64 = u64::try_from(n).expect("loop count fits u64");
    assert_eq!(stats.acquired, n_u64, "every iteration accounted for");
    assert_eq!(
        stats.released_to_pool, n_u64,
        "every release fits under the four-slot capacity"
    );
    assert_eq!(
        stats.released_dropped, 0,
        "pool capacity is comfortable for one live session"
    );
    assert_eq!(
        stats.imports_performed, 1,
        "after the first acquire every subsequent one must reuse the cached env"
    );
    assert_eq!(
        stats.reused,
        n_u64 - 1,
        "n acquires, one fresh import, n - 1 cache hits"
    );
}

/// Long-form pool acquire/release loop. Off by default; intended for
/// the sanitizer CI job.
#[test]
#[ignore = "long-running pool leak surface; run under sanitizer CI"]
fn pool_acquire_release_loop_long() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime(), 4);
    let imports = ["LeanRsFixture.Handles"];

    let n = iters(2048);
    for _ in 0..n {
        let sess = pool
            .acquire(&caps, &imports, None, None)
            .expect("acquire from warm pool");
        drop(sess);
    }

    // Sanity assertions—the long loop must still reuse the same one
    // cached environment, otherwise the leak surface we are measuring is
    // not the one we think.
    let stats = pool.stats();
    let n_u64 = u64::try_from(n).expect("loop count fits u64");
    assert_eq!(stats.acquired, n_u64);
    assert_eq!(stats.imports_performed, 1);
    assert_eq!(stats.reused, n_u64 - 1);
}

#[test]
fn pool_overflow_eviction_loop_small() {
    // A capacity-1 pool that sees concurrent acquires forces the drop
    // path on release: the first release fits under the cap and the
    // second overflows. Looping that pattern exercises the
    // pool-eviction Drop on the released env every iteration, which is
    // the path most likely to leak if the wrapper forgets a
    // `lean_dec`.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime(), 1);
    let imports = ["LeanRsFixture.Handles"];

    let n = iters(4);
    for _ in 0..n {
        let s1 = pool.acquire(&caps, &imports, None, None).expect("acquire #1");
        let s2 = pool.acquire(&caps, &imports, None, None).expect("acquire #2");
        drop(s1);
        drop(s2);
    }

    let stats = pool.stats();
    let n_u64 = u64::try_from(n).expect("loop count fits u64");
    // Every iteration drops the second session (the first fits under
    // capacity, the second overflows). The released-to-pool counter
    // grows by 1 per iteration; the released-dropped counter grows by
    // 1 per iteration after the very first one (the warm slot from
    // the previous iteration is hit by the next acquire pair).
    assert_eq!(stats.acquired, n_u64 * 2);
    assert!(
        stats.released_to_pool >= n_u64,
        "at least one release per iteration fits the pool",
    );
    assert!(
        stats.released_dropped >= n_u64,
        "at least one release per iteration overflows",
    );
}
