//! Compile-fail tests pinning structural invariants of the curated
//! `lean_rs::*` surface that the type markers alone cannot express:
//!
//! 1. `LeanModule::exported_unchecked` is `unsafe fn`—arbitrary dynamic-export
//!    lookup cannot be validated from a symbol name plus caller-chosen
//!    `Args`/`R`, so calling it requires an `unsafe` block.
//! 2. `LeanIo<T>` is an opaque type-level marker whose single field is private,
//!    so downstream code cannot construct a value.
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
//!   cargo test -p lean-rs --test compile_fail_surface
//! ```

#[test]
fn surface_invariants_are_enforced_by_the_type_system() {
    if std::env::var_os("RUN_TRYBUILD").is_none() {
        eprintln!(
            "skipping trybuild snapshot test; set RUN_TRYBUILD=1 to run (the pinned \
             compile-fail CI job does). Regenerate with `TRYBUILD=overwrite RUN_TRYBUILD=1 \
             cargo test -p lean-rs --test compile_fail_surface`"
        );
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/exported_unchecked_requires_unsafe.rs");
    t.compile_fail("tests/compile_fail/lean_io_not_constructible.rs");
}
