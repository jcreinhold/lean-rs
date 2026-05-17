//! High-level surface for hosting Lean capabilities.
//!
//! The eventual public shape of `host` (per
//! [`docs/architecture/03-host-api.md`](../../../docs/architecture/03-host-api.md))
//! is a four-piece API on top of the [`crate::module`] dispatch primitives:
//!
//! - [`handle`] — opaque, lifetime-bound receipts for semantic Lean values
//!   (`Name`, `Level`, `Expr`, `Declaration`). **Landed (prompt 13).**
//! - [`LeanHost`], [`LeanCapabilities`], [`LeanSession`] — Lake-project
//!   entry point, capability loading (with pre-resolved session symbol
//!   addresses), and a long-lived session with import + environment-query
//!   methods. **Landed (prompt 14).** Bulk methods on `LeanSession`
//!   follow in prompt 20.
//! - [`elaboration`] — typed options, diagnostics, and the
//!   `LeanSession::elaborate` / `kernel_check` methods (prompt 15).
//! - [`evidence`] — opaque kernel-checked evidence handle plus the
//!   `EvidenceStatus` / `LeanKernelOutcome` taxonomy returned by
//!   `kernel_check`. The prompt-17 expansion adds `ProofSummary` and a
//!   re-validation method.
//!
//! ## Cascade
//!
//! ```ignore
//! let runtime  = lean_rs::LeanRuntime::init()?;
//! let host     = lean_rs::LeanHost::from_lake_project(runtime, lake_root)?;
//! let caps     = host.load_capabilities("my_pkg", "MyLib")?;
//! let mut sess = caps.session(&["MyLib.SomeModule"])?;
//! let decl     = sess.query_declaration("MyLib.SomeModule.myDef")?;
//! ```
//!
//! Construction or inspection of the handle types in [`handle`] still
//! goes through Lean fixture exports reached via
//! [`crate::module::LeanModule::exported`] when needed outside of a
//! session.

pub mod elaboration;
pub mod evidence;
pub mod handle;
pub mod meta;

pub(crate) mod lake;

mod capabilities;
#[allow(
    clippy::module_inception,
    reason = "the LeanHost type is the natural name for this file"
)]
mod host;
mod session;

pub use self::capabilities::LeanCapabilities;
pub use self::host::LeanHost;
pub use self::session::LeanSession;

#[cfg(test)]
mod tests;
