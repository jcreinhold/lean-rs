//! High-level surface for hosting Lean capabilities.
//!
//! Sits on top of the [`lean_rs::module`] dispatch primitives. The four-piece
//! shape is pinned by `docs/architecture/03-host-stack.md`:
//!
//! - L1 handles ([`lean_rs::handle::LeanName`],
//!   [`lean_rs::handle::LeanLevel`], [`lean_rs::handle::LeanExpr`],
//!   [`lean_rs::handle::LeanDeclaration`]) ship in the FFI primitive
//!   crate `lean-rs`; the session methods here take and return them.
//! - [`LeanHost`], [`LeanCapabilities`], [`LeanSession`] — Lake-project
//!   entry point, capability loading (with pre-resolved session symbol
//!   addresses cached at load time), and a long-lived session that owns
//!   the imported `Lean.Environment` and dispatches every typed query,
//!   elaboration, kernel check, bulk operation, and meta call.
//! - [`elaboration`] — bounded [`elaboration::LeanElabOptions`], typed
//!   [`elaboration::LeanDiagnostic`] / [`elaboration::LeanElabFailure`],
//!   and the published byte / heartbeat ceilings consumed by
//!   [`LeanSession::elaborate`] and [`LeanSession::kernel_check`].
//! - [`evidence`] — opaque [`evidence::LeanEvidence`] kernel-checked
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
//! Construction or inspection of the handle types in [`lean_rs::handle`]
//! outside of a session goes through Lean fixture exports reached via
//! [`lean_rs::module::LeanModule::exported`].

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

pub use self::cancellation::LeanCancellationToken;
pub use self::capabilities::LeanCapabilities;
pub use self::host::LeanHost;
pub use self::pool::{PoolStats, PooledSession, SessionPool};
pub use self::progress::{LeanProgressEvent, LeanProgressSink};
pub use self::session::{LeanDeclarationFilter, LeanSession, LeanSourceRange, SessionStats};

#[cfg(test)]
mod tests;
