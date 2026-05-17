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

#[test]
fn surface_invariants_are_enforced_by_the_type_system() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/handle_outlives_runtime.rs");
    t.compile_fail("tests/compile_fail/runtime_is_not_send_or_sync.rs");
}
