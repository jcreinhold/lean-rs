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
//! ## Happy path
//!
//! Bring the runtime up once, open a Lake project, load a capability
//! module, import a Lean library, and query a declaration:
//!
//! ```ignore
//! let runtime  = lean_rs::LeanRuntime::init()?;
//! let host     = lean_rs::LeanHost::from_lake_project(runtime, lake_root)?;
//! let caps     = host.load_capabilities("my_pkg", "MyLib")?;
//! let mut sess = caps.session(&["MyLib.SomeModule"])?;
//! let decl     = sess.query_declaration("MyLib.SomeModule.myDef")?;
//! ```
//!
//! [`LeanRuntime::init`] is the single doorway into the safe surface.
//! Calling it brings the Lean runtime up (idempotently, process-once) and
//! returns a `'static` borrow that anchors the `'lean` lifetime carried by
//! every later handle. Use-before-init is structurally impossible: the
//! constructors of every handle take a `&'lean LeanRuntime` (or a value
//! derived from one) as input.
//!
//! Worker threads that did not start inside Lean must be attached for the
//! duration of their Lean work via [`LeanThreadGuard::attach`]; see
//! `docs/architecture/04-concurrency.md` for the contract.
//!
//! ## Module map
//!
//! - [`error`] — typed error boundary. The single public enum
//!   [`LeanError`] has two variants: [`LeanError::LeanException`] for
//!   Lean-thrown `IO` errors (the `kind` is in [`LeanExceptionKind`], the
//!   message bounded to [`LEAN_ERROR_MESSAGE_LIMIT`]) and
//!   [`LeanError::Host`] for any host-stack failure (the `stage` is in
//!   [`HostStage`]). Payload structs ([`LeanException`], [`HostFailure`])
//!   have private fields, so the bounded-message invariant is structural.
//!   Every error-bearing type also projects to [`LeanDiagnosticCode`] via
//!   `.code()`, and the crate emits structured `tracing` spans against
//!   the `lean_rs` target. The in-process [`DiagnosticCapture`] RAII
//!   guard lets tests assert on emitted events without installing a
//!   global subscriber.
//! - [`module`] — load a Lake-built Lean shared object and call typed
//!   exported functions. [`module::LeanLibrary`] is an RAII handle over
//!   the dylib; [`module::LeanModule`] proves a module's initializer
//!   succeeded; [`module::LeanExported`] is a single generic typed
//!   function handle whose `.call` impl is macro-stamped per arity
//!   `0..=12`. Used directly by embedders that need a Lean export not
//!   already wrapped by [`LeanSession`].
//! - [`host`] — high-level surface for hosting Lean capabilities. The
//!   [`LeanHost`] → [`LeanCapabilities`] → [`LeanSession`] cascade is the
//!   happy path. The session exposes read-only environment queries
//!   ([`LeanSession::query_declaration`], [`LeanSession::list_declarations`],
//!   `declaration_type` / `declaration_kind` / `declaration_name`),
//!   elaboration and kernel checking ([`LeanSession::elaborate`],
//!   [`LeanSession::kernel_check`]) using the typed
//!   [`LeanElabOptions`] / [`LeanElabFailure`] / [`LeanDiagnostic`] /
//!   [`LeanSeverity`] / [`LeanPosition`] diagnostic surface, evidence
//!   re-validation ([`LeanSession::check_evidence`],
//!   [`LeanSession::summarize_evidence`] returning [`EvidenceStatus`] and
//!   [`ProofSummary`]), bulk operations
//!   ([`LeanSession::query_declarations_bulk`],
//!   [`LeanSession::elaborate_bulk`],
//!   [`LeanSession::call_capability`]), and per-session metrics
//!   ([`LeanSession::stats`] returning [`SessionStats`]). The four opaque
//!   semantic handle types ([`LeanName`], [`LeanLevel`], [`LeanExpr`],
//!   [`LeanDeclaration`]) carry the `'lean` lifetime so they cannot
//!   outlive the runtime borrow. [`SessionPool`] + [`PooledSession`]
//!   amortise import cost across calls. The bounded `MetaM` surface
//!   ([`host::meta::LeanMetaService`], [`host::meta::LeanMetaResponse`],
//!   [`host::meta::LeanMetaOptions`], the three pinned service constructors
//!   [`host::meta::infer_type`] / [`host::meta::whnf`] /
//!   [`host::meta::heartbeat_burn`]) lives at [`host::meta`] — see
//!   `docs/architecture/03-host-api.md` for why it is not at the crate
//!   root.
//! - `runtime` (`pub(crate)`) — process-wide [`LeanRuntime`], thread
//!   attach RAII, and the lifetime-bound owned/borrowed object handles
//!   (`Obj<'lean>`, `ObjRef<'lean, '_>`) that own every `lean_inc` /
//!   `lean_dec` inside the crate.
//! - `abi` (`pub(crate)`) — typed first-order ABI conversions
//!   (`IntoLean`, `TryFromLean`) for scalars, `Nat` / `Int`, `String`,
//!   `ByteArray`, `Option`, `Vec`, and the `Except` value type used to
//!   decode `IO`-returning Lean exports.
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
//! `docs/architecture/03-host-api.md`; the frozen public-surface
//! baseline lives at `docs/api-review/lean-rs-public.txt`.

pub(crate) mod abi;
pub mod error;
pub mod host;
pub mod module;
pub(crate) mod runtime;

#[cfg(feature = "fuzzing")]
pub mod fuzz_entry;

pub use crate::error::{
    CapturedEvent, DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY, DiagnosticCapture, HostFailure, HostStage,
    LEAN_ERROR_MESSAGE_LIMIT, LeanDiagnosticCode, LeanError, LeanException, LeanExceptionKind, LeanResult,
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
