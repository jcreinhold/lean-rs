//! Compile-fail tests pinning two structural invariants of the curated
//! `lean_rs::*` surface:
//!
//! 1. Semantic handles cannot outlive the `LeanRuntime` borrow that
//!    anchors their `'lean` lifetime.
//! 2. `LeanRuntime`, `LeanSession`, and the semantic handles are
//!    neither [`Send`] nor [`Sync`], so a Lean-derived value cannot
//!    travel to another OS thread.
//!
//! Each negative case is a standalone `.rs` file with a matching
//! `.stderr` snapshot. Regenerate snapshots after a toolchain bump
//! with `TRYBUILD=overwrite cargo test --test compile_fail_surface`.
//!
//! Skipped under `CI=true` because `rustc` emits subtly different
//! diagnostic whitespace between local macOS and the GitHub Actions
//! macOS runner (different on-disk `$RUST/core/src` line wraps),
//! and between Linux and macOS (different note-position heuristics).
//! The invariants the snapshots pin are platform-agnostic; the
//! `PhantomData<*mut ()>` markers in source plus the consumer crates'
//! own `cargo check` are the load-bearing enforcement. Developers run
//! this locally before commit; CI does not.
//!
//! To regenerate after a refactor: `TRYBUILD=overwrite cargo test
//! -p lean-rs-host --test compile_fail_surface`.

#[test]
fn surface_invariants_are_enforced_by_the_type_system() {
    if std::env::var_os("CI").is_some() {
        eprintln!(
            "skipping trybuild snapshot test under CI; run locally with \
             `TRYBUILD=overwrite cargo test -p lean-rs-host --test compile_fail_surface`"
        );
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/handle_outlives_runtime.rs");
    t.compile_fail("tests/compile_fail/runtime_is_not_send_or_sync.rs");
}
