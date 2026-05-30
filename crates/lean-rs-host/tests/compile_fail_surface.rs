//! Compile-fail tests pinning structural invariants of the curated
//! `lean_rs::*` / `lean_rs_host::*` surface:
//!
//! 1. Semantic handles cannot outlive the `LeanRuntime` borrow that
//!    anchors their `'lean` lifetime.
//! 2. `LeanRuntime`, `LeanSession`, and the semantic handles are
//!    neither [`Send`] nor [`Sync`], so a Lean-derived value cannot
//!    travel to another OS thread.
//! 3. A `LeanSession` cannot outlive the `LeanCapabilities` borrow it
//!    was opened from, and a `PooledSession` cannot outlive its
//!    `SessionPool`—each handle borrows its parent and is bounded by it.
//!
//! Each negative case is a standalone `.rs` file with a matching
//! `.stderr` snapshot. The snapshots capture `rustc` diagnostics, whose
//! exact wording and whitespace drift across platforms and toolchain
//! versions (different on-disk `$RUST/core/src` line wraps, different
//! note-position heuristics between Linux and macOS), so the suite runs
//! only when `RUN_TRYBUILD=1` is set. The pinned `compile-fail` CI job
//! sets it on a single OS + the repo's pinned stable toolchain, so the
//! snapshots are stable there; the type markers plus consumer `cargo
//! check` remain the enforcement everywhere else. Regenerate after a
//! refactor or toolchain bump with:
//!
//! ```sh
//! TRYBUILD=overwrite RUN_TRYBUILD=1 \
//!   cargo test -p lean-rs-host --test compile_fail_surface
//! ```

#[test]
fn surface_invariants_are_enforced_by_the_type_system() {
    if std::env::var_os("RUN_TRYBUILD").is_none() {
        eprintln!(
            "skipping trybuild snapshot test; set RUN_TRYBUILD=1 to run (the pinned \
             compile-fail CI job does). Regenerate with `TRYBUILD=overwrite RUN_TRYBUILD=1 \
             cargo test -p lean-rs-host --test compile_fail_surface`"
        );
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/handle_outlives_runtime.rs");
    t.compile_fail("tests/compile_fail/runtime_is_not_send_or_sync.rs");
    t.compile_fail("tests/compile_fail/session_outlives_capabilities.rs");
    t.compile_fail("tests/compile_fail/pooled_session_outlives_pool.rs");
}
