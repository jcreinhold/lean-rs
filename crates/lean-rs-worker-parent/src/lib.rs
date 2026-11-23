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

// `deny`, not `forbid`, so the macOS child-CPU sampler can take a narrowly
// scoped exception for the one `proc_pid_rusage` FFI call (see
// `supervisor::child_cpu_millis`); every other module stays unsafe-free.
#![deny(unsafe_code)]

mod capability;
#[cfg(unix)]
mod crash_ownership;
mod pipe;
mod planning;
mod pool;
mod process;
mod session;
mod supervisor;

pub use capability::{
    LeanWorkerBootstrapCheck, LeanWorkerBootstrapDiagnosticCode, LeanWorkerBootstrapReport,
    LeanWorkerBootstrapSeverity, LeanWorkerCapability, LeanWorkerCapabilityBuilder, LeanWorkerChild,
    LeanWorkerHostHandle, LeanWorkerHostHandleBuilder, LeanWorkerModuleCacheLimits,
};
#[cfg(unix)]
pub use crash_ownership::{
    LeanWorkerNativePendingCleanup, LeanWorkerNativeScope, LeanWorkerNativeScopeLease, LeanWorkerNativeSpawnError,
    LeanWorkerOwnedGeneration, LeanWorkerOwnedProcess, LeanWorkerOwnerObservation,
};
pub use planning::{
    LeanWorkerBatchFingerprint, LeanWorkerImportPlanConfig, LeanWorkerImportPlanError, LeanWorkerImportPlanner,
    LeanWorkerModuleWork, LeanWorkerPlanMetadataExpectation, LeanWorkerPlannedBatch,
};
pub use pool::{
    LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerPoolSnapshot, LeanWorkerRestartPolicyClass, LeanWorkerSessionKey,
    LeanWorkerSessionLease,
};
pub use process::{LeanWorkerExitObservation, LeanWorkerProcessTracker};
pub use session::{
    LeanWorkerCancellationToken, LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerDiagnosticEvent,
    LeanWorkerDiagnosticSink, LeanWorkerJsonCommand, LeanWorkerProgressEvent, LeanWorkerProgressSink,
    LeanWorkerRuntimeMetadata, LeanWorkerSession, LeanWorkerSessionConfig, LeanWorkerStreamSummary,
    LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink, LeanWorkerTypedStreamSummary,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[doc(hidden)]
pub use supervisor::{ManagedInspectionOutcome, ManagedWarmInspectionOutcome};
/// Replace a child's environment with a checked snapshot of the parent environment,
/// including the command's explicit additions and removals.
///
/// Use on commands intended to inherit from the parent. This supplies the parent
/// environment even if the command previously called [`std::process::Command::env_clear`].
/// Do not add loader settings after this check. This checks environment variables;
/// it does not verify an executable, its linked images, or its search paths.
///
/// # Errors
/// Returns an error, without changing the command, if the resulting environment
/// contains a `DYLD_*`, `__XPC_DYLD_*`, `LD_*`, or `GLIBC_TUNABLES` variable.
pub fn inherit_checked_native_loader_environment(command: &mut std::process::Command) -> std::io::Result<()> {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => {
                environment.insert(name.to_owned(), value.to_owned());
            }
            None => {
                environment.remove(name);
            }
        }
    }
    for name in environment.keys() {
        let key = name.as_encoded_bytes();
        if key.starts_with(b"DYLD_")
            || key.starts_with(b"LD_")
            || key.starts_with(b"__XPC_DYLD_")
            || key == b"GLIBC_TUNABLES"
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("native loader environment override refused: {}", name.to_string_lossy()),
            ));
        }
    }
    command.env_clear().envs(environment);
    Ok(())
}

pub use supervisor::{
    LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT, LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT,
    LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING, LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT, LeanWorker, LeanWorkerConfig,
    LeanWorkerError, LeanWorkerExit, LeanWorkerLifecycleSnapshot, LeanWorkerPendingRequestStop,
    LeanWorkerRequestCheckpoint, LeanWorkerRequestStopReceipt, LeanWorkerRestartPolicy, LeanWorkerRestartReason,
    LeanWorkerShutdownOutcome, LeanWorkerShutdownReport, LeanWorkerStats, LeanWorkerStatus,
};

// Curated re-exports of the wire types that appear on this crate's public
// API. Callers can depend on `lean-rs-worker-parent` alone for the common
// path; `lean-rs-worker-protocol` remains independently consumable for peers
// that drive the wire format (alternative transports, fuzz harnesses,
// recorders).
#[doc(inline)]
pub use lean_rs_worker_protocol::types::{
    LeanWorkerArtifactReadEpoch, LeanWorkerCapabilityFact, LeanWorkerCapabilityMetadata, LeanWorkerCommandMetadata,
    LeanWorkerDeclarationFilter, LeanWorkerDeclarationFlags, LeanWorkerDeclarationInspection,
    LeanWorkerDeclarationInspectionFields, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationNameMatch, LeanWorkerDeclarationOutlineResult,
    LeanWorkerDeclarationProofSearchFacts, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchBias, LeanWorkerDeclarationSearchFacts, LeanWorkerDeclarationSearchPruning,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationSearchRow, LeanWorkerDeclarationSearchScope,
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
