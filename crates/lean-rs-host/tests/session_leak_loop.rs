//! Session create/drop loops.
//!
//! The Rust side has no oracle for "did `Drop` release everything Lean
//! allocated", so these tests repeat a session lifecycle until a
//! per-iteration leak would grow resident memory or show up under a
//! sanitizer run by hand. Routine runs use a small iteration count; the
//! long variant needs `--include-ignored` and
//! `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1`. The CI `AddressSanitizer` job does
//! not run in-process Lean tests (see `.github/workflows/sanitizer.yml`).
//!
//! The `import_stats` assertions are not leak detection: they pin the
//! import bookkeeping, so a refactor cannot change what one import
//! reports without a test failing.

#![allow(clippy::expect_used)]

#[path = "support/fixed_fixture.rs"]
mod fixture;
use std::path::PathBuf;

use lean_rs::LeanRuntime;
use lean_rs_host::{LeanCapabilities, LeanHost, LeanSession, LeanSessionImportProfile};

fn fixture_lake_root() -> PathBuf {
    fixture::native_fixture_root()
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
}

fn session_over_handles<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&fixture::native_imports(caps, "LeanRsFixture.Handles"), None, None)
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

fn env_value_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"))
}

fn import_heavy_diagnostics_enabled() -> bool {
    if env_value_truthy("LEAN_RS_RUN_IMPORT_HEAVY_TESTS") {
        return true;
    }
    eprintln!("skipping import-heavy diagnostic loop; set LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1 to run it");
    false
}

#[test]
fn session_create_drop_loop_small() {
    // Eight session lifecycles is enough to exercise the import +
    // Environment drop path on every supported platform under the
    // normal test budget. Each create/drop is independent—sessions do
    // not share state on the Rust side—so a leak grows linearly with
    // the iteration count.
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");

    for _ in 0..iters(8) {
        let session = session_over_handles(&caps);
        drop(session);
    }
}

#[test]
fn session_leak_loop_import_stats_reports_fixture_shape() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
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
    assert_eq!(
        stats.imported_bytes, stats.compacted_region_bytes,
        "imported bytes remains a compatibility alias for compacted-region bytes"
    );
    assert_eq!(
        stats.compacted_region_bytes,
        stats
            .memory_mapped_region_bytes
            .saturating_add(stats.non_memory_mapped_region_bytes),
        "compacted-region bytes should split into mmap and non-mmap totals"
    );
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
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let session = caps
        .session_with_profile(
            &fixture::native_imports_for(
                &caps,
                &["LeanRsFixture.Handles"],
                LeanSessionImportProfile::FullPrivateCompat,
            ),
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

/// Long session lifecycle loop: an explicit leak diagnostic, run by hand
/// with `--include-ignored` and `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1`.
#[test]
#[ignore = "long-running leak diagnostic; also needs LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1"]
fn session_create_drop_loop_long() {
    if !import_heavy_diagnostics_enabled() {
        return;
    }

    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");

    let n = iters(512);
    for _ in 0..n {
        let session = session_over_handles(&caps);
        drop(session);
    }
}
