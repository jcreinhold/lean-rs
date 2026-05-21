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

pub use capability::{LeanWorkerCapability, LeanWorkerCapabilityBuilder};
pub use planning::{
    LeanWorkerBatchFingerprint, LeanWorkerImportPlanConfig, LeanWorkerImportPlanError, LeanWorkerImportPlanner,
    LeanWorkerModuleWork, LeanWorkerPlanMetadataExpectation, LeanWorkerPlannedBatch,
};
pub use pool::{
    LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerPoolSnapshot, LeanWorkerRestartPolicyClass, LeanWorkerSessionKey,
    LeanWorkerSessionLease,
};
pub use session::{
    LeanWorkerCancellationToken, LeanWorkerCapabilityFact, LeanWorkerCapabilityMetadata, LeanWorkerCommandMetadata,
    LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerDiagnostic, LeanWorkerDiagnosticEvent, LeanWorkerDiagnosticSink,
    LeanWorkerDoctorDiagnostic, LeanWorkerDoctorReport, LeanWorkerDoctorSeverity, LeanWorkerElabOptions,
    LeanWorkerElabResult, LeanWorkerJsonCommand, LeanWorkerKernelResult, LeanWorkerKernelStatus,
    LeanWorkerProgressEvent, LeanWorkerProgressSink, LeanWorkerRuntimeMetadata, LeanWorkerSession,
    LeanWorkerSessionConfig, LeanWorkerStreamSummary, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow,
    LeanWorkerTypedDataSink, LeanWorkerTypedStreamSummary,
};
pub use supervisor::{
    LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT, LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING, LeanWorker, LeanWorkerConfig,
    LeanWorkerError, LeanWorkerExit, LeanWorkerRestartPolicy, LeanWorkerRestartReason, LeanWorkerStats,
    LeanWorkerStatus,
};

/// Run the worker child process on stdin/stdout.
///
/// This entry point exists for the `lean-rs-worker-child` binary. It is not
/// the public worker API; the supervisor (`LeanWorker`) is the consumer
/// surface over this child runner.
#[doc(hidden)]
pub fn __run_child_stdio() -> std::process::ExitCode {
    child::run_stdio()
}

#[doc(hidden)]
pub mod __test_support;
