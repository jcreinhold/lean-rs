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
//! Prompt 17 completes the surface:
//!
//! - [`ProofSummary`] — bounded-string display projection of a
//!   `LeanEvidence`. Owns no `Obj<'lean>`, so it carries no lifetime
//!   and is freely stashable or cloneable.
//! - [`LeanSession::check_evidence`](crate::LeanSession::check_evidence)
//!   re-runs the kernel against a captured evidence handle and
//!   returns a fresh [`EvidenceStatus`].
//! - [`LeanSession::summarize_evidence`](crate::LeanSession::summarize_evidence)
//!   produces a [`ProofSummary`] on demand. The summary is computed
//!   lazily so well-typed `kernel_check` calls do not pay the
//!   pretty-print cost when the caller only inspects the status tag.
//!
//! `EvidenceStatus` now decodes directly from the Lean-side
//! `EvidenceStatus` inductive (through the crate-internal
//! `TryFromLean` machinery), so it can be returned straight from the
//! re-validation export without an intermediate sum carrier.

mod handle;
mod status;
mod summary;

pub use self::handle::LeanEvidence;
pub use self::status::{EvidenceStatus, LeanKernelOutcome};
pub use self::summary::{LEAN_PROOF_SUMMARY_BYTE_LIMIT, ProofSummary};
