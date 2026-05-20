//! Worker-process boundary for `lean-rs` host workloads.
//!
//! `LeanWorker` is the public process-boundary supervisor. It hides child
//! spawning, pipe management, protocol framing, child exit parsing, and cleanup
//! behind a small lifecycle API. The protocol module is private; callers should
//! not learn frame bytes or restart bookkeeping. `LeanWorkerRestartPolicy`
//! cycles the child process for memory reset; it does not change the
//! in-process `lean-rs-host` memory contract.

mod child;
mod protocol;
mod session;
mod supervisor;

pub use session::{
    LeanWorkerCancellationToken, LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerDiagnostic, LeanWorkerElabOptions,
    LeanWorkerElabResult, LeanWorkerKernelResult, LeanWorkerKernelStatus, LeanWorkerProgressEvent,
    LeanWorkerProgressSink, LeanWorkerSession, LeanWorkerSessionConfig, LeanWorkerStreamSummary,
};
pub use supervisor::{
    LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerExit, LeanWorkerRestartPolicy, LeanWorkerRestartReason,
    LeanWorkerStats, LeanWorkerStatus,
};

/// Run the prompt-56 child process on stdin/stdout.
///
/// This entry point exists for the `lean-rs-worker-child` binary. It is not
/// the public worker API; prompt 57 will add a supervisor surface over this
/// child runner.
#[doc(hidden)]
pub fn __run_child_stdio() -> std::process::ExitCode {
    child::run_stdio()
}

#[doc(hidden)]
pub mod __test_support;
