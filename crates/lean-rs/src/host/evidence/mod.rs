//! Kernel-checked evidence and outcome tags.
//!
//! Prompt 15 lands the minimum slice of this module:
//!
//! - [`LeanEvidence`] — opaque handle for a value the Lean kernel
//!   accepted (returned by [`crate::LeanSession::kernel_check`] on the
//!   `Checked` branch).
//! - [`EvidenceStatus`] — the four-way classification
//!   (`Checked / Rejected / Unavailable / Unsupported`) shared between
//!   the prompt-15 `kernel_check` constructor path and the future
//!   prompt-17 re-validation method.
//! - [`LeanKernelOutcome`] — the value type `kernel_check` returns;
//!   carries either [`LeanEvidence`] (on `Checked`) or a
//!   [`crate::host::elaboration::LeanElabFailure`] (on the other three
//!   variants) so callers can both branch on the status tag and read
//!   the diagnostics.
//!
//! Prompt 17 adds `ProofSummary` (Lean-authored display + identifier)
//! and `LeanSession::check_evidence(&LeanEvidence) -> EvidenceStatus`
//! for re-validating handles produced earlier in the session lifetime.

mod handle;
mod status;

pub use self::handle::LeanEvidence;
pub use self::status::{EvidenceStatus, LeanKernelOutcome};
