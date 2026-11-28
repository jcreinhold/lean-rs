//! The parent-side supervisor of one worker child.
//!
//! [`LeanWorker`] runs one child process at a time. This module holds the
//! supervisor's state and its small accessors: status, statistics, the
//! lifecycle snapshot, stop receipts, and the request timeout. The submodules
//! hold the rest:
//!
//! - `config`: [`LeanWorkerConfig`] and the default deadlines.
//! - `startup`: spawning a child, its handshake, and its stderr tail.
//! - `request`: writing a request, reading the child's replies, and one typed
//!   method per request kind.
//! - `restart`: the restart policy, what the current child has used, and
//!   replacing a child.
//! - `shutdown`: stopping, killing, and reaping the child.
//! - `resources`: samples of the child's RSS and CPU time.
//! - `error`: [`LeanWorkerError`] and the bounded text it displays.

mod config;
mod error;
mod request;
mod resources;
mod restart;
mod shutdown;
mod startup;
#[cfg(all(test, unix))]
mod terminal_frame_tests;
#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]
mod tests;

pub(crate) use config::LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING;
pub use config::LeanWorkerConfig;
pub use error::LeanWorkerError;
pub use restart::{LeanWorkerRestartPolicy, LeanWorkerRestartReason};
pub use shutdown::{LeanWorkerShutdownOutcome, LeanWorkerShutdownReport};

use std::collections::VecDeque;
use std::io::{BufReader, BufWriter};
use std::process::{ChildStdin, ChildStdout, ExitStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::types::{LeanWorkerHostRuntimeMetadata, LeanWorkerImportStats};

use crate::pipe::Pipe;
use crate::process::{LeanWorkerExitObservation, OwnedChild};
use crate::session::{LeanWorkerRuntimeMetadata, LeanWorkerSessionConfig};
use restart::GenerationBudget;
use startup::{StartupChild, StderrCapture};

/// Timing facts for the most recent synchronous worker replacement.
///
/// These are observability facts only. A replacement remains a process cycle
/// hidden behind supervisor policy; callers do not receive child
/// identities or lifecycle handles.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanWorkerReplacementTiming {
    pub spawn_handshake: Duration,
    pub session_open_import: Duration,
    pub replacement_total: Duration,
    pub replacement_reason: String,
    pub replacement_budget_status: String,
}

/// Snapshot of worker lifecycle counters.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanWorkerStats {
    /// Requests that entered a worker child.
    pub requests: u64,
    /// Import-like requests that entered a worker child.
    pub imports: u64,
    /// Import-like requests that reached the supervisor admission gate.
    pub import_like_admission_attempts: u64,
    /// Import-like requests admitted past parent-side restart/RSS policy.
    pub import_like_admitted: u64,
    /// Last sampled child RSS before an import-like request was admitted, when available.
    pub last_import_like_rss_before_admission_kib: Option<u64>,
    /// Child exits observed by the supervisor, including policy cycles.
    pub exits: u64,
    /// Restarts performed by the supervisor, planned and abnormal.
    pub restarts: u64,
    /// Restarts whose reason is abnormal; see [`LeanWorkerRestartReason::is_planned`].
    pub abnormal_restarts: u64,
    /// Explicit process cycles.
    pub explicit_cycles: u64,
    /// Restarts caused by `LeanWorkerRestartPolicy::max_requests`.
    pub max_request_restarts: u64,
    /// Restarts caused by `LeanWorkerRestartPolicy::max_imports`.
    pub max_import_restarts: u64,
    /// Restarts caused by `LeanWorkerRestartPolicy::max_import_residue_bytes`.
    pub import_residue_restarts: u64,
    /// Confirmed imports whose reported region bytes could not be believed.
    ///
    /// The child said it imported modules but attributed no compacted-region
    /// bytes to them. Residue accounting silently under-counts for exactly that
    /// many imports, which is the difference between the byte bound doing its job
    /// and the `max_imports` backstop being the only thing left. A non-zero value
    /// here is the signal to look at the shim, not at the budget.
    pub import_stats_unusable: u64,
    /// Restarts caused by `LeanWorkerRestartPolicy::max_rss_kib`.
    pub rss_restarts: u64,
    /// Restarts caused by `LeanWorkerRestartPolicy::idle_restart_after`.
    pub idle_restarts: u64,
    /// Restarts caused by parent-side cancellation of an in-flight request.
    pub cancelled_restarts: u64,
    /// Restarts caused by parent-side request timeouts.
    pub timeout_restarts: u64,
    /// RSS checks skipped because the platform did not provide a usable sample.
    pub rss_samples_unavailable: u64,
    /// Last measured child RSS in KiB, when a policy check could sample it.
    pub last_rss_kib: Option<u64>,
    /// Most recent restart reason, if any.
    pub last_restart_reason: Option<LeanWorkerRestartReason>,
    /// Worker replacement attempts performed synchronously by this supervisor.
    pub replacement_attempts: u64,
    /// Successful worker replacements.
    pub replacement_successes: u64,
    /// Failed worker replacements.
    pub replacement_failures: u64,
    /// Replacements admitted by the configured policy without overlapping children.
    pub replacement_budget_admitted: u64,
    /// Replacement attempts skipped by a budget guard.
    pub replacement_budget_skipped: u64,
    /// Most recent replacement timing facts, if a replacement has succeeded.
    pub last_replacement_timing: Option<LeanWorkerReplacementTiming>,
    /// Most recent skipped replacement reason, if any.
    pub last_replacement_skipped_reason: Option<String>,
    /// Most recent worker spawn and protocol-handshake elapsed time.
    pub last_spawn_handshake_elapsed: Option<Duration>,
    /// Most recent host-session open/import elapsed time.
    pub last_session_open_import_elapsed: Option<Duration>,
    /// Lean-native import attribution for the most recent opened host session, if any.
    pub last_import_stats: Option<LeanWorkerImportStats>,
    /// Streaming requests that reached terminal success.
    pub stream_successes: u64,
    /// Streaming requests that failed after entering the child.
    pub stream_failures: u64,
    /// Data rows delivered to parent-side sinks.
    pub data_rows_delivered: u64,
    /// Raw row payload bytes delivered to parent-side sinks.
    pub data_row_payload_bytes: u64,
    /// Total elapsed time spent in streaming requests.
    pub stream_elapsed: Duration,
    /// Times the bounded worker-event reader had to wait for the parent to drain events.
    pub backpressure_waits: u64,
    /// Streaming requests that failed after bounded-buffer backpressure was observed.
    pub backpressure_failures: u64,
}

