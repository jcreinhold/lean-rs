//! Safe Rust bindings for hosting Lean 4 capabilities.
//!
//! The single safe front door of the `lean-rs` project. Lean owns
//! elaboration, kernel checking, proof objects, universes, `MetaM`, and
//! dependent-type meaning; this crate owns linking, runtime
//! initialization, ABI conversion, module loading, error and panic
//! boundaries, scheduling, diagnostics, batching, and packaging. Raw Lean
//! 4 C ABI symbols enter the workspace via the in-tree `lean-rs-sys`
//! crate; this crate consumes them inside `pub(crate)` modules and never
//! re-exports them.
//!
//! ## Entry point
//!
//! [`LeanRuntime::init`] is the single doorway into the safe surface.
//! Calling it brings the Lean runtime up (idempotently, process-once) and
//! returns a `'static` borrow that anchors the `'lean` lifetime carried by
//! every later handle. Use-before-init is structurally impossible: the
//! constructors of every handle introduced in later prompts require a
//! `&'lean LeanRuntime` (or a value derived from one) as input.
//!
//! ```ignore
//! let runtime = lean_rs::LeanRuntime::init()?;
//! // Hand `runtime` to a host, capability, or session — its `'static`
//! // lifetime coerces to any `'lean` the later API needs.
//! ```
//!
//! Worker threads that did not start inside Lean must be attached for the
//! duration of their Lean work; an RAII attach handle lives in the
//! crate-internal `runtime::thread` module today and is scheduled for
//! public elevation by prompt 24.
//!
//! ## Module map
//!
//! - [`error`] — typed error boundary ([`LeanError`], [`LeanResult`]).
//!   Prompt 06 lands the [`LeanError::Init`] variant; prompt 08 adds
//!   [`LeanError::Conversion`]; prompt 10 fills in the rest.
//! - `runtime` (`pub(crate)`) — process-wide [`LeanRuntime`], thread
//!   attach RAII, and the lifetime-bound owned/borrowed object handles
//!   (`Obj<'lean>`, `ObjRef<'lean, '_>`) that own every `lean_inc` /
//!   `lean_dec` inside the crate.
//! - `abi` (`pub(crate)`) — typed first-order ABI conversions
//!   (`IntoLean`, `TryFromLean`) for scalars, `Nat`/`Int`, `String`, and
//!   `ByteArray`. Infrastructure for the `module` and `host` modules
//!   landing in prompts 09–18.
//! - Other modules — `module`, `host` — land in prompts 09–18.
//!
//! ## Layering
//!
//! `lean-rs-sys → lean-toolchain → lean-rs`. The first two crates expose
//! raw FFI and toolchain metadata; this crate is the only safe surface
//! Rust applications should depend on. Embedders that genuinely need the
//! raw `lean_*` symbols may depend on `lean-rs-sys` directly, accepting
//! its full `unsafe` discipline.

pub(crate) mod abi;
pub mod error;
pub(crate) mod runtime;

pub use crate::error::{LeanError, LeanResult};
pub use crate::runtime::LeanRuntime;

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
