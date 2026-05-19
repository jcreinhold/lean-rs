//! Opinionated Rust host stack for embedding Lean 4 as a theorem-prover
//! capability.
//!
//! This crate is the L2 application framework built on top of the L1
//! FFI primitive shipped by [`lean-rs`](https://docs.rs/lean-rs). It owns:
//!
//! - The high-level [`LeanHost`] / [`LeanCapabilities`] / [`LeanSession`]
//!   trio, plus the [`SessionPool`] / [`PooledSession`] reuse helper.
//! - The host-defined evidence / kernel-outcome / elaboration / meta
//!   value types: [`LeanEvidence`], [`LeanKernelOutcome`],
//!   [`ProofSummary`], [`LeanElabOptions`], [`LeanElabFailure`], the
//!   `meta::*` service surface.
//! - The capability contract: 13 mandatory + 3 optional
//!   `lean_rs_host_*` `@[export]` Lean shims this stack expects in the
//!   capability dylib it loads. Today the shims ship as test scaffolding
//!   only; an external-consumer packaging story (Lake-require vs.
//!   bundled dylib) is the prompt-30 deliverable per `RD-2026-05-18-001`.
//!
//! Downstream applications that want the (β)-binding minimum — call any
//! `@[export]` Lean function with typed arguments, no shim contract —
//! should depend on `lean-rs` directly and skip this crate.

pub mod host;

/// Bounded `MetaM` service surface. Reachable only at this sub-module
/// path (Decision 1, prompt 18) so callers opt in explicitly via
/// `use lean_rs_host::meta::{...};`.
pub mod meta {
    pub use crate::host::meta::*;
}

pub use crate::host::LeanCancellationToken;
pub use crate::host::elaboration::{
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX, LEAN_HEARTBEAT_LIMIT_DEFAULT,
    LEAN_HEARTBEAT_LIMIT_MAX, LeanDiagnostic, LeanElabFailure, LeanElabOptions, LeanPosition, LeanSeverity,
};
pub use crate::host::evidence::{
    EvidenceStatus, LEAN_PROOF_SUMMARY_BYTE_LIMIT, LeanEvidence, LeanKernelOutcome, ProofSummary,
};
pub use crate::host::{LeanCapabilities, LeanHost, LeanSession, PoolStats, PooledSession, SessionPool, SessionStats};