/// Compact lifecycle facts for callers that supervise a worker boundary.
///
/// `LeanWorkerStats` remains the detailed counter set. This snapshot is the
/// stable, policy-facing view: a caller can compare two snapshots to observe
/// every restart performed inside the supervisor, including restarts caused by
/// timeout, cancellation, RSS limits, child failures, import/request cycling,
/// or explicit cycles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerLifecycleSnapshot {
    /// Monotone generation number for the current child. It equals the total
    /// restart count observed by this supervisor, and the generation that
    /// [`LeanWorkerRequestStopReceipt::worker_generation`] reports.
    pub worker_generation: u64,
    /// Total restarts performed by the supervisor.
    pub restarts: u64,
    /// Restarts whose reason is abnormal; see [`LeanWorkerRestartReason::is_planned`].
    pub abnormal_restarts: u64,
    /// Abnormal restarts inside the current restart window. When this reaches
    /// the configured limit, the next abnormal restart is refused.
    pub abnormal_restarts_in_window: u64,
    /// How long the worker stays down because abnormal restarts used up the
    /// window, or `None` when a restart is allowed now. Once it passes, the
    /// next request starts a new child.
    pub restart_retry_after: Option<Duration>,
    /// Child exits observed by the supervisor, including policy cycles.
    pub exits: u64,
    /// Most recent restart reason, if any.
    pub last_restart_reason: Option<LeanWorkerRestartReason>,
    /// Most recent child exit observed by the supervisor, if any.
    pub last_exit: Option<LeanWorkerExit>,
    /// Last measured child RSS in KiB, when available.
    pub last_rss_kib: Option<u64>,
    /// RSS checks skipped because the platform did not provide a usable sample.
    pub rss_samples_unavailable: u64,
    /// Unreclaimable bytes retained by imports in the *current* child.
    ///
    /// Resets to zero on every restart, unlike everything else on this snapshot.
    /// Compare it against `import_residue_limit_bytes` to see how close the
    /// current generation is to its planned cycle — which is what an idle-time
    /// cycling policy needs and cannot get from the lifetime counters.
    pub import_residue_bytes: u64,
    /// The configured residue limit, when one is set.
    pub import_residue_limit_bytes: Option<u64>,
}

