//! Worker-process boundary for `lean-rs` host workloads.
//!
//! `LeanWorker` is the public process-boundary supervisor. It hides child
//! spawning, pipe management, protocol framing, child exit parsing, and cleanup
//! behind a small lifecycle API. The protocol module is private; callers should
//! not learn frame bytes or restart bookkeeping. `LeanWorkerRestartPolicy`
//! cycles the child process for memory reset; it does not change the
//! in-process `lean-rs-host` memory contract.

mod capability;
mod child;
mod planning;
mod pool;
mod protocol;
mod session;
mod supervisor;
mod types;

pub use capability::{
    LeanWorkerBootstrapCheck, LeanWorkerBootstrapDiagnosticCode, LeanWorkerBootstrapReport,
    LeanWorkerBootstrapSeverity, LeanWorkerCapability, LeanWorkerCapabilityBuilder, LeanWorkerChild,
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
    LeanWorkerError, LeanWorkerExit, LeanWorkerRestartPolicy, LeanWorkerRestartReason, LeanWorkerStats,
    LeanWorkerStatus,
};
pub use types::{
    LeanWorkerCapabilityFact, LeanWorkerCapabilityMetadata, LeanWorkerCommandInfo, LeanWorkerCommandMetadata,
    LeanWorkerDeclarationFilter, LeanWorkerDeclarationRow, LeanWorkerDiagnostic, LeanWorkerDoctorDiagnostic,
    LeanWorkerDoctorReport, LeanWorkerDoctorSeverity, LeanWorkerElabFailure, LeanWorkerElabOptions,
    LeanWorkerElabResult, LeanWorkerKernelResult, LeanWorkerKernelStatus, LeanWorkerKernelSummary,
    LeanWorkerMetaResult, LeanWorkerMetaTransparency, LeanWorkerNameRef, LeanWorkerProcessFileOutcome,
    LeanWorkerProcessModuleOutcome, LeanWorkerProcessedFile, LeanWorkerRendered, LeanWorkerRendering,
    LeanWorkerSourceRange, LeanWorkerTacticInfo, LeanWorkerTermInfo,
};

/// Run the worker child process on stdin/stdout.
///
/// Production applications can expose a tiny app-owned child binary:
///
/// ```ignore
/// fn main() -> std::process::ExitCode {
///     lean_rs_worker::run_worker_child_stdio()
/// }
/// ```
///
/// Parent processes should still use [`LeanWorker`],
/// [`LeanWorkerCapabilityBuilder`], or [`LeanWorkerPool`]. This function is
/// only the child-side binary entry point.
pub fn run_worker_child_stdio() -> std::process::ExitCode {
    child::run_stdio()
}

#[doc(hidden)]
pub fn __run_child_stdio() -> std::process::ExitCode {
    run_worker_child_stdio()
}

#[doc(hidden)]
pub mod __test_support;
