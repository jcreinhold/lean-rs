//! Parent-side supervisor for the `lean-rs` worker process boundary.
//!
//! [`LeanWorker`] is the public process-boundary supervisor. It hides child
//! spawning, pipe management, protocol framing, child exit parsing, and
//! cleanup behind a small lifecycle API. Sessions and the host handle builder
//! sit on top of it. The wire types it speaks come from
//! [`lean_rs_worker_protocol`] and are re-exported here, so callers need no
//! second crate dependency for the common path.
//!
//! This crate does not link `libleanshared`. The worker child runtime that
//! does is published separately as [`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child).

// `deny`, not `forbid`, so the macOS child-CPU sampler can take a narrowly
// scoped exception for its `proc_pidinfo` and `mach_timebase_info` calls (see
// `supervisor::resources::child_cpu_millis`); every other module stays
// unsafe-free.
#![deny(unsafe_code)]

mod capability;
mod pipe;
mod process;
mod session;
mod supervisor;

pub use capability::{
    LeanWorkerBootstrapCheck, LeanWorkerBootstrapDiagnosticCode, LeanWorkerBootstrapReport,
    LeanWorkerBootstrapSeverity, LeanWorkerChild, LeanWorkerHostHandle, LeanWorkerHostHandleBuilder,
    LeanWorkerModuleCacheLimits,
};
pub use process::LeanWorkerProcessTracker;
pub use session::{
    LeanWorkerCancellationToken, LeanWorkerProgressEvent, LeanWorkerProgressSink, LeanWorkerRuntimeMetadata,
    LeanWorkerSession, LeanWorkerSessionConfig,
};
pub use supervisor::{
    LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerExit, LeanWorkerLifecycleSnapshot,
    LeanWorkerPendingRequestStop, LeanWorkerRequestCheckpoint, LeanWorkerRequestStopReceipt, LeanWorkerRestartPolicy,
    LeanWorkerRestartReason, LeanWorkerShutdownOutcome, LeanWorkerShutdownReport, LeanWorkerStats, LeanWorkerStatus,
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
    LeanWorkerDeclarationNameMatchKind, LeanWorkerDeclarationOutlineResult, LeanWorkerDeclarationProofSearchFacts,
    LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch, LeanWorkerDeclarationSearchFacts,
    LeanWorkerDeclarationSearchPruning, LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationSearchRow,
    LeanWorkerDeclarationSearchTimings, LeanWorkerDeclarationTargetInfo, LeanWorkerDeclarationTargetResult,
    LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationBatchItem,
    LeanWorkerDeclarationVerificationBatchRequest, LeanWorkerDeclarationVerificationBatchResult,
    LeanWorkerDeclarationVerificationBatchRow, LeanWorkerDeclarationVerificationFacts,
    LeanWorkerDeclarationVerificationRequest, LeanWorkerDeclarationVerificationResult,
    LeanWorkerDeclarationVerificationStatus, LeanWorkerDeclarationVerificationTarget, LeanWorkerDerivedWorkFacts,
    LeanWorkerDiagnostic, LeanWorkerDoctorDiagnostic, LeanWorkerDoctorReport, LeanWorkerDoctorSeverity,
    LeanWorkerElabFailure, LeanWorkerElabOptions, LeanWorkerElabResult, LeanWorkerGoalAtResult,
    LeanWorkerHostRuntimeExpectation, LeanWorkerHostRuntimeMetadata, LeanWorkerKernelResult, LeanWorkerKernelStatus,
    LeanWorkerKernelSummary, LeanWorkerLocalInfo, LeanWorkerMetaResult, LeanWorkerMetaTransparency,
    LeanWorkerModuleCacheStatus, LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchEnvelope,
    LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult,
    LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQueryOutcome, LeanWorkerModuleQueryResult,
    LeanWorkerModuleQuerySelector, LeanWorkerModuleQueryTimings, LeanWorkerModuleSnapshotCacheClearResult,
    LeanWorkerModuleSourceSpan, LeanWorkerNameRef, LeanWorkerOutputBudgets, LeanWorkerProofAttemptEnvelope,
    LeanWorkerProofAttemptRequest, LeanWorkerProofAttemptResult, LeanWorkerProofAttemptRow,
    LeanWorkerProofAttemptStatus, LeanWorkerProofCandidate, LeanWorkerProofEditTarget, LeanWorkerProofPositionSelector,
    LeanWorkerProofPositionSummary, LeanWorkerProofStateInfo, LeanWorkerProofStateResult, LeanWorkerReferencesResult,
    LeanWorkerRendered, LeanWorkerRenderedInfo, LeanWorkerRendering, LeanWorkerSessionImportProfile,
    LeanWorkerSorryPolicy, LeanWorkerSourceCoordinateSpace, LeanWorkerSourceRange,
    LeanWorkerSurroundingDeclarationResult, LeanWorkerTypeAtResult,
};
