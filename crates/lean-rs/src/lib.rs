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
//! - [`error`] — typed error boundary. Per `RD-2026-05-17-006`, the
//!   single public enum [`LeanError`] has two variants:
//!   [`LeanError::LeanException`] for Lean-thrown `IO` errors (the
//!   `kind` is in [`LeanExceptionKind`], the message bounded to
//!   [`LEAN_ERROR_MESSAGE_LIMIT`]) and [`LeanError::Host`] for any
//!   host-stack failure (the `stage` is in [`HostStage`]). Payload
//!   structs ([`LeanException`], [`HostFailure`]) have private fields,
//!   so the bounded-message invariant is structural.
//! - `runtime` (`pub(crate)`) — process-wide [`LeanRuntime`], thread
//!   attach RAII, and the lifetime-bound owned/borrowed object handles
//!   (`Obj<'lean>`, `ObjRef<'lean, '_>`) that own every `lean_inc` /
//!   `lean_dec` inside the crate.
//! - `abi` (`pub(crate)`) — typed first-order ABI conversions
//!   (`IntoLean`, `TryFromLean`) for scalars, `Nat`/`Int`, `String`, and
//!   `ByteArray`. Infrastructure for the `module` and `host` modules
//!   landing in prompts 09–18.
//! - [`module`] — load Lake-built Lean shared objects and initialize
//!   their modules. Surfaces [`module::LeanLibrary`] (RAII handle over
//!   the dylib) and [`module::LeanModule`] (proof that a module's
//!   initializer succeeded). Typed exported-function handles attach to
//!   `LeanModule` in prompt 12.
//! - [`host`] — high-level surface for hosting Lean capabilities.
//!   [`host::handle`] (prompt 13) lands the four opaque semantic handle
//!   types — [`LeanName`], [`LeanLevel`], [`LeanExpr`],
//!   [`LeanDeclaration`]. [`LeanHost`], [`LeanCapabilities`], and
//!   [`LeanSession`] (prompt 14) layer Lake-project entry, capability
//!   loading with pre-resolved symbol caches, and a long-lived session
//!   with `query_declaration` / `list_declarations` / `declaration_type`
//!   / `declaration_kind` / `declaration_name`. Prompt 15 adds
//!   `elaborate` / `kernel_check` and the [`LeanElabOptions`] /
//!   [`LeanElabFailure`] / [`LeanDiagnostic`] / [`LeanSeverity`] /
//!   [`LeanPosition`] diagnostic surface. Prompt 17 adds the
//!   re-validation pair [`LeanSession::check_evidence`] /
//!   [`LeanSession::summarize_evidence`] and the bounded
//!   [`ProofSummary`] projection. Prompt 16's bounded `MetaM` surface
//!   lives at [`host::meta`] — including `LeanMetaOptions`,
//!   `LeanMetaService`, `LeanMetaResponse`, `MetaCallStatus`,
//!   `LeanMetaTransparency`, and the three pinned service constructors
//!   `infer_type` / `whnf` / `heartbeat_burn`. Bulk session methods
//!   land in prompt 20.
//!
//! ## Layering
//!
//! `lean-rs-sys → lean-toolchain → lean-rs`. The first two crates expose
//! raw FFI and toolchain metadata; this crate is the only safe surface
//! Rust applications should depend on. Embedders that genuinely need the
//! raw `lean_*` symbols may depend on `lean-rs-sys` directly, accepting
//! its full `unsafe` discipline.
//!
//! ## Curation policy
//!
//! The crate root names entry points and mandatory session capabilities
//! only. Items at `lean_rs::*` are the curated semver surface; refactors
//! that reshape internal modules are free as long as those re-exports
//! stay stable. Specialized or optional capabilities live at their
//! sub-module path: the bounded `MetaM` surface at [`host::meta`], the
//! typed exported-function loader at [`module`]. Path-shortening
//! re-exports are not added — every name at the crate root must
//! correspond to a happy-path entry point or a type that appears in a
//! crate-root method's signature. The full classification is pinned in
//! `docs/architecture/03-host-api.md`.

pub(crate) mod abi;
pub mod error;
pub mod host;
pub mod module;
pub(crate) mod runtime;

#[cfg(feature = "fuzzing")]
pub mod fuzz_entry;

pub use crate::error::{
    HostFailure, HostStage, LEAN_ERROR_MESSAGE_LIMIT, LeanError, LeanException, LeanExceptionKind, LeanResult,
};
pub use crate::host::elaboration::{
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX, LEAN_HEARTBEAT_LIMIT_DEFAULT,
    LEAN_HEARTBEAT_LIMIT_MAX, LeanDiagnostic, LeanElabFailure, LeanElabOptions, LeanPosition, LeanSeverity,
};
pub use crate::host::evidence::{
    EvidenceStatus, LEAN_PROOF_SUMMARY_BYTE_LIMIT, LeanEvidence, LeanKernelOutcome, ProofSummary,
};
pub use crate::host::handle::{LeanDeclaration, LeanExpr, LeanLevel, LeanName};
pub use crate::host::{LeanCapabilities, LeanHost, LeanSession, PoolStats, PooledSession, SessionPool, SessionStats};
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
