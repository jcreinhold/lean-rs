//! Safe Rust FFI primitive for embedding Lean 4 from a host application.
//!
//! `lean-rs` is the L1 typed-FFI binding to the Lean 4 runtime — the
//! minimum surface a Rust crate needs to drive a compiled Lean library:
//! bring the runtime up, open a Lake-built dylib, initialise a module,
//! and call typed `@[export]` functions with first-class type
//! marshalling. The opinionated theorem-prover-host stack (`LeanHost`,
//! `LeanCapabilities`, `LeanSession`, plus the evidence and meta
//! surfaces) lives in the sibling
//! [`lean-rs-host`](https://docs.rs/lean-rs-host) crate, with its own
//! 18+4 `lean_rs_host_*` Lean shim contract. This crate has no
//! Lean-side shim contract and ships zero target-language code.
//!
//! ## Happy path (L1)
//!
//! Bring the runtime up once, open the Lake-built dylib, initialise the
//! module, look up a typed export, and call it:
//!
//! ```ignore
//! let runtime = lean_rs::LeanRuntime::init()?;
//! let library = lean_rs::LeanLibrary::open(runtime, env!("MY_CAPABILITY_DYLIB"))?;
//! let module  = library.initialize_module("my_pkg", "MyMod")?;
//! let add     = module.exported::<(u64, u64), u64>("my_export_add")?;
//! let sum     = add.call((3, 4))?;
//! ```
//!
//! [`LeanRuntime::init`] is the single doorway. It brings Lean up
//! (process-once, idempotent) and returns a `'static` borrow that
//! anchors the `'lean` lifetime every later handle carries; use-before-
//! init is structurally impossible.
//!
//! Worker threads that did not start inside Lean must be attached for
//! the duration of their Lean work via [`LeanThreadGuard::attach`];
//! see `docs/architecture/04-concurrency.md` for the contract.
//!
//! ## Module map
//!
//! - [`error`] — typed error boundary. [`LeanError`] is a three-variant
//!   `#[non_exhaustive]` enum (`LeanException` for Lean-thrown `IO`
//!   errors, `Host` for host-stack failures, `Cancelled` for cooperative
//!   host cancellation); payload structs ([`LeanException`],
//!   [`HostFailure`], [`LeanCancelled`]) have private fields so the
//!   bounded-message invariant is structural. Every error projects to a
//!   [`LeanDiagnosticCode`] via `.code()`. The in-process
//!   [`DiagnosticCapture`] RAII guard lets tests assert on `tracing`
//!   events without installing a global subscriber.
//! - [`module`] — load a Lake-built Lean shared object and call typed
//!   exported functions. [`LeanLibrary`] is the RAII dylib handle;
//!   [`LeanModule`] proves a module's initializer succeeded;
//!   [`LeanExported`] is a single generic typed function handle whose
//!   `.call` impl is macro-stamped per arity `0..=12`.
//! - [`handle`] — opaque, lifetime-bound receipts for the four core
//!   Lean semantic values ([`LeanName`], [`LeanLevel`], [`LeanExpr`],
//!   [`LeanDeclaration`]). Construction and inspection happen Lean-side
//!   through [`LeanModule::exported`] against caller-authored shims.
//! - [`callback`] — RAII callback registrations for Lean-to-Rust calls.
//!   [`LeanCallbackHandle`] hides the registry id and trampoline while
//!   still producing the two `USize` ABI values generic interop shims
//!   pass to Lean.
//! - [`runtime`] — process-wide [`LeanRuntime`] anchor,
//!   [`LeanThreadGuard`] attach RAII, and the lifetime-bound owned /
//!   borrowed object handles [`Obj`] / [`ObjRef`].
//! - [`abi`] — sealed [`LeanAbi`] trait + per-Lean-type C-ABI
//!   representation impls. The trait is the bound on
//!   [`LeanExported`]'s argument and return types; the impls are
//!   crate-internal (sealing prevents downstream `LeanAbi for MyType`).
//!
//! ## Layering
//!
//! `lean-rs-sys → lean-toolchain → lean-rs → lean-rs-host`. The first
//! two crates expose raw FFI and toolchain metadata; this crate is the
//! L1 safe surface every (β)-binding consumer depends on. The
//! opinionated theorem-prover-host stack lives in `lean-rs-host`.
//! Embedders that genuinely need the raw `lean_*` symbols can depend
//! on `lean-rs-sys` directly, accepting its full `unsafe` discipline.
//!
//! ## Curation policy
//!
//! Items at `lean_rs::*` are the curated semver surface. The crate
//! root re-exports the typed-FFI primitive plus the four handle types
//! and the L1 error model. Refactors that reshape internal modules
//! are free as long as those re-exports stay stable. The public-
//! surface baseline lives at `docs/api-review/lean-rs-public.txt`.

pub mod abi;
pub mod callback;
pub mod error;
pub mod handle;
pub mod module;
pub mod runtime;

/// **Internal extension point.** Not part of the public API; not covered
/// by semver. Exists so the sibling `lean-rs-host` crate can construct
/// `LeanError` values via the narrow constructor wrappers it
/// uses without bypassing the structural bounding invariant: external
/// callers cannot mint `LeanError` values with unbounded messages. The
/// seam stays narrow on purpose:
/// every extra re-export here is interface surface external readers
/// might mistake for a stable API. Add a wrapper only when a real call
/// site needs it.
///
/// External crates must not depend on anything under this path.
#[doc(hidden)]
pub mod __host_internals {
    pub use crate::error::host_cancelled;
    pub use crate::error::host_module_init;
}

#[cfg(feature = "fuzzing")]
pub mod fuzz_entry;

pub use crate::abi::traits::LeanAbi;
pub use crate::callback::{LeanCallbackEvent, LeanCallbackHandle, LeanCallbackStatus};
pub use crate::error::{
    CapturedEvent, DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY, DiagnosticCapture, HostFailure, HostStage,
    LEAN_ERROR_MESSAGE_LIMIT, LeanCancelled, LeanDiagnosticCode, LeanError, LeanException, LeanExceptionKind,
    LeanResult,
};
pub use crate::handle::{LeanDeclaration, LeanExpr, LeanLevel, LeanName};
pub use crate::module::{DecodeCallResult, LeanArgs, LeanExported, LeanIo, LeanLibrary, LeanModule};
pub use crate::runtime::obj::{Obj, ObjRef};
pub use crate::runtime::{LeanRuntime, LeanThreadGuard};

/// Version of the `lean-rs` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_constant_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
