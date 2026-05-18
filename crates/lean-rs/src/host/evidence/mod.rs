//! Kernel-checked evidence and outcome tags.
//!
//! Four types share this module:
//!
//! - [`LeanEvidence`] — opaque handle for a value the Lean kernel
//!   accepted. Returned by [`crate::LeanSession::kernel_check`] on the
//!   `Checked` branch.
//! - [`EvidenceStatus`] — the four-way classification
//!   (`Checked` / `Rejected` / `Unavailable` / `Unsupported`) shared
//!   between the `kernel_check` constructor path and the
//!   [`crate::LeanSession::check_evidence`] re-validation method.
//! - [`LeanKernelOutcome`] — the value type `kernel_check` returns;
//!   carries either [`LeanEvidence`] (on `Checked`) or a
//!   [`crate::host::elaboration::LeanElabFailure`] (on the other three
//!   variants) so callers can both branch on the status tag and read
//!   the diagnostics.
//! - [`ProofSummary`] — bounded-string display projection of a
//!   `LeanEvidence`. Owns no `Obj<'lean>`, so it carries no lifetime and
//!   is freely stashable or cloneable. Produced on demand by
//!   [`crate::LeanSession::summarize_evidence`]; the pretty-print cost
//!   is paid only when callers ask for it, leaving `kernel_check`
//!   cheap.
//!
//! `EvidenceStatus` decodes directly from the Lean-side `EvidenceStatus`
//! inductive (through the crate-internal `TryFromLean` machinery), so it
//! can be returned straight from the re-validation export without an
//! intermediate sum carrier.

mod handle;
mod status;
mod summary;

pub use self::handle::LeanEvidence;
pub use self::status::{EvidenceStatus, LeanKernelOutcome};
pub use self::summary::{LEAN_PROOF_SUMMARY_BYTE_LIMIT, ProofSummary};
