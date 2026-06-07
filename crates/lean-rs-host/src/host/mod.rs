//! High-level surface for hosting Lean capabilities.
//!
//! Sits on top of the [`lean_rs::module`] dispatch primitives. The four-piece
//! structure is pinned by `docs/architecture/03-host-stack.md`:
//!
//! - `lean-rs` handles ([`lean_rs::handle::LeanName`],
//!   [`lean_rs::handle::LeanLevel`], [`lean_rs::handle::LeanExpr`],
//!   [`lean_rs::handle::LeanDeclaration`]) ship in the FFI primitive
//!   crate `lean-rs`; the session methods here take and return them.
//! - [`LeanHost`], [`LeanCapabilities`], [`LeanSession`]—Lake-project
//!   entry point, capability loading (either user-dylib-backed or
//!   shims-only, with manifest-checked host-shim bindings resolved when a
//!   session is constructed), and a long-lived session that owns the imported
//!   `Lean.Environment` and dispatches every typed query, elaboration,
//!   kernel check, bulk operation, and meta call.
//! - [`elaboration`]—bounded [`elaboration::LeanElabOptions`], typed
//!   [`elaboration::LeanDiagnostic`] / [`elaboration::LeanElabFailure`],
//!   and the published byte / heartbeat ceilings consumed by
//!   [`LeanSession::elaborate`] and [`LeanSession::kernel_check`].
//! - [`evidence`]—opaque [`evidence::LeanEvidence`] kernel-checked
//!   evidence handle plus the [`evidence::EvidenceStatus`] /
//!   [`evidence::LeanKernelOutcome`] taxonomy returned by
//!   [`LeanSession::kernel_check`], and the bounded
//!   [`evidence::ProofSummary`] projection returned by
//!   [`LeanSession::summarize_evidence`].
//!
//! Two further pieces sit alongside but stay at sub-module paths:
//! [`meta`] for the optional bounded `MetaM` capability (only
//! [`LeanSession::run_meta`] touches it), and [`pool`] for the
//! capacity-bounded [`pool::SessionPool`] / [`pool::PooledSession`]
//! reuse helper.
//!
//! ## Cascade
//!
//! ```ignore
//! let runtime  = lean_rs::LeanRuntime::init()?;
//! let host     = lean_rs::LeanHost::from_lake_project(runtime, lake_root)?;
//! let caps     = host.load_capabilities("my_pkg", "MyLib")?;
//! let mut sess = caps.session(&["MyLib.SomeModule"], None, None)?;
//! let decl     = sess.query_declaration("MyLib.SomeModule.myDef", None)?;
//! ```
//!
//! Use [`LeanHost::load_shims_only`] instead of
//! [`LeanHost::load_capabilities`] when the host only needs the bundled
//! session services and will not call ad-hoc user `@[export]` symbols.
//!
//! Construction or inspection of the handle types in [`lean_rs::handle`]
//! outside of a session belongs to the lower-level `lean-rs` crate.

pub mod declaration_search;
pub mod elaboration;
pub mod evidence;
pub mod meta;
pub mod pool;
pub mod process;

pub(crate) mod lake;

mod cancellation;
mod capabilities;
#[allow(
    clippy::module_inception,
    reason = "the LeanHost type is the natural name for this file"
)]
mod host;
mod progress;
mod session;
mod shim_bindings;

pub use self::cancellation::LeanCancellationToken;
pub use self::capabilities::LeanCapabilities;
pub use self::declaration_search::{
    DeclarationFlags, DeclarationInspection, DeclarationInspectionBudgets, DeclarationInspectionFields,
    DeclarationInspectionRequest, DeclarationInspectionResult, DeclarationNameMatch, DeclarationProofSearchFacts,
    DeclarationRenderedInfo, DeclarationSearchBias, DeclarationSearchFacts, DeclarationSearchPruning,
    DeclarationSearchRequest, DeclarationSearchResult, DeclarationSearchRow, DeclarationSearchScope,
    DeclarationSearchTimings,
};
pub use self::host::LeanHost;
pub use self::pool::{PoolStats, PooledSession, SessionPool, SessionPoolConfig, SessionPoolMemoryPolicy};
pub use self::process::{
    DeclarationVerificationFacts, DeclarationVerificationOutcome, DeclarationVerificationRequest,
    DeclarationVerificationStatus, DeclarationVerificationTarget, ProofAttemptEnvelope, ProofAttemptOutcome,
    ProofAttemptRequest, ProofAttemptRow, ProofAttemptStatus, ProofCandidate, ProofEditTarget, ProofPositionSelector,
    ProofPositionSummary,
};
pub use self::progress::{LeanProgressEvent, LeanProgressSink};
pub use self::session::{LeanDeclarationFilter, LeanSession, LeanSourceRange, SessionStats};

#[cfg(test)]
mod tests;
