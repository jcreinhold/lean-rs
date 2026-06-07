//! Parent-side supervisor for the `lean-rs` worker process boundary.
//!
//! [`LeanWorker`] is the public process-boundary supervisor. It hides child
//! spawning, pipe management, protocol framing, child exit parsing, and
//! cleanup behind a small lifecycle API. Sessions, capability builders, and
//! the local worker pool sit on top of it. The wire types it speaks come
//! from [`lean_rs_worker_protocol`] and are re-exported on this crate's
//! public surface so callers do not need a second crate dependency for the
//! common path.
//!
//! This crate does not link `libleanshared`. The worker child runtime that
//! does is published separately as [`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child).

#![forbid(unsafe_code)]

mod capability;
mod planning;
mod pool;
mod session;
mod supervisor;

pub use capability::{
    LeanWorkerBootstrapCheck, LeanWorkerBootstrapDiagnosticCode, LeanWorkerBootstrapReport,
    LeanWorkerBootstrapSeverity, LeanWorkerCapability, LeanWorkerCapabilityBuilder, LeanWorkerChild,
    LeanWorkerHostHandle, LeanWorkerHostHandleBuilder, LeanWorkerModuleCacheLimits,
};
pub use planning::{
    LeanWorkerBatchFingerprint, LeanWorkerImportPlanConfig, LeanWorkerImportPlanError, LeanWorkerImportPlanner,
    LeanWorkerModuleWork, LeanWorkerPlanMetadataExpectation, LeanWorkerPlannedBatch,
};
pub use pool::{
    LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerPoolSnapshot, LeanWorkerRestartPolicyClass, LeanWorkerSessionKey,
    LeanWorkerSessionLease,
};
pub use session::{
    LeanWorkerCancellationToken, LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerDiagnosticEvent,
    LeanWorkerDiagnosticSink, LeanWorkerJsonCommand, LeanWorkerProgressEvent, LeanWorkerProgressSink,
    LeanWorkerRuntimeMetadata, LeanWorkerSession, LeanWorkerSessionConfig, LeanWorkerStreamSummary,
    LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink, LeanWorkerTypedStreamSummary,
};
pub use supervisor::{
    LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT, LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING, LeanWorker, LeanWorkerConfig,
    LeanWorkerError, LeanWorkerExit, LeanWorkerLifecycleSnapshot, LeanWorkerRestartPolicy, LeanWorkerRestartReason,
    LeanWorkerStats, LeanWorkerStatus,
};

// Curated re-exports of the wire types that appear on this crate's public
// API. Callers can depend on `lean-rs-worker-parent` alone for the common
// path; `lean-rs-worker-protocol` remains independently consumable for peers
// that drive the wire format (alternative transports, fuzz harnesses,
// recorders).
#[doc(inline)]
pub use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityFact, LeanWorkerCapabilityMetadata, LeanWorkerCommandMetadata, LeanWorkerDeclarationFilter,
    LeanWorkerDeclarationFlags, LeanWorkerDeclarationInspection, LeanWorkerDeclarationInspectionFields,
    LeanWorkerDeclarationInspectionRequest, LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationNameMatch,
    LeanWorkerDeclarationProofSearchFacts, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchBias, LeanWorkerDeclarationSearchFacts, LeanWorkerDeclarationSearchPruning,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationSearchRow, LeanWorkerDeclarationSearchScope,
    LeanWorkerDeclarationSearchTimings, LeanWorkerDeclarationTargetInfo, LeanWorkerDeclarationTargetResult,
    LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationFacts, LeanWorkerDeclarationVerificationRequest,
    LeanWorkerDeclarationVerificationResult, LeanWorkerDeclarationVerificationStatus,
    LeanWorkerDeclarationVerificationTarget, LeanWorkerDiagnostic, LeanWorkerDoctorDiagnostic, LeanWorkerDoctorReport,
    LeanWorkerDoctorSeverity, LeanWorkerElabFailure, LeanWorkerElabOptions, LeanWorkerElabResult,
    LeanWorkerGoalAtResult, LeanWorkerKernelResult, LeanWorkerKernelStatus, LeanWorkerKernelSummary,
    LeanWorkerLocalInfo, LeanWorkerMetaResult, LeanWorkerMetaTransparency, LeanWorkerModuleCacheStatus,
    LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchEnvelope, LeanWorkerModuleQueryBatchItem,
    LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult, LeanWorkerModuleQueryCacheFacts,
    LeanWorkerModuleQueryOutcome, LeanWorkerModuleQueryResult, LeanWorkerModuleQuerySelector,
    LeanWorkerModuleQueryTimings, LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerModuleSourceSpan,
    LeanWorkerNameRef, LeanWorkerOutputBudgets, LeanWorkerProofAttemptEnvelope, LeanWorkerProofAttemptRequest,
    LeanWorkerProofAttemptResult, LeanWorkerProofAttemptRow, LeanWorkerProofAttemptStatus, LeanWorkerProofCandidate,
    LeanWorkerProofEditTarget, LeanWorkerProofPositionSelector, LeanWorkerProofPositionSummary,
    LeanWorkerProofStateInfo, LeanWorkerProofStateResult, LeanWorkerReferencesResult, LeanWorkerRendered,
    LeanWorkerRenderedInfo, LeanWorkerRendering, LeanWorkerSessionImportProfile, LeanWorkerSorryPolicy,
    LeanWorkerSourceRange, LeanWorkerSurroundingDeclarationResult, LeanWorkerTypeAtResult,
};