impl LeanWorkerLifecycleSnapshot {
    fn from_worker(
        stats: &LeanWorkerStats,
        last_exit: Option<LeanWorkerExit>,
        import_residue_bytes: u64,
        import_residue_limit_bytes: Option<u64>,
        abnormal_restarts_in_window: u64,
        restart_retry_after: Option<Duration>,
    ) -> Self {
        Self {
            worker_generation: stats.restarts,
            restarts: stats.restarts,
            abnormal_restarts: stats.abnormal_restarts,
            abnormal_restarts_in_window,
            restart_retry_after,
            exits: stats.exits,
            last_restart_reason: stats.last_restart_reason.clone(),
            last_exit,
            last_rss_kib: stats.last_rss_kib,
            rss_samples_unavailable: stats.rss_samples_unavailable,
            import_residue_bytes,
            import_residue_limit_bytes,
        }
    }
}

impl LeanWorkerStats {
    fn record_restart(&mut self, reason: LeanWorkerRestartReason) {
        self.restarts = self.restarts.saturating_add(1);
        if !reason.is_planned() {
            self.abnormal_restarts = self.abnormal_restarts.saturating_add(1);
        }
        match &reason {
            LeanWorkerRestartReason::Explicit => {
                self.explicit_cycles = self.explicit_cycles.saturating_add(1);
            }
            LeanWorkerRestartReason::MaxRequests { .. } => {
                self.max_request_restarts = self.max_request_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::MaxImports { .. } => {
                self.max_import_restarts = self.max_import_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::ImportResidue { .. } | LeanWorkerRestartReason::ImportAccountingUnknown => {
                self.import_residue_restarts = self.import_residue_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::RssCeiling { .. } => {
                self.rss_restarts = self.rss_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::RssHardLimit { .. } => {
                self.rss_restarts = self.rss_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::Idle { .. } => {
                self.idle_restarts = self.idle_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::Cancelled { .. } => {
                self.cancelled_restarts = self.cancelled_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::RequestTimeout { .. } => {
                self.timeout_restarts = self.timeout_restarts.saturating_add(1);
            }
            // `abnormal_restarts` and `last_restart_reason` already record
            // these; no caller needs a counter per kind of failure.
            LeanWorkerRestartReason::ChildAbort { .. }
            | LeanWorkerRestartReason::ChildExit { .. }
            | LeanWorkerRestartReason::SessionMissing { .. }
            | LeanWorkerRestartReason::RequestFailed { .. }
            | LeanWorkerRestartReason::StartFailed { .. } => {}
        }
        self.last_restart_reason = Some(reason);
    }
}

/// Public lifecycle state for a worker child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeanWorkerStatus {
    /// The worker process is still running.
    Running,
    /// The worker process has exited.
    Exited(LeanWorkerExit),
}

/// Rendered child-process exit information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerExit {
    /// Whether the child process exited successfully.
    pub success: bool,
    /// The platform exit code when one is available.
    pub code: Option<i32>,
    /// The platform-rendered process status.
    pub status: String,
    /// Captured child diagnostics, if available.
    pub diagnostics: String,
}

impl LeanWorkerExit {
    fn from_status(status: ExitStatus, diagnostics: String) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
            status: status.to_string(),
            diagnostics,
        }
    }
}

/// An opaque checkpoint taken before a synchronous worker operation.
///
/// Checkpoints belong to one supervisor instance and remain valid across its
/// replacements. They are in-process correlation tokens, not durable process
/// identities or permission to signal a PID.
#[derive(Clone, Debug)]
pub struct LeanWorkerRequestCheckpoint {
    supervisor: Arc<()>,
    next_request_id: WorkerRequestId,
}

/// Confirmed exit of the direct child that handled one worker request.
///
/// This is evidence that the named request can no longer execute in that child.
/// It does not prove a successful result, absence of earlier side effects, or
/// termination of descendants or a subsequently spawned replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerRequestStopReceipt {
    request: InFlightRequest,
    exit: LeanWorkerExit,
}

impl LeanWorkerRequestStopReceipt {
    /// Monotone request identifier within the checkpoint's supervisor instance.
    #[must_use]
    pub fn request_id(&self) -> u64 {
        self.request.id.0
    }

    /// Generation of the child whose exit was confirmed.
    #[must_use]
    pub fn worker_generation(&self) -> u64 {
        self.request.generation.0
    }

    /// Operation being sent or awaited when this request was recorded.
    #[must_use]
    pub fn operation(&self) -> &'static str {
        self.request.operation
    }

    /// PID of the owned, reaped direct child; diagnostic identity only.
    #[must_use]
    pub fn process_id(&self) -> u32 {
        self.request.process_id
    }

    /// Exit status obtained from the owned child's `try_wait`.
    #[must_use]
    pub fn exit(&self) -> &LeanWorkerExit {
        &self.exit
    }
}

/// Exact request-bound observation whose exit may be confirmed by a later reaper.
/// It is not a stop receipt until `confirmed_stop` returns one.
#[derive(Clone, Debug)]
pub struct LeanWorkerPendingRequestStop {
    request: InFlightRequest,
    exit: LeanWorkerExitObservation,
}

impl LeanWorkerPendingRequestStop {
    /// Exact dispatched operation at this checkpoint; this is not exit evidence.
    #[must_use]
    pub fn operation(&self) -> &'static str {
        self.request.operation
    }

