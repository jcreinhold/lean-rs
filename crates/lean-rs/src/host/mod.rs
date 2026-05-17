//! High-level surface for hosting Lean capabilities.
//!
//! The eventual public shape of `host` (per
//! [`docs/architecture/03-host-api.md`](../../../docs/architecture/03-host-api.md))
//! is a four-piece API on top of the [`crate::module`] dispatch primitives:
//!
//! - [`handle`] — opaque, lifetime-bound receipts for semantic Lean values
//!   (`Name`, `Level`, `Expr`, `Declaration`). **Landed.**
//! - `evidence` — opaque kernel-checked evidence and Lean-authored proof
//!   summaries (`LeanEvidence`, `ProofSummary`, `EvidenceStatus`).
//!   *Pending — prompt 17.*
//! - `LeanHost`, `LeanCapabilities`, `LeanSession` — Lake-project entry
//!   point, capability loading, and long-lived session with import +
//!   bulk-query methods. *Pending — prompts 14-16, with bulk methods
//!   following in prompt 20.*
//!
//! Today only [`handle`] is present; the curated re-exports for the rest
//! of the surface arrive with their respective prompts. Callers that need
//! to construct or inspect a handle in the meantime do so through Lean
//! fixture exports reached via [`crate::module::LeanModule::exported`],
//! which already accepts any type implementing [`crate::module::LeanAbi`]
//! as an argument or return.

pub mod handle;
