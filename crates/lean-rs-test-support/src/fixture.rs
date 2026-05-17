//! Throwaway fixture-loader for prompt-08 ABI tests.
//!
//! TODO(prompt 12): remove this module. The typed `LeanLibrary` +
//! `LeanModule` + `LeanExported{N}` machinery landed by prompts 11 + 12
//! replaces every consumer of this helper. The replacement is:
//!
//! ```ignore
//! let runtime = lean_rs::LeanRuntime::init()?;
//! let host = lean_rs::LeanHost::from_lake_project(runtime, "fixtures/lean")?;
//! let caps = host.load_capabilities("LeanRsFixture.Scalars")?;
//! let exported = caps.module().exported_1::<u8, u8>("lean_rs_fixture_u8_identity")?;
//! ```
//!
//! Until that lands, prompt-08's `abi::tests` need a way to invoke fixture
//! exports directly. This module provides it through a single
//! [`init_fixture`] entry point that invokes
//! `initialize_lean__rs__fixture_LeanRsFixture` against the linked
//! fixture dylib (linkage emitted by `build.rs`).
//!
//! The caller is responsible for bringing the Lean runtime up first via
//! `lean_rs::LeanRuntime::init` (or, in test-support's own self-tests,
//! the raw `lean_initialize` sequence). We deliberately do **not** depend
//! on `lean-rs` from this crate to avoid the dev-dep diamond (`lean-rs`
//! dev-depends on `lean-rs-test-support`; making `lean-rs-test-support`
//! also depend on `lean-rs` produces "multiple versions of crate `lean_rs`"
//! errors in `cargo test -p lean-rs`).

// SAFETY DOC: every `unsafe { ... }` block carries its own `// SAFETY:`
// comment; this is the only non-`lean-rs` consumer of raw Lean ABI types
// in the workspace.
#![allow(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic)]

use std::sync::OnceLock;

use lean_rs_sys::io::{lean_io_mark_end_initialization, lean_io_result_get_error, lean_io_result_is_ok};
use lean_rs_sys::object::lean_box;
use lean_rs_sys::refcount::lean_dec;
use lean_rs_sys::types::lean_object;

unsafe extern "C" {
    /// Lean-generated top-level initializer for the `LeanRsFixture`
    /// library. The first parameter is the standard `uint8_t builtin`
    /// flag (1 for the runtime path); the second is the IO world token
    /// (`lean_box(0)` — Lean's convention for the world value).
    fn initialize_lean__rs__fixture_LeanRsFixture(builtin: u8, world: *mut lean_object) -> *mut lean_object;
}

/// Initialize the `LeanRsFixture` library, exactly once for the lifetime
/// of the process.
///
/// **Caller obligation:** bring the Lean runtime up first via
/// `lean_rs::LeanRuntime::init` before calling this function. The fixture
/// initializer assumes the runtime is live.
///
/// # Panics
///
/// Panics if the fixture's module initializer returns an `IO.error`.
/// This is a test-only failure mode; the throwaway nature of this helper
/// does not justify a typed return.
pub fn init_fixture() {
    static FIXTURE_INIT: OnceLock<()> = OnceLock::new();
    FIXTURE_INIT.get_or_init(|| {
        // SAFETY: `lean_box(0)` is the conventional encoding for the IO
        // world token used by Lean's generated module initializers. The
        // initializer returns an `IO Unit`-shaped result that we inspect
        // and release.
        unsafe {
            let world = lean_box(0);
            let result = initialize_lean__rs__fixture_LeanRsFixture(1, world);
            if !lean_io_result_is_ok(result) {
                let err = lean_io_result_get_error(result);
                lean_dec(err);
                lean_dec(result);
                panic!("LeanRsFixture initialiser returned IO.error");
            }
            lean_dec(result);
            lean_io_mark_end_initialization();
        }
    });
}
