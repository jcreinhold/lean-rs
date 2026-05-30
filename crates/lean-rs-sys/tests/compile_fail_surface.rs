//! Compile-fail tests pinning the published-opacity guarantees of
//! `lean-rs-sys`: downstream code can hold a `lean_object` only by pointer and
//! cannot name the crate-private layout mirror. These back the safety-model
//! claim (`docs/architecture/01-safety-model.md`) that object state is reached
//! exclusively through the `pub unsafe fn` helpers.
//!
//! Each negative case is a standalone `.rs` file with a matching `.stderr`
//! snapshot. The snapshots capture `rustc` diagnostics, whose exact wording
//! and whitespace drift across platforms and toolchain versions, so the suite
//! runs only when `RUN_TRYBUILD=1` is set—the pinned `compile-fail` CI job
//! sets it on a single OS + the repo's pinned stable toolchain. Locally,
//! regenerate after a refactor or toolchain bump with:
//!
//! ```sh
//! TRYBUILD=overwrite RUN_TRYBUILD=1 \
//!   cargo test -p lean-rs-sys --test compile_fail_surface
//! ```

#[test]
fn opacity_invariants_are_enforced_by_the_type_system() {
    if std::env::var_os("RUN_TRYBUILD").is_none() {
        eprintln!(
            "skipping trybuild snapshot test; set RUN_TRYBUILD=1 to run (the pinned \
             compile-fail CI job does). Regenerate with `TRYBUILD=overwrite RUN_TRYBUILD=1 \
             cargo test -p lean-rs-sys --test compile_fail_surface`"
        );
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/lean_object_not_constructible.rs");
    t.compile_fail("tests/compile_fail/lean_object_repr_not_nameable.rs");
}
