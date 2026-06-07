//! Standard Lean service layer for Rust applications embedding Lean 4.
//!
//! This crate is the standard service layer built on top of the typed FFI
//! crate shipped by [`lean-rs`](https://docs.rs/lean-rs). It owns:
//!
//! - The high-level [`LeanHost`] / [`LeanCapabilities`] / [`LeanSession`]
//!   trio, plus the [`SessionPool`] / [`PooledSession`] reuse helper and
//!   [`LeanProgressSink`] for live progress from long-running calls.
//! - The host-defined evidence / kernel-outcome / elaboration / meta
//!   value types: [`LeanEvidence`], [`LeanKernelOutcome`],
//!   [`ProofSummary`], [`LeanElabOptions`], [`LeanElabFailure`], the
//!   `meta::*` service surface.
//! - The capability contract: 28 mandatory + 9 optional `lean_rs_host_*`
//!   `@[export]` Lean shims bundled with this crate and loaded alongside the
//!   consumer capability dylib.
//!
//! Downstream applications that only need to call `@[export]` Lean functions
//! with typed arguments and no shim contract
//! should depend on `lean-rs` directly and skip this crate.

#![forbid(unsafe_code)]

pub mod host;

/// Bounded `MetaM` service surface. Reachable only at this sub-module
/// path so callers opt in explicitly via
/// `use lean_rs_host::meta::{...};`.
pub use crate::host::meta;

pub use crate::host::elaboration::{
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX, LEAN_HEARTBEAT_LIMIT_DEFAULT,
    LEAN_HEARTBEAT_LIMIT_MAX, LeanDiagnostic, LeanElabFailure, LeanElabOptions, LeanPosition, LeanSeverity,
};
pub use crate::host::evidence::{
    EvidenceStatus, LEAN_PROOF_SUMMARY_BYTE_LIMIT, LeanEvidence, LeanKernelOutcome, ProofSummary,
};
pub use crate::host::{
    DeclarationFlags, DeclarationInspection, DeclarationInspectionBudgets, DeclarationInspectionFields,
    DeclarationInspectionRequest, DeclarationInspectionResult, DeclarationNameMatch, DeclarationProofSearchFacts,
    DeclarationRenderedInfo, DeclarationSearchBias, DeclarationSearchFacts, DeclarationSearchPruning,
    DeclarationSearchRequest, DeclarationSearchResult, DeclarationSearchRow, DeclarationSearchScope,
    DeclarationSearchTimings, DeclarationVerificationFacts, DeclarationVerificationOutcome,
    DeclarationVerificationRequest, DeclarationVerificationStatus, DeclarationVerificationTarget, LeanCapabilities,
    LeanDeclarationFilter, LeanHost, LeanSession, LeanSourceRange, PoolStats, PooledSession, ProofAttemptEnvelope,
    ProofAttemptOutcome, ProofAttemptRequest, ProofAttemptRow, ProofAttemptStatus, ProofCandidate, ProofEditTarget,
    ProofPositionSelector, ProofPositionSummary, SessionPool, SessionPoolConfig, SessionPoolMemoryPolicy, SessionStats,
};
pub use crate::host::{LeanCancellationToken, LeanProgressEvent, LeanProgressSink};