    /// Return stop evidence only after the owned child's wait/reap resolves.
    /// This never infers proof success or that no prior IO effects occurred.
    #[must_use]
    pub fn confirmed_stop(&self) -> Option<LeanWorkerRequestStopReceipt> {
        self.exit.confirmed_exit().map(|status| LeanWorkerRequestStopReceipt {
            request: self.request.clone(),
            exit: LeanWorkerExit::from_status(status, String::new()),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorkerGeneration(u64);

impl WorkerGeneration {
    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorkerRequestId(u64);

impl WorkerRequestId {
    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InFlightRequest {
    id: WorkerRequestId,
    operation: &'static str,
    generation: WorkerGeneration,
    process_id: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkerSupervisorState {
    Idle {
        generation: WorkerGeneration,
    },
    Busy {
        request: InFlightRequest,
    },
    Streaming {
        request: InFlightRequest,
    },
    Stopping {
        generation: WorkerGeneration,
    },
    Killing {
        generation: WorkerGeneration,
    },
    Reaping {
        generation: WorkerGeneration,
    },
    Crashed {
        generation: WorkerGeneration,
    },
    /// Abnormal restarts used up the window. `until` is when the oldest counted
    /// restart leaves it, or `None` when the window is too long to reach.
    RestartExhausted {
        generation: WorkerGeneration,
        until: Option<Instant>,
    },
    Exited {
        generation: WorkerGeneration,
    },
}

impl WorkerSupervisorState {
    fn rejects_new_requests(&self) -> bool {
        matches!(
            self,
            Self::Stopping { .. } | Self::Killing { .. } | Self::Reaping { .. }
        )
    }

    fn current_operation(&self) -> Option<&'static str> {
        match self {
            Self::Busy { request } | Self::Streaming { request } => Some(request.operation),
            Self::Idle { .. }
            | Self::Stopping { .. }
            | Self::Killing { .. }
            | Self::Reaping { .. }
            | Self::Crashed { .. }
            | Self::RestartExhausted { .. }
            | Self::Exited { .. } => None,
        }
    }

    fn current_request_id(&self) -> Option<WorkerRequestId> {
        match self {
            Self::Busy { request } | Self::Streaming { request } => Some(request.id),
            Self::Idle { .. }
            | Self::Stopping { .. }
            | Self::Killing { .. }
            | Self::Reaping { .. }
            | Self::Crashed { .. }
            | Self::RestartExhausted { .. }
            | Self::Exited { .. } => None,
        }
    }
}

/// Supervisor for one `lean-rs-worker` child process.
///
/// Dropping a live supervisor starts the same bounded shutdown path as
/// explicit shutdown, but cannot report kill or wait failures. Call
/// [`LeanWorker::shutdown`] when callers need structured exit status.
#[derive(Debug)]
pub struct LeanWorker {
    config: LeanWorkerConfig,
    child: Option<OwnedChild>,
    stdin: Option<BufWriter<Pipe<ChildStdin>>>,
    request_started: Option<Instant>,
    stdout: Option<BufReader<Pipe<ChildStdout>>>,
    stderr: Option<StderrCapture>,
    last_exit: Option<LeanWorkerExit>,
    request_scope: Arc<()>,
    last_request: Option<InFlightRequest>,
    last_request_stop: Option<LeanWorkerRequestStopReceipt>,
    last_request_exit: Option<LeanWorkerPendingRequestStop>,
    /// A failed replacement's process is still owned until cleanup proves exit.
    /// Historical `last_exit` belongs to an earlier child and is not that proof.
    pending_startup: Option<Box<StartupChild>>,
    runtime_metadata: LeanWorkerRuntimeMetadata,
    host_runtime_metadata: Option<LeanWorkerHostRuntimeMetadata>,
    last_host_session_refusal: Option<InFlightRequest>,
    stats: LeanWorkerStats,
    budget: GenerationBudget,
    /// The session configs the live child was observed to hold, MRU at the
    /// back, used only to predict whether the next `open_worker_session` will
    /// import.
    ///
    /// A *list* because the child pools imported environments rather than
    /// holding one: with a single key, a workload alternating two profiles
    /// mispredicts every switch, and the resulting `max_imports` enforcement
    /// restarts the child before it can answer `HostSessionReused` — which
    /// would make the pool dead on arrival. Bounded by
    /// [`Self::reuse_hint_capacity`], which is the same `max_imports` the child
    /// derives its pool capacity from.
    ///
    /// Deliberately *not* carried across `restart_with_reason_before_spawn`,
    /// which rebuilds through `*self = next`: the omission is what makes the
    /// hint provably fresh on every restart path, with no code to forget.
    open_session_keys: Vec<LeanWorkerSessionConfig>,
    /// Cleared the first time a child re-imports a config it was predicted to
    /// reuse **and has never demonstrated reuse at all**, which is how a
    /// pre-reuse worker binary is detected. Without it, a stale binary would
    /// keep answering `HostSessionOpened` while the parent kept skipping the
    /// `max_imports` check that exists to bound exactly that growth.
    ///
    /// The `reuse_observed` qualifier is what a pool requires: once a child has
    /// reused a session, a later miss is ordinary staleness — an eviction, or a
    /// child whose capacity is below this parent's hint capacity — and
    /// disabling the hint for the whole generation on the first eviction would
    /// turn pooling off permanently.
    reuse_hint_enabled: bool,
    /// Whether this child generation has ever answered `HostSessionReused`.
    reuse_observed: bool,
    last_activity: Instant,
    generation: WorkerGeneration,
    next_request_id: WorkerRequestId,
    /// When each abnormal restart inside the window was admitted, oldest first.
    restart_window: VecDeque<Instant>,
    /// Why the child is gone when the request that lost it could not replace
    /// it: an expired deadline, a refused restart, or a failed start. The next
    /// request or [`Self::cycle`] replaces the child with this reason.
    recovery: Option<LeanWorkerRestartReason>,
    /// Set by [`Self::try_shutdown`]: requests are refused until
    /// [`Self::cycle`] starts a new child.
    stopped: bool,
    state: WorkerSupervisorState,
}

impl LeanWorker {
    /// Return the current worker lifecycle status.
    ///
    /// `Exited` means no child runs now; it is not final. Unless
    /// [`Self::try_shutdown`] stopped the worker, the next request or
    /// [`Self::cycle`] starts a new child, once the restart window allows it.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if checking the process status fails.
    pub fn status(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        if self.pending_startup.is_some() {
            return Err(self.dead_error());
        }
        if let Some(exit) = self.last_exit.clone() {
            self.mark_exited();
            return Ok(LeanWorkerStatus::Exited(exit));
        }
        let Some(child) = self.child.as_mut() else {
            self.mark_exited();
            return Ok(LeanWorkerStatus::Exited(LeanWorkerExit {
                success: false,
                code: None,
                status: "worker is not running".to_owned(),
                diagnostics: String::new(),
            }));
        };
        match child.try_wait().map_err(|source| LeanWorkerError::Wait { source })? {
            Some(status) => {
                let exit = self.finalize_child_exit(status);
                Ok(LeanWorkerStatus::Exited(exit))
            }
            None => Ok(LeanWorkerStatus::Running),
        }
    }

    /// Record that no child runs, without hiding a used-up restart window.
    fn mark_exited(&mut self) {
        if !matches!(self.state, WorkerSupervisorState::RestartExhausted { .. }) {
            self.state = WorkerSupervisorState::Exited {
                generation: self.generation,
            };
        }
    }

    /// Whether this owner is retiring rather than ready for another request.
    ///
    /// This only observes stored state. It does not dispatch, poll, prove exit,
    /// or release capacity. An exited leader with live inherited descendants
    /// remains pending until actual owned cleanup completes.
    #[must_use]
    pub fn cleanup_pending(&self) -> bool {
        self.pending_startup.is_some()
            || self.child.as_ref().is_some_and(|child| {
                self.state.rejects_new_requests()
                    || matches!(self.state, WorkerSupervisorState::Crashed { .. })
                    || child.cleanup_pending()
            })
    }

    /// Return lifecycle counters for this supervisor.
    #[must_use]
    pub fn stats(&self) -> LeanWorkerStats {
        self.stats.clone()
    }

    /// Return policy-facing lifecycle facts for this supervisor.
    #[must_use]
    pub fn lifecycle_snapshot(&self) -> LeanWorkerLifecycleSnapshot {
        let now = Instant::now();
        let window = self.config.restart_policy.restart_intensity.window;
        let in_window = self
            .restart_window
            .iter()
            .filter(|admitted| now.saturating_duration_since(**admitted) < window)
            .count();
        LeanWorkerLifecycleSnapshot::from_worker(
            &self.stats,
            self.last_exit.clone(),
            self.budget.import_residue_bytes,
            self.config.restart_policy.max_import_residue_bytes,
            u64::try_from(in_window).unwrap_or(u64::MAX),
            self.restart_retry_after(now),
        )
    }

    /// Correlate a subsequent synchronous operation with confirmed child exit.
    #[must_use]
    pub fn request_checkpoint(&self) -> LeanWorkerRequestCheckpoint {
        LeanWorkerRequestCheckpoint {
            supervisor: Arc::clone(&self.request_scope),
            next_request_id: self.next_request_id,
        }
    }

    /// Return stop evidence for the latest request attempted since `checkpoint`.
    ///
    /// Returns `None` for a different supervisor, an older receipt, a later
    /// request without confirmed exit, or failed kill/reap. A receipt may remain
    /// available while replacement startup has failed: it certifies only the
    /// original request's direct child, never replacement cleanup or pool capacity.
    #[must_use]
    pub fn confirmed_request_stop_since(
        &self,
        checkpoint: &LeanWorkerRequestCheckpoint,
    ) -> Option<&LeanWorkerRequestStopReceipt> {
        let receipt = self.last_request_stop.as_ref()?;
        (Arc::ptr_eq(&self.request_scope, &checkpoint.supervisor)
            && receipt.request.id.0 >= checkpoint.next_request_id.0
            && self.last_request.as_ref() == Some(&receipt.request))
        .then_some(receipt)
    }

    /// Observe the exact latest request since a checkpoint, even when owned
    /// cleanup has transferred its child to the deferred reaper.
    #[must_use]
    pub fn pending_request_stop_since(
        &self,
        checkpoint: &LeanWorkerRequestCheckpoint,
    ) -> Option<LeanWorkerPendingRequestStop> {
        let pending = self.last_request_exit.as_ref()?;
        (Arc::ptr_eq(&self.request_scope, &checkpoint.supervisor)
            && pending.request.id.0 >= checkpoint.next_request_id.0
            && self.last_request.as_ref() == Some(&pending.request))
        .then(|| pending.clone())
    }

    /// Exact framed host-session refusal before a body, preserved across cleanup.
    /// This names a request since this supervisor's checkpoint, not proof or exit.
    #[must_use]
    pub fn host_session_refusal_since(&self, checkpoint: &LeanWorkerRequestCheckpoint) -> Option<&'static str> {
        let request = self.last_host_session_refusal.as_ref()?;
        (Arc::ptr_eq(&self.request_scope, &checkpoint.supervisor) && request.id.0 >= checkpoint.next_request_id.0)
            .then_some(request.operation)
    }

    /// Return protocol/runtime facts reported by the worker child.
    #[must_use]
    pub fn runtime_metadata(&self) -> LeanWorkerRuntimeMetadata {
        self.runtime_metadata.clone()
    }

    /// Return the timeout used for subsequent worker requests.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.config.request_timeout
    }

    /// Change the timeout for subsequent worker requests.
    ///
    /// This changes supervisor policy only. The supervisor still owns the
    /// deadline, child kill, replacement, and restart accounting.
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.config.request_timeout = timeout;
    }

    #[doc(hidden)]
    /// Kill the child process for supervisor tests.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is already dead or the OS kill
    /// request fails.
    pub fn __kill_for_test(&mut self) -> Result<(), LeanWorkerError> {
        let Some(child) = self.child.as_mut() else {
            return Err(self.dead_error());
        };
        child.kill().map_err(|source| LeanWorkerError::Kill { source })?;
        Ok(())
    }

    #[doc(hidden)]
    /// Return the child process id for supervisor tests.
    #[must_use]
    pub fn __child_pid_for_test(&self) -> Option<u32> {
        self.child.as_ref().map(|child| child.id())
    }
}
