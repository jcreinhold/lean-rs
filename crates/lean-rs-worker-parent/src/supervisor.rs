use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::io::{BufReader, BufWriter, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::protocol::{
    HostSessionMode, MAX_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP, MIN_FRAME_BYTES, Message, Request, Response,
    read_frame, write_frame,
};
use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationFacts,
    LeanWorkerDeclarationVerificationRequest, LeanWorkerDeclarationVerificationResult,
    LeanWorkerDeclarationVerificationStatus, LeanWorkerDoctorReport, LeanWorkerElabOptions, LeanWorkerElabResult,
    LeanWorkerImportStats, LeanWorkerKernelResult, LeanWorkerMetaResult, LeanWorkerMetaTransparency,
    LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchEnvelope, LeanWorkerModuleQueryBatchItem,
    LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQueryOutcome,
    LeanWorkerModuleQuerySelector, LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerOutputBudgets,
    LeanWorkerProofAttemptRequest, LeanWorkerProofAttemptResult, LeanWorkerRendered, LeanWorkerResourceExhaustedFacts,
};
use lean_rs_worker_protocol::worker_exports::{fixture_mul_signature, fixture_panic_signature};

use crate::capability::LeanWorkerBootstrapDiagnosticCode;
use crate::session::LeanWorkerDataSinkTarget;
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerDataSink, LeanWorkerDiagnosticSink, LeanWorkerProgressSink,
    LeanWorkerRawDataRow, LeanWorkerRawDataSink, LeanWorkerRuntimeMetadata, LeanWorkerSessionConfig,
    LeanWorkerSessionMode, LeanWorkerStreamSummary, check_cancelled, elapsed_event, report_parent_data_row,
    report_parent_diagnostic, report_parent_progress,
};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_EVENT_BUFFER_CAPACITY: usize = 64;
const DEFAULT_RESTART_INTENSITY_LIMIT: u64 = 16;
const DEFAULT_RESTART_INTENSITY_WINDOW: Duration = Duration::from_mins(1);

/// Default deadline for one worker request after startup.
pub const LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/// Suggested deadline for long-running worker requests.
pub const LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING: Duration = Duration::from_mins(10);

/// Default deadline for graceful child shutdown before kill escalation.
pub const LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(2);

/// Default deadline for waiting on a killed child process to be reaped.
pub const LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT: Duration = Duration::from_secs(5);

/// Configuration for starting a `lean-rs-worker` child process.
///
/// The executable should be the `lean-rs-worker-child` binary.
///
/// **Worker-child panic policy.** The supervisor spawns every child with two
/// defaults that together pin a process boundary around Lean panics:
///
/// - `LEAN_ABORT_ON_PANIC=1`—Lean internal panics terminate the child
///   instead of returning default values, so the parent observes a fatal
///   exit rather than silently-corrupted state.
/// - `LEAN_BACKTRACE=0`—Lean's panic-time backtrace handler is skipped.
///   Since Lean 4.30 that handler calls back into Lean code (the demangler
///   is now `@[export]`'d from `Lean.Compiler.NameDemangling`); a worker
///   child embeds a minimal Lean and cannot guarantee that callback's
///   transitive module dependencies are initialized when user code panics.
///   Disabling the backtrace removes that dependency entirely. See
///   `docs/architecture/06-panic-containment.md` for the boundary argument.
///
/// Both are safe defaults—explicit `.env()` entries supplied here override
/// them, in case a caller knows the dependency is satisfied and wants a
/// demangled backtrace on the child's stderr.
#[derive(Clone, Debug)]
pub struct LeanWorkerConfig {
    executable: PathBuf,
    current_dir: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    startup_timeout: Duration,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    restart_policy: LeanWorkerRestartPolicy,
    rss_hard_limit_kib: Option<u64>,
    rss_sample_interval: Duration,
    max_frame_bytes: u32,
}

impl LeanWorkerConfig {
    /// Create a worker configuration for a child executable.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            current_dir: None,
            env: Vec::new(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            request_timeout: LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT,
            shutdown_timeout: LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT,
            restart_policy: LeanWorkerRestartPolicy::default(),
            rss_hard_limit_kib: None,
            rss_sample_interval: Duration::from_millis(250),
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }

    /// Return the child executable path.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Set the child working directory.
    #[must_use]
    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    /// Add or override one child environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[cfg(test)]
    pub(crate) fn env_overrides(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    /// Set the maximum time to wait for the child handshake.
    #[must_use]
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Set the maximum time to wait for one request's terminal response.
    ///
    /// The request timeout starts after the request frame is written. It covers
    /// live rows, diagnostics, progress events, and the terminal response. On
    /// timeout, the supervisor kills and replaces the child process.
    #[must_use]
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Set the maximum time to wait for graceful worker shutdown.
    ///
    /// Explicit shutdown and `Drop` both ask the child to terminate first.
    /// If this deadline expires before the child exits, the supervisor kills
    /// the process and waits for it to be reaped.
    #[must_use]
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Use the documented long-running request timeout profile.
    #[must_use]
    pub fn long_running_requests(mut self) -> Self {
        self.request_timeout = LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING;
        self
    }

    /// Set the worker restart policy.
    ///
    /// Policy checks run before requests enter the child. A policy restart is a
    /// process restart; it is the only supported reset for Lean process-global
    /// runtime and import state.
    #[must_use]
    pub fn restart_policy(mut self, policy: LeanWorkerRestartPolicy) -> Self {
        self.restart_policy = policy;
        self
    }

    /// Kill and replace the worker during an in-flight request if its RSS is
    /// sampled at or above `limit_kib`.
    ///
    /// This is a protective hard stop, not a throughput optimization. The
    /// request that crosses the limit returns
    /// [`LeanWorkerError::RssHardLimitExceeded`] after the child is replaced.
    #[must_use]
    pub fn rss_hard_limit(mut self, limit_kib: u64, sample_interval: Duration) -> Self {
        self.rss_hard_limit_kib = Some(limit_kib.max(1));
        self.rss_sample_interval = sample_interval.max(Duration::from_millis(1));
        self
    }

    /// Set the per-frame byte cap negotiated with the worker child at handshake.
    ///
    /// The cap is announced to the child immediately after its handshake frame
    /// and applies in both directions for the lifetime of the connection. The
    /// default is [`MAX_FRAME_BYTES`] (1 MiB), which is enough for every
    /// session-backed tool whose result composes from many frames. Capabilities
    /// whose *single* logical result is a frame—e.g. an outline of an
    /// entire module, or a diagnostics snapshot of a refactor-in-progress
    /// file—can raise the cap here to admit larger envelopes.
    ///
    /// Values are clamped into <code>[[MIN_FRAME_BYTES], [MAX_FRAME_BYTES_HARD_CAP]]</code>.
    /// The floor keeps even a malformed setter from breaking the handshake
    /// itself; the ceiling prevents the memory-safety policy from being
    /// defeated by an absurd value.
    #[must_use]
    pub fn max_frame_bytes(mut self, max_frame_bytes: u32) -> Self {
        self.max_frame_bytes = max_frame_bytes.clamp(MIN_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP);
        self
    }
}

/// Policy for cycling a worker child before the next request.
///
/// The policy resets retained Lean runtime memory only by restarting the
/// process. It does not change `lean-rs-host`'s in-process memory model, and it
/// does not imply that `SessionPool::drain()` can return process-global Lean
/// memory to the OS.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanWorkerRestartPolicy {
    max_requests: Option<u64>,
    max_imports: Option<u64>,
    max_rss_kib: Option<u64>,
    idle_restart_after: Option<Duration>,
    restart_intensity: RestartIntensityLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RestartIntensityLimit {
    max_restarts: u64,
    window: Duration,
}

impl Default for RestartIntensityLimit {
    fn default() -> Self {
        Self {
            max_restarts: DEFAULT_RESTART_INTENSITY_LIMIT,
            window: DEFAULT_RESTART_INTENSITY_WINDOW,
        }
    }
}

impl LeanWorkerRestartPolicy {
    /// Disable automatic policy restarts.
    ///
    /// Use only for short-lived tests, benchmarks, or hosts that enforce a
    /// process memory boundary elsewhere. Long-running Lean hosts should use
    /// [`Self::memory_bounded`] and pair it with `LeanWorkerPoolConfig` total
    /// and per-worker RSS budgets, because fresh imports retain Lean
    /// process-global state until the child exits.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Restart before fresh-import-like requests or RSS growth can accumulate
    /// without bound in one child process.
    ///
    /// The production default shape is one full-session import per worker
    /// under a measured local RSS cap:
    ///
    /// ```rust
    /// # use lean_rs_worker_parent::LeanWorkerRestartPolicy;
    /// let policy = LeanWorkerRestartPolicy::memory_bounded(1, 1_572_864);
    /// ```
    ///
    /// This is admission and cycling policy, not memory reclamation inside a
    /// running Lean process.
    #[must_use]
    pub fn memory_bounded(max_imports: u64, max_rss_kib: u64) -> Self {
        Self::default().max_imports(max_imports).max_rss_kib(max_rss_kib)
    }

    /// Restart before a request when this many requests have entered the child.
    #[must_use]
    pub fn max_requests(mut self, limit: u64) -> Self {
        self.max_requests = Some(limit.max(1));
        self
    }

    /// Restart before an import-like request when this many imports have run.
    #[must_use]
    pub fn max_imports(mut self, limit: u64) -> Self {
        self.max_imports = Some(limit.max(1));
        self
    }

    /// Restart before a request when measured child RSS is at least this many KiB.
    ///
    /// RSS measurement is best effort. It is implemented for the current
    /// supported Unix development targets; unsupported platforms skip the
    /// check and increment `LeanWorkerStats::rss_samples_unavailable`.
    #[must_use]
    pub fn max_rss_kib(mut self, limit: u64) -> Self {
        self.max_rss_kib = Some(limit.max(1));
        self
    }

    /// Restart before a request when the worker has been idle for this long.
    #[must_use]
    pub fn idle_restart_after(mut self, duration: Duration) -> Self {
        self.idle_restart_after = Some(duration);
        self
    }

    /// Refuse replacement after this many restarts in one moving time window.
    ///
    /// The limit is enforced after the old child reaches a terminal state and
    /// before a replacement is spawned. Exhaustion returns
    /// [`LeanWorkerError::RestartLimitExceeded`] and leaves the supervisor
    /// without an accepted child; create a new worker or pool entry to apply a
    /// fresh restart window.
    #[must_use]
    pub fn max_restarts_per_window(mut self, max_restarts: u64, window: Duration) -> Self {
        self.restart_intensity = RestartIntensityLimit {
            max_restarts: max_restarts.max(1),
            window: window.max(Duration::from_millis(1)),
        };
        self
    }
}

/// Reason recorded for the latest worker cycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeanWorkerRestartReason {
    /// The caller explicitly requested a process cycle.
    Explicit,
    /// Request count reached the configured limit before the next request.
    MaxRequests { limit: u64 },
    /// Import-like request count reached the configured limit before the next import.
    MaxImports { limit: u64 },
    /// Child resident set size reached the configured limit.
    RssCeiling {
        current_kib: u64,
        limit_kib: u64,
        last_import_stats: Option<LeanWorkerImportStats>,
    },
    /// Child resident set size crossed the hard in-flight kill limit.
    RssHardLimit {
        operation: &'static str,
        current_kib: u64,
        limit_kib: u64,
        last_import_stats: Option<LeanWorkerImportStats>,
    },
    /// Worker was idle at least as long as the configured limit.
    Idle { idle_for: Duration, limit: Duration },
    /// Parent-side cancellation replaced the child during an in-flight request.
    Cancelled { operation: &'static str },
    /// Parent-side request timeout replaced the child during an in-flight request.
    RequestTimeout {
        operation: &'static str,
        duration: Duration,
    },
    /// The child aborted (SIGABRT / fatal panic) during an in-flight request and
    /// the supervisor respawned it. Used by the read-only verify/proof-state
    /// guard that converts such an abort into a degraded verdict instead of a
    /// hard error.
    ChildAbort { operation: &'static str },
}

impl LeanWorkerRestartReason {
    /// Stable wire/policy cause name for this restart reason.
    ///
    /// This is intentionally smaller than the full enum payload: callers can
    /// branch on the cause while still using the typed enum when they need
    /// details such as limits or durations.
    #[must_use]
    pub const fn stable_cause(&self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::MaxRequests { .. } => "max_requests",
            Self::MaxImports { .. } => "max_imports",
            Self::RssCeiling { .. } => "rss_ceiling",
            Self::RssHardLimit { .. } => "rss_hard_limit",
            Self::Idle { .. } => "idle",
            Self::Cancelled { .. } => "cancelled",
            Self::RequestTimeout { .. } => "timeout",
            Self::ChildAbort { .. } => "child_abort",
        }
    }
}

/// Timing facts for the most recent synchronous worker replacement.
///
/// These are observability facts only. A replacement remains a process cycle
/// hidden behind supervisor or pool policy; callers do not receive child
/// identities or lifecycle handles.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanWorkerReplacementTiming {
    pub spawn_handshake: Duration,
    pub capability_load: Duration,
    pub session_open_import: Duration,
    pub first_command: Option<Duration>,
    pub warm_command: Option<Duration>,
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
    /// Policy or explicit restarts performed by the supervisor.
    pub restarts: u64,
    /// Explicit process cycles.
    pub explicit_cycles: u64,
    /// Restarts caused by `LeanWorkerRestartPolicy::max_requests`.
    pub max_request_restarts: u64,
    /// Restarts caused by `LeanWorkerRestartPolicy::max_imports`.
    pub max_import_restarts: u64,
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
    /// Most recent capability build/load phase elapsed time observed by a capability builder.
    pub last_capability_load_elapsed: Option<Duration>,
    /// Most recent host-session open/import elapsed time.
    pub last_session_open_import_elapsed: Option<Duration>,
    /// Most recent first command elapsed time after opening or replacing a worker.
    pub last_first_command_elapsed: Option<Duration>,
    /// Most recent warm command elapsed time on an already-open worker.
    pub last_warm_command_elapsed: Option<Duration>,
    /// Lean-native import attribution for the most recent opened host session, if any.
    pub last_import_stats: Option<LeanWorkerImportStats>,
    /// Streaming requests that entered a worker child.
    pub stream_requests: u64,
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
/// timeout, cancellation, RSS limits, import/request cycling, or explicit
/// cycles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerLifecycleSnapshot {
    /// Monotone generation number for the current child. It equals the total
    /// restart count observed by this supervisor.
    pub worker_generation: u64,
    /// Total restarts performed by the supervisor.
    pub restarts: u64,
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
}

impl LeanWorkerLifecycleSnapshot {
    fn from_worker(stats: &LeanWorkerStats, last_exit: Option<LeanWorkerExit>) -> Self {
        Self {
            worker_generation: stats.restarts,
            restarts: stats.restarts,
            exits: stats.exits,
            last_restart_reason: stats.last_restart_reason.clone(),
            last_exit,
            last_rss_kib: stats.last_rss_kib,
            rss_samples_unavailable: stats.rss_samples_unavailable,
        }
    }
}

impl LeanWorkerStats {
    fn record_restart(&mut self, reason: LeanWorkerRestartReason) {
        self.restarts = self.restarts.saturating_add(1);
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
            // The general `restarts` counter and `last_restart_reason` already
            // capture child aborts; `stable_cause() == "child_abort"` keys the
            // parent's relabel, so no dedicated counter is warranted.
            LeanWorkerRestartReason::ChildAbort { .. } => {}
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

/// Structured result of shutting down a worker child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerShutdownReport {
    /// How shutdown reached a terminal child state.
    pub outcome: LeanWorkerShutdownOutcome,
    /// Final child-process exit information.
    pub exit: LeanWorkerExit,
    /// Graceful-shutdown deadline used for this operation.
    pub graceful_timeout: Duration,
    /// Total elapsed shutdown time observed by the parent.
    pub elapsed: Duration,
    /// Time spent after kill escalation, when a kill was needed.
    pub kill_elapsed: Option<Duration>,
    /// Time spent waiting for the final child exit.
    pub wait_elapsed: Duration,
}

/// Shutdown path used to reach a terminal child state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeanWorkerShutdownOutcome {
    /// The child had already exited before shutdown was requested.
    AlreadyExited,
    /// The child accepted `Request::Terminate` and exited without escalation.
    Graceful,
    /// The child did not exit before the graceful deadline, so the parent killed it.
    GracefulTimedOutKilled,
    /// The graceful protocol path failed, so the parent killed the child.
    GracefulProtocolFailedKilled,
    /// The caller requested an immediate kill/reap path.
    KillOnly,
}

/// Errors reported by the worker supervisor.
#[derive(Debug)]
pub enum LeanWorkerError {
    /// The worker child could not be spawned.
    Spawn {
        executable: PathBuf,
        source: std::io::Error,
    },
    /// The default worker child executable could not be resolved.
    WorkerChildUnresolved {
        /// Candidate paths checked by the default resolver.
        tried: Vec<PathBuf>,
    },
    /// The resolved worker child is missing or is not executable.
    WorkerChildNotExecutable { path: PathBuf, reason: String },
    /// Worker bootstrap preflight failed before a real command ran.
    Bootstrap {
        code: LeanWorkerBootstrapDiagnosticCode,
        message: String,
    },
    /// The capability Lake target could not be built.
    CapabilityBuild {
        /// Typed Lake/toolchain diagnostic from `lean-toolchain`.
        diagnostic: lean_toolchain::LinkDiagnostics,
    },
    /// The child process could not be prepared after spawning.
    Setup { message: String },
    /// The child did not complete the startup handshake.
    Handshake { message: String },
    /// The worker protocol failed after the handshake.
    Protocol { message: String },
    /// The child returned a typed worker error.
    Worker { code: String, message: String },
    /// The child exited while a request was in flight.
    ChildExited { exit: LeanWorkerExit },
    /// The child exited fatally while a request was in flight.
    ChildPanicOrAbort { exit: LeanWorkerExit },
    /// A worker operation timed out.
    Timeout {
        operation: &'static str,
        duration: Duration,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// The worker was killed and replaced because an in-flight RSS sample
    /// crossed the hard parent-side limit.
    RssHardLimitExceeded {
        operation: &'static str,
        current_kib: u64,
        limit_kib: u64,
        last_import_stats: Option<Box<LeanWorkerImportStats>>,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// A parent-side cancellation token was observed.
    Cancelled {
        operation: &'static str,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// A parent-side progress sink panicked while handling a worker event.
    ProgressPanic { message: String },
    /// A parent-side data sink panicked while handling a worker row.
    DataSinkPanic { message: String },
    /// A parent-side diagnostic sink panicked while handling a worker diagnostic.
    DiagnosticSinkPanic { message: String },
    /// A streaming export returned a nonzero downstream status byte.
    StreamExportFailed { status: u8 },
    /// The in-child string callback helper returned a callback failure status.
    StreamCallbackFailed { status: u8, description: String },
    /// A streaming callback emitted a malformed row envelope.
    StreamRowMalformed { message: String },
    /// A capability metadata export returned malformed JSON.
    CapabilityMetadataMalformed { message: String },
    /// Capability metadata did not match the caller's requested expectation.
    CapabilityMetadataMismatch {
        export: String,
        expected: Box<LeanWorkerCapabilityMetadata>,
        actual: Box<LeanWorkerCapabilityMetadata>,
    },
    /// A capability doctor export returned malformed JSON.
    CapabilityDoctorMalformed { message: String },
    /// A typed command request could not be serialized as JSON.
    TypedCommandRequestEncode { export: String, message: String },
    /// A typed non-streaming command response could not be decoded.
    TypedCommandResponseDecode { export: String, message: String },
    /// A typed streaming command row payload could not be decoded.
    TypedCommandRowDecode {
        export: String,
        stream: String,
        sequence: u64,
        message: String,
    },
    /// A typed streaming command terminal summary could not be decoded.
    TypedCommandSummaryDecode { export: String, message: String },
    /// A pool session lease was invalidated by a worker lifecycle transition.
    LeaseInvalidated { reason: String },
    /// A local worker pool cannot admit another distinct session key.
    WorkerPoolExhausted {
        max_workers: usize,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// A local worker pool cannot admit work without exceeding its RSS budget.
    WorkerPoolMemoryBudgetExceeded {
        current_kib: u64,
        limit_kib: u64,
        last_import_stats: Option<Box<LeanWorkerImportStats>>,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// Waiting for local worker-pool admission exceeded the configured limit.
    WorkerPoolQueueTimeout {
        waited: Duration,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// A supervising policy refused to restart the worker again in its current window.
    RestartLimitExceeded { restarts: u64, window: Duration },
    /// The public supervisor does not support the requested operation.
    UnsupportedRequest { operation: &'static str },
    /// Waiting for a child process failed.
    Wait { source: std::io::Error },
    /// Killing a child process failed.
    Kill { source: std::io::Error },
    /// Waiting for a child process exceeded the configured bounded wait.
    WaitTimeout {
        operation: &'static str,
        duration: Duration,
    },
    /// The worker has begun shutdown and no longer accepts new requests.
    ShutdownInProgress { operation: &'static str },
}

impl fmt::Display for LeanWorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { executable, source } => {
                write!(f, "failed to spawn worker {}: {source}", executable.display())
            }
            Self::WorkerChildUnresolved { tried } => {
                let tried = tried
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "could not resolve lean-rs-worker-child; set LEAN_RS_WORKER_CHILD or place it beside the current executable (tried: {tried})"
                )
            }
            Self::WorkerChildNotExecutable { path, reason } => {
                write!(f, "worker child '{}' is not executable: {reason}", path.display())
            }
            Self::Bootstrap { code, message } => {
                write!(f, "worker bootstrap check {code} failed: {message}")
            }
            Self::CapabilityBuild { diagnostic } => {
                write!(f, "worker capability Lake target build failed: {diagnostic}")
            }
            Self::Setup { message } => write!(f, "worker child setup failed: {message}"),
            Self::Handshake { message } => write!(f, "worker handshake failed: {message}"),
            Self::Protocol { message } => write!(f, "worker protocol failed: {message}"),
            Self::Worker { code, message } => write!(f, "worker returned {code}: {message}"),
            Self::ChildExited { exit } => write_exit(f, "worker exited", exit),
            Self::ChildPanicOrAbort { exit } => write_exit(f, "worker exited fatally", exit),
            Self::Timeout {
                operation, duration, ..
            } => {
                write!(f, "worker operation {operation} timed out after {duration:?}")
            }
            Self::RssHardLimitExceeded {
                operation,
                current_kib,
                limit_kib,
                last_import_stats,
                ..
            } => {
                write!(
                    f,
                    "worker operation {operation} exceeded hard RSS limit; current_kib={current_kib} limit_kib={limit_kib}; {}",
                    import_stats_diagnostic(last_import_stats.as_deref())
                )
            }
            Self::Cancelled { operation, .. } => write!(f, "worker operation {operation} was cancelled"),
            Self::ProgressPanic { message } => write!(f, "worker progress sink panicked: {message}"),
            Self::DataSinkPanic { message } => write!(f, "worker data sink panicked: {message}"),
            Self::DiagnosticSinkPanic { message } => {
                write!(f, "worker diagnostic sink panicked: {message}")
            }
            Self::StreamExportFailed { status } => write!(f, "streaming export returned status {status}"),
            Self::StreamCallbackFailed { status, description } => {
                write!(f, "streaming callback failed with status {status}: {description}")
            }
            Self::StreamRowMalformed { message } => write!(f, "streaming export emitted malformed row: {message}"),
            Self::CapabilityMetadataMalformed { message } => {
                write!(f, "capability metadata export returned malformed JSON: {message}")
            }
            Self::CapabilityMetadataMismatch { export, .. } => {
                write!(f, "capability metadata from {export} did not match expectation")
            }
            Self::CapabilityDoctorMalformed { message } => {
                write!(f, "capability doctor export returned malformed JSON: {message}")
            }
            Self::TypedCommandRequestEncode { export, message } => {
                write!(f, "typed worker command {export} request JSON encode failed: {message}")
            }
            Self::TypedCommandResponseDecode { export, message } => {
                write!(
                    f,
                    "typed worker command {export} response JSON decode failed: {message}"
                )
            }
            Self::TypedCommandRowDecode {
                export,
                stream,
                sequence,
                message,
            } => {
                write!(
                    f,
                    "typed worker command {export} row decode failed at stream {stream} sequence {sequence}: {message}"
                )
            }
            Self::TypedCommandSummaryDecode { export, message } => {
                write!(
                    f,
                    "typed worker command {export} terminal summary decode failed: {message}"
                )
            }
            Self::LeaseInvalidated { reason } => write!(f, "worker pool lease was invalidated: {reason}"),
            Self::WorkerPoolExhausted { max_workers, .. } => {
                write!(
                    f,
                    "worker pool cannot admit another session key; max_workers={max_workers}"
                )
            }
            Self::WorkerPoolMemoryBudgetExceeded {
                current_kib,
                limit_kib,
                last_import_stats,
                ..
            } => {
                write!(
                    f,
                    "worker pool cannot admit work within RSS budget; current_kib={current_kib} limit_kib={limit_kib}; {}",
                    import_stats_diagnostic(last_import_stats.as_deref())
                )
            }
            Self::WorkerPoolQueueTimeout { waited, .. } => {
                write!(f, "worker pool admission timed out after {waited:?}")
            }
            Self::RestartLimitExceeded { restarts, window } => {
                write!(
                    f,
                    "worker restart limit exceeded after {restarts} restarts in {window:?}"
                )
            }
            Self::UnsupportedRequest { operation } => {
                write!(f, "worker operation {operation} is not supported")
            }
            Self::Wait { source } => write!(f, "failed to wait for worker child: {source}"),
            Self::Kill { source } => write!(f, "failed to kill worker child: {source}"),
            Self::WaitTimeout { operation, duration } => {
                write!(
                    f,
                    "timed out waiting for worker child during {operation} after {duration:?}"
                )
            }
            Self::ShutdownInProgress { operation } => {
                write!(
                    f,
                    "worker operation {operation} was rejected because shutdown is in progress"
                )
            }
        }
    }
}

impl std::error::Error for LeanWorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Wait { source } | Self::Kill { source } => Some(source),
            Self::CapabilityBuild { diagnostic } => Some(diagnostic),
            Self::WorkerChildUnresolved { .. } | Self::WorkerChildNotExecutable { .. } | Self::Bootstrap { .. } => None,
            Self::Setup { .. }
            | Self::Handshake { .. }
            | Self::Protocol { .. }
            | Self::Worker { .. }
            | Self::ChildExited { .. }
            | Self::ChildPanicOrAbort { .. }
            | Self::Timeout { .. }
            | Self::RssHardLimitExceeded { .. }
            | Self::Cancelled { .. }
            | Self::ProgressPanic { .. }
            | Self::DataSinkPanic { .. }
            | Self::DiagnosticSinkPanic { .. }
            | Self::StreamExportFailed { .. }
            | Self::StreamCallbackFailed { .. }
            | Self::StreamRowMalformed { .. }
            | Self::CapabilityMetadataMalformed { .. }
            | Self::CapabilityMetadataMismatch { .. }
            | Self::CapabilityDoctorMalformed { .. }
            | Self::TypedCommandRequestEncode { .. }
            | Self::TypedCommandResponseDecode { .. }
            | Self::TypedCommandRowDecode { .. }
            | Self::TypedCommandSummaryDecode { .. }
            | Self::LeaseInvalidated { .. }
            | Self::WorkerPoolExhausted { .. }
            | Self::WorkerPoolMemoryBudgetExceeded { .. }
            | Self::WorkerPoolQueueTimeout { .. }
            | Self::RestartLimitExceeded { .. }
            | Self::UnsupportedRequest { .. }
            | Self::WaitTimeout { .. }
            | Self::ShutdownInProgress { .. } => None,
        }
    }
}

impl LeanWorkerError {
    /// Structured resource-boundary facts for errors caused by an admission,
    /// timeout, cancellation, or RSS limit. `None` means the error is a
    /// protocol, Lean, build, or lifecycle failure without resource evidence.
    #[must_use]
    pub fn resource_exhausted_facts(&self) -> Option<&LeanWorkerResourceExhaustedFacts> {
        match self {
            Self::Timeout { resource, .. }
            | Self::RssHardLimitExceeded { resource, .. }
            | Self::Cancelled { resource, .. }
            | Self::WorkerPoolExhausted { resource, .. }
            | Self::WorkerPoolMemoryBudgetExceeded { resource, .. }
            | Self::WorkerPoolQueueTimeout { resource, .. } => Some(resource.as_ref()),
            Self::Spawn { .. }
            | Self::Kill { .. }
            | Self::WorkerChildUnresolved { .. }
            | Self::WorkerChildNotExecutable { .. }
            | Self::Bootstrap { .. }
            | Self::CapabilityBuild { .. }
            | Self::Setup { .. }
            | Self::Handshake { .. }
            | Self::Protocol { .. }
            | Self::Worker { .. }
            | Self::ChildExited { .. }
            | Self::ChildPanicOrAbort { .. }
            | Self::ProgressPanic { .. }
            | Self::DataSinkPanic { .. }
            | Self::DiagnosticSinkPanic { .. }
            | Self::StreamExportFailed { .. }
            | Self::StreamCallbackFailed { .. }
            | Self::StreamRowMalformed { .. }
            | Self::CapabilityMetadataMalformed { .. }
            | Self::CapabilityMetadataMismatch { .. }
            | Self::CapabilityDoctorMalformed { .. }
            | Self::TypedCommandRequestEncode { .. }
            | Self::TypedCommandResponseDecode { .. }
            | Self::TypedCommandRowDecode { .. }
            | Self::TypedCommandSummaryDecode { .. }
            | Self::LeaseInvalidated { .. }
            | Self::RestartLimitExceeded { .. }
            | Self::UnsupportedRequest { .. }
            | Self::WaitTimeout { .. }
            | Self::ShutdownInProgress { .. }
            | Self::Wait { .. } => None,
        }
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[allow(
    clippy::too_many_arguments,
    reason = "flat fact construction mirrors the public diagnostic payload"
)]
fn resource_facts(
    cause: impl Into<String>,
    work_entered_child: bool,
    operation: Option<&'static str>,
    current_rss_kib: Option<u64>,
    limit_kib: Option<u64>,
    import_count: Option<u64>,
    worker_generation: Option<u64>,
    restart_reason: Option<String>,
    queue_wait: Option<Duration>,
    duration: Option<Duration>,
    cold_open_attempts: Option<u64>,
    cold_open_admitted: Option<u64>,
    cold_open_refusals: Option<u64>,
    import_like_requests: Option<u64>,
    import_like_admitted: Option<u64>,
    last_import_stats: Option<LeanWorkerImportStats>,
) -> LeanWorkerResourceExhaustedFacts {
    LeanWorkerResourceExhaustedFacts {
        cause: cause.into(),
        work_entered_child,
        operation: operation.map(str::to_owned),
        current_rss_kib,
        limit_kib,
        import_count,
        worker_generation,
        restart_reason,
        queue_wait_ms: queue_wait.map(duration_ms),
        duration_ms: duration.map(duration_ms),
        cold_open_attempts,
        cold_open_admitted,
        cold_open_refusals,
        import_like_requests,
        import_like_admitted,
        last_import_stats,
    }
}

fn worker_resource_facts(
    cause: impl Into<String>,
    work_entered_child: bool,
    operation: Option<&'static str>,
    stats: &LeanWorkerStats,
    current_rss_kib: Option<u64>,
    limit_kib: Option<u64>,
    duration: Option<Duration>,
) -> LeanWorkerResourceExhaustedFacts {
    resource_facts(
        cause,
        work_entered_child,
        operation,
        current_rss_kib,
        limit_kib,
        Some(stats.imports),
        Some(stats.restarts),
        stats
            .last_restart_reason
            .as_ref()
            .map(LeanWorkerRestartReason::stable_cause)
            .map(str::to_owned),
        None,
        duration,
        None,
        None,
        None,
        Some(stats.import_like_admission_attempts),
        Some(stats.import_like_admitted),
        stats.last_import_stats.clone(),
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorkerGeneration(u64);

impl WorkerGeneration {
    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorkerRequestId(u64);

impl WorkerRequestId {
    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InFlightRequest {
    id: WorkerRequestId,
    operation: &'static str,
    generation: WorkerGeneration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkerSupervisorState {
    Idle { generation: WorkerGeneration },
    Busy { request: InFlightRequest },
    Streaming { request: InFlightRequest },
    Stopping { generation: WorkerGeneration },
    Killing { generation: WorkerGeneration },
    Reaping { generation: WorkerGeneration },
    Crashed { generation: WorkerGeneration },
    RestartExhausted { generation: WorkerGeneration },
    Exited { generation: WorkerGeneration },
}

impl WorkerSupervisorState {
    fn rejects_new_requests(&self) -> bool {
        matches!(
            self,
            Self::Stopping { .. } | Self::Killing { .. } | Self::Reaping { .. } | Self::RestartExhausted { .. }
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
}

/// Supervisor for one `lean-rs-worker` child process.
///
/// Dropping a live supervisor starts the same bounded shutdown path as
/// explicit shutdown, but cannot report kill or wait failures. Call
/// [`LeanWorker::shutdown`] when callers need structured exit status.
#[derive(Debug)]
pub struct LeanWorker {
    config: LeanWorkerConfig,
    child: Option<Child>,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: Option<BufReader<ChildStdout>>,
    stderr: Option<ChildStderr>,
    last_exit: Option<LeanWorkerExit>,
    runtime_metadata: LeanWorkerRuntimeMetadata,
    stats: LeanWorkerStats,
    requests_since_restart: u64,
    imports_since_restart: u64,
    last_activity: Instant,
    generation: WorkerGeneration,
    next_request_id: WorkerRequestId,
    restart_window: VecDeque<Instant>,
    state: WorkerSupervisorState,
}

#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "Response and Message are #[non_exhaustive] across the lean-rs-worker-protocol crate boundary; every wildcard arm here uniformly converts an unexpected variant into a protocol-level error rather than enumerating each known variant"
)]
impl LeanWorker {
    /// Spawn a worker child and wait for its protocol handshake.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the child cannot be spawned, child setup
    /// fails, the child exits before handshaking, or the startup timeout
    /// expires.
    pub fn spawn(config: &LeanWorkerConfig) -> Result<Self, LeanWorkerError> {
        let spawn_started = Instant::now();
        let mut command = Command::new(&config.executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("LEAN_ABORT_ON_PANIC", "1")
            .env("LEAN_BACKTRACE", "0")
            .env("RUST_BACKTRACE", "0");

        if let Some(current_dir) = &config.current_dir {
            command.current_dir(current_dir);
        }
        for (key, value) in &config.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|source| LeanWorkerError::Spawn {
            executable: config.executable.clone(),
            source,
        })?;

        let mut stdin = child
            .stdin
            .take()
            .map(BufWriter::new)
            .ok_or_else(|| LeanWorkerError::Setup {
                message: "child stdin unavailable".to_owned(),
            })?;
        let stdout = child.stdout.take().ok_or_else(|| LeanWorkerError::Setup {
            message: "child stdout unavailable".to_owned(),
        })?;
        let stderr = child.stderr.take();

        let max_frame_bytes = config.max_frame_bytes;
        let (sender, receiver) = mpsc::channel();
        let _handshake_reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let result = expect_handshake(&mut stdout, max_frame_bytes);
            drop(sender.send((stdout, result)));
        });

        let (stdout, runtime_metadata) = match receiver.recv_timeout(config.startup_timeout) {
            Ok((stdout, Ok(metadata))) => (stdout, metadata),
            Ok((_stdout, Err(_handshake_err))) => {
                // The handshake-thread observed a protocol-level error reading
                // the child's first frame. In practice this means the child is
                // mid-`abort()` and hasn't quite died yet—using `try_wait`
                // (non-blocking) here loses the race and drops the child's
                // stderr. Kill if still alive, then go through the canonical
                // post-mortem path so `LeanWorkerExit.diagnostics` carries the
                // bootstrap stderr with the same diagnostic contract as a runtime crash.
                drop(child.kill());
                let exit = wait_with_stderr(&mut child, stderr)?;
                return Err(if exit.success {
                    LeanWorkerError::ChildExited { exit }
                } else {
                    LeanWorkerError::ChildPanicOrAbort { exit }
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                drop(child.kill());
                let _exit = wait_with_stderr(&mut child, stderr)?;
                return Err(LeanWorkerError::Timeout {
                    operation: "startup",
                    duration: config.startup_timeout,
                    resource: Box::new(resource_facts(
                        "worker_timeout",
                        false,
                        Some("startup"),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(config.startup_timeout),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )),
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(LeanWorkerError::Handshake {
                    message: "handshake reader exited without a result".to_owned(),
                });
            }
        };

        // Negotiate the per-connection frame cap to the child. The child
        // blocks on this frame after sending its handshake; until it lands,
        // no Request frame can be sent.
        write_frame(
            &mut stdin,
            Message::ConfigureFrameLimit { max_frame_bytes },
            max_frame_bytes,
        )
        .map_err(|err| LeanWorkerError::Protocol {
            message: format!("failed to send ConfigureFrameLimit: {err}"),
        })?;

        let spawn_handshake_elapsed = spawn_started.elapsed();
        Ok(Self {
            config: config.clone(),
            child: Some(child),
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr,
            last_exit: None,
            runtime_metadata,
            stats: LeanWorkerStats {
                last_spawn_handshake_elapsed: Some(spawn_handshake_elapsed),
                ..LeanWorkerStats::default()
            },
            requests_since_restart: 0,
            imports_since_restart: 0,
            last_activity: Instant::now(),
            generation: WorkerGeneration::default(),
            next_request_id: WorkerRequestId::default(),
            restart_window: VecDeque::new(),
            state: WorkerSupervisorState::Idle {
                generation: WorkerGeneration::default(),
            },
        })
    }

    /// Check whether the worker responds to requests.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the protocol fails, or
    /// the child returns a typed worker error.
    pub fn health(&mut self) -> Result<(), LeanWorkerError> {
        self.prepare_request(false)?;
        self.send_request(Request::Health)?;
        self.record_request(false);
        match self.read_response("health")? {
            Response::HealthOk => Ok(()),
            other => Err(unexpected_response("health", &other)),
        }
    }

    /// Load the in-tree fixture capability in the worker child.
    ///
    /// This is a fixture-only entry point used to exercise the supervisor path
    /// in tests. The supported public path is `open_session`, which returns the
    /// host-session adapter instead of expanding this fixture surface.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, fixture loading fails,
    /// or protocol communication fails.
    pub fn load_fixture_capability(&mut self, fixture_root: impl AsRef<Path>) -> Result<(), LeanWorkerError> {
        let manifest_path = fixture_capability_manifest(fixture_root.as_ref())?;
        self.prepare_request(true)?;
        self.send_request(Request::LoadFixtureCapability {
            manifest_path: path_string(&manifest_path),
        })?;
        self.record_request(true);
        match self.read_response("load_fixture_capability")? {
            Response::CapabilityLoaded => Ok(()),
            other => Err(unexpected_response("load_fixture_capability", &other)),
        }
    }

    /// Call the fixture multiplication export in the worker child.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the export fails, or
    /// protocol communication fails.
    pub fn call_fixture_mul(
        &mut self,
        fixture_root: impl AsRef<Path>,
        lhs: u64,
        rhs: u64,
    ) -> Result<u64, LeanWorkerError> {
        let manifest_path = fixture_capability_manifest(fixture_root.as_ref())?;
        self.prepare_request(true)?;
        self.send_request(Request::CallFixtureMul {
            manifest_path: path_string(&manifest_path),
            lhs,
            rhs,
        })?;
        self.record_request(true);
        match self.read_response("call_fixture_mul")? {
            Response::U64 { value } => Ok(value),
            other => Err(unexpected_response("call_fixture_mul", &other)),
        }
    }

    /// Return the current worker lifecycle status.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if checking the process status fails.
    pub fn status(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        if let Some(exit) = &self.last_exit {
            self.state = WorkerSupervisorState::Exited {
                generation: self.generation,
            };
            return Ok(LeanWorkerStatus::Exited(exit.clone()));
        }
        let Some(child) = self.child.as_mut() else {
            self.state = WorkerSupervisorState::Exited {
                generation: self.generation,
            };
            return Ok(LeanWorkerStatus::Exited(LeanWorkerExit {
                success: false,
                code: None,
                status: "worker is not running".to_owned(),
                diagnostics: String::new(),
            }));
        };
        match child.try_wait().map_err(|source| LeanWorkerError::Wait { source })? {
            Some(status) => {
                let diagnostics = self.read_stderr();
                let exit = LeanWorkerExit::from_status(status, diagnostics);
                self.last_exit = Some(exit.clone());
                self.child = None;
                self.stdin = None;
                self.stdout = None;
                self.stats.exits = self.stats.exits.saturating_add(1);
                self.state = WorkerSupervisorState::Exited {
                    generation: self.generation,
                };
                Ok(LeanWorkerStatus::Exited(exit))
            }
            None => Ok(LeanWorkerStatus::Running),
        }
    }

    /// Return lifecycle counters for this supervisor.
    #[must_use]
    pub fn stats(&self) -> LeanWorkerStats {
        self.stats.clone()
    }

    pub(crate) fn record_capability_open_timing(
        &mut self,
        capability_load_elapsed: Duration,
        session_open_import_elapsed: Duration,
    ) {
        self.stats.last_capability_load_elapsed = Some(capability_load_elapsed);
        self.stats.last_session_open_import_elapsed = Some(session_open_import_elapsed);
        if let Some(timing) = self.stats.last_replacement_timing.as_mut() {
            timing.capability_load = capability_load_elapsed;
            timing.session_open_import = session_open_import_elapsed;
        }
    }

    pub(crate) fn record_command_timing(&mut self, first_command_after_open: bool, elapsed: Duration) {
        if first_command_after_open {
            self.stats.last_first_command_elapsed = Some(elapsed);
            if let Some(timing) = self.stats.last_replacement_timing.as_mut() {
                timing.first_command = Some(elapsed);
            }
        } else {
            self.stats.last_warm_command_elapsed = Some(elapsed);
            if let Some(timing) = self.stats.last_replacement_timing.as_mut() {
                timing.warm_command = Some(elapsed);
            }
        }
    }

    /// Return policy-facing lifecycle facts for this supervisor.
    #[must_use]
    pub fn lifecycle_snapshot(&self) -> LeanWorkerLifecycleSnapshot {
        LeanWorkerLifecycleSnapshot::from_worker(&self.stats, self.last_exit.clone())
    }

    /// Return protocol/runtime facts reported by the worker child.
    #[must_use]
    pub fn runtime_metadata(&self) -> LeanWorkerRuntimeMetadata {
        self.runtime_metadata.clone()
    }

    /// Measure the current child RSS in KiB when supported by the platform.
    ///
    /// This is an observability hook for restart policy and memory-cycling
    /// workloads. A `None` result means the platform did not provide a usable
    /// sample; it is not a worker failure.
    pub fn rss_kib(&mut self) -> Option<u64> {
        match self.child_rss_kib() {
            Some(value) => {
                self.stats.last_rss_kib = Some(value);
                Some(value)
            }
            None => {
                self.stats.rss_samples_unavailable = self.stats.rss_samples_unavailable.saturating_add(1);
                None
            }
        }
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

    /// Explicitly cycle the worker process.
    ///
    /// This is the manual memory-reset operation. It terminates the current
    /// child, starts a replacement with the original configuration, and records
    /// `LeanWorkerRestartReason::Explicit`.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the existing child cannot be waited on or
    /// the replacement child cannot be spawned and handshaken.
    pub fn cycle(&mut self) -> Result<(), LeanWorkerError> {
        self.restart_with_reason(LeanWorkerRestartReason::Explicit)
    }

    pub(crate) fn cycle_with_restart_reason(&mut self, reason: LeanWorkerRestartReason) -> Result<(), LeanWorkerError> {
        self.restart_with_reason(reason)
    }

    /// Restart this worker using its original configuration.
    ///
    /// This is an explicit lifecycle operation. Prompt 58 adds policy-driven
    /// restarts for memory cycling; this method only gives callers a direct
    /// reset point.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the existing child cannot be waited on or
    /// the replacement child cannot be spawned and handshaken.
    pub fn restart(&mut self) -> Result<(), LeanWorkerError> {
        self.cycle()
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
        self.child.as_ref().map(Child::id)
    }

    /// Ask the child to terminate cleanly and wait for it.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is already dead, the protocol
    /// fails, or waiting for the child process fails.
    #[deprecated(note = "use LeanWorker::shutdown for structured shutdown status")]
    pub fn terminate(self) -> Result<LeanWorkerExit, LeanWorkerError> {
        self.shutdown().map(|report| report.exit)
    }

    /// Shut down the worker child, escalating to kill after a bounded grace period.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the terminate request cannot be written,
    /// kill escalation fails, or the child cannot be reaped within the bounded
    /// wait.
    pub fn shutdown(mut self) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        self.shutdown_child(ShutdownIntent::Graceful)
    }

    #[doc(hidden)]
    /// Trigger the fixture panic path.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker does not exit fatally or if the
    /// protocol fails before the panic path runs.
    pub fn __trigger_lean_panic_fixture(
        mut self,
        fixture_root: impl AsRef<Path>,
    ) -> Result<LeanWorkerExit, LeanWorkerError> {
        let manifest_path = fixture_capability_manifest(fixture_root.as_ref())?;
        self.prepare_request(true)?;
        self.send_request(Request::TriggerLeanPanic {
            manifest_path: path_string(&manifest_path),
        })?;
        self.record_request(true);
        match self.read_response("trigger_lean_panic") {
            Ok(response) => Err(unexpected_response("trigger_lean_panic", &response)),
            Err(LeanWorkerError::ChildPanicOrAbort { exit }) => Ok(exit),
            Err(err) => Err(err),
        }
    }

    #[doc(hidden)]
    /// Emit synthetic worker data rows through the private protocol for row sink tests.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the sink panics,
    /// cancellation is observed, or protocol communication fails.
    pub fn __emit_test_rows(
        &mut self,
        streams: Vec<String>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        data: Option<&dyn LeanWorkerDataSink>,
    ) -> Result<u64, LeanWorkerError> {
        const OPERATION: &str = "emit_test_rows";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false)?;
        self.send_request(Request::EmitTestRows { streams })?;
        self.record_request(false);
        match self.read_response_with_events(
            OPERATION,
            None,
            cancellation,
            data.map(LeanWorkerDataSinkTarget::Value),
            None,
        )? {
            Response::RowsComplete { count } => Ok(count),
            other => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn open_worker_session(
        &mut self,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<(), LeanWorkerError> {
        const OPERATION: &str = "open_worker_session";
        check_cancelled(OPERATION, cancellation)?;
        let before_restarts = self.stats.restarts;
        self.prepare_request(true)?;
        let import_started = Instant::now();
        let mode = match config.mode() {
            LeanWorkerSessionMode::Capability {
                package,
                lib_name,
                manifest_path,
            } => HostSessionMode::Capability {
                package: package.clone(),
                lib_name: lib_name.clone(),
                manifest_path: manifest_path.as_ref().map(|path| path_string(path)),
            },
            LeanWorkerSessionMode::ShimsOnly => HostSessionMode::ShimsOnly,
        };
        self.send_request(Request::OpenHostSession {
            project_root: config.project_root_string(),
            mode,
            imports: config.imports().to_vec(),
            import_profile: config.import_profile(),
        })?;
        self.record_request(true);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::HostSessionOpened { import_stats } => {
                let session_open_import_elapsed = import_started.elapsed();
                self.stats.last_session_open_import_elapsed = Some(session_open_import_elapsed);
                if self.stats.restarts > before_restarts
                    && let Some(timing) = self.stats.last_replacement_timing.as_mut()
                {
                    timing.session_open_import = session_open_import_elapsed;
                }
                self.stats.last_import_stats = Some(import_stats);
                Ok(())
            }
            other => Err(unexpected_response(OPERATION, &other)),
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_elaborate(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerElabResult, LeanWorkerError> {
        self.round_trip(
            "worker_elaborate",
            Request::Elaborate {
                source: source.to_owned(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Elaboration { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_kernel_check(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerKernelResult, LeanWorkerError> {
        self.round_trip(
            "worker_kernel_check",
            Request::KernelCheck {
                source: source.to_owned(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::KernelCheck { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_declaration_kinds(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        self.round_trip(
            "worker_declaration_kinds",
            Request::DeclarationKinds {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Strings { values } => Ok(values),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_declaration_names(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        self.round_trip(
            "worker_declaration_names",
            Request::DeclarationNames {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Strings { values } => Ok(values),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_infer_type(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<LeanWorkerRendered>, LeanWorkerError> {
        self.round_trip(
            "worker_infer_type",
            Request::InferType {
                source: source.to_owned(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::MetaExpr { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_whnf(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<LeanWorkerRendered>, LeanWorkerError> {
        self.round_trip(
            "worker_whnf",
            Request::Whnf {
                source: source.to_owned(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::MetaExpr { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_is_def_eq(
        &mut self,
        lhs: &str,
        rhs: &str,
        transparency: LeanWorkerMetaTransparency,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<bool>, LeanWorkerError> {
        self.round_trip(
            "worker_is_def_eq",
            Request::IsDefEq {
                lhs: lhs.to_owned(),
                rhs: rhs.to_owned(),
                transparency,
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::MetaBool { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_describe(
        &mut self,
        name: &str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Option<LeanWorkerDeclarationRow>, LeanWorkerError> {
        self.round_trip(
            "worker_describe",
            Request::Describe { name: name.to_owned() },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Declaration { row } => Ok(row),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_search_declarations(
        &mut self,
        search: &LeanWorkerDeclarationSearch,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationSearchResult, LeanWorkerError> {
        self.round_trip(
            "worker_search_declarations",
            Request::SearchDeclarations { search: search.clone() },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationSearch { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_declaration_type(
        &mut self,
        name: &str,
        max_bytes: usize,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Option<LeanWorkerDeclarationType>, LeanWorkerError> {
        self.round_trip(
            "worker_declaration_type",
            Request::DeclarationType {
                name: name.to_owned(),
                max_bytes,
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationType { row } => Ok(row),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_inspect_declaration(
        &mut self,
        request: &LeanWorkerDeclarationInspectionRequest,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationInspectionResult, LeanWorkerError> {
        self.round_trip(
            "worker_inspect_declaration",
            Request::InspectDeclaration {
                request: request.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationInspection { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_attempt_proof(
        &mut self,
        request: &LeanWorkerProofAttemptRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerProofAttemptResult, LeanWorkerError> {
        self.round_trip(
            "worker_attempt_proof",
            Request::AttemptProof {
                request: request.clone(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ProofAttempt { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_verify_declaration(
        &mut self,
        request: &LeanWorkerDeclarationVerificationRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationResult, LeanWorkerError> {
        const OPERATION: &str = "worker_verify_declaration";
        let outcome = self.round_trip(
            OPERATION,
            Request::VerifyDeclaration {
                request: request.clone(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationVerification { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        );
        match outcome {
            Ok(result) => Ok(result),
            // A read-only verification must never surface a child abort as a hard
            // error: the worker's own (best-effort) screen could not prevent a
            // residual metavariable panic, so the supervisor respawns and reports
            // the honest degraded verdict. Verification is monotone, so relabeling
            // a non-result to `BudgetExceeded` never downgrades an `Accepted`.
            Err(err) => {
                self.recover_child_abort(OPERATION, err)?;
                Ok(LeanWorkerDeclarationVerificationResult::Ok {
                    verification_status: LeanWorkerDeclarationVerificationStatus::BudgetExceeded,
                    facts: Box::new(LeanWorkerDeclarationVerificationFacts::unavailable()),
                    imports: Vec::new(),
                })
            }
        }
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "filter is cheap to clone, passed by value matches caller shape"
    )]
    pub(crate) fn worker_list_declarations_strings(
        &mut self,
        filter: LeanWorkerDeclarationFilter,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        const OPERATION: &str = "worker_list_declarations_strings";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false)?;
        self.send_request(Request::ListDeclarationsStrings {
            filter,
            progress: progress.is_some(),
        })?;
        self.record_request(false);
        let collector = DeclarationNameCollector::default();
        let response = self.read_response_with_events(
            OPERATION,
            progress,
            cancellation,
            Some(LeanWorkerDataSinkTarget::Raw(&collector)),
            None,
        )?;
        if let Some(message) = collector.decode_error.lock().ok().and_then(|guard| guard.clone()) {
            return Err(LeanWorkerError::Protocol { message });
        }
        match response {
            Response::RowsComplete { count } => {
                let names = collector.into_inner();
                let observed = u64::try_from(names.len()).unwrap_or(u64::MAX);
                if observed != count {
                    return Err(LeanWorkerError::Protocol {
                        message: format!(
                            "worker_list_declarations_strings: parent collected {observed} rows but child reported {count}"
                        ),
                    });
                }
                Ok(names)
            }
            other => Err(unexpected_response(OPERATION, &other)),
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_describe_bulk(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<LeanWorkerDeclarationRow>, LeanWorkerError> {
        self.round_trip(
            "worker_describe_bulk",
            Request::DescribeBulk {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationBulk { rows } => Ok(rows),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_process_module_query(
        &mut self,
        source: &str,
        query: LeanWorkerModuleQuery,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryOutcome, LeanWorkerError> {
        self.round_trip(
            "worker_process_module_query",
            Request::ProcessModuleQuery {
                source: source.to_owned(),
                query,
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ProcessModuleQuery { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_process_module_query_batch(
        &mut self,
        source: &str,
        selectors: &[LeanWorkerModuleQuerySelector],
        budgets: &LeanWorkerOutputBudgets,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryBatchOutcome, LeanWorkerError> {
        const OPERATION: &str = "worker_process_module_query_batch";
        let outcome = self.round_trip(
            OPERATION,
            Request::ProcessModuleQueryBatch {
                source: source.to_owned(),
                selectors: selectors.to_vec(),
                budgets: budgets.clone(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ProcessModuleQueryBatch { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        );
        match outcome {
            Ok(outcome) => Ok(outcome),
            // As with verification: a child abort during a read-only proof-state
            // batch becomes a per-selector degraded item, not a hard error.
            Err(err) => {
                let resource = err.resource_exhausted_facts().cloned().unwrap_or_else(|| {
                    worker_resource_facts(
                        "worker_child_abort",
                        true,
                        Some(OPERATION),
                        &self.stats,
                        self.stats.last_rss_kib,
                        None,
                        None,
                    )
                });
                self.recover_child_abort(OPERATION, err)?;
                Ok(degraded_query_batch_outcome(selectors, resource))
            }
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_clear_module_snapshot_cache(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerError> {
        self.round_trip(
            "worker_clear_module_snapshot_cache",
            Request::ClearModuleSnapshotCache,
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ModuleSnapshotCacheCleared { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    pub(crate) fn worker_run_data_stream(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        rows: &dyn LeanWorkerDataSink,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerStreamSummary, LeanWorkerError> {
        self.worker_run_data_stream_with_sink(
            export,
            request,
            LeanWorkerDataSinkTarget::Value(rows),
            diagnostics,
            cancellation,
            progress,
        )
    }

    pub(crate) fn worker_run_data_stream_raw(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        rows: &dyn LeanWorkerRawDataSink,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerStreamSummary, LeanWorkerError> {
        self.worker_run_data_stream_with_sink(
            export,
            request,
            LeanWorkerDataSinkTarget::Raw(rows),
            diagnostics,
            cancellation,
            progress,
        )
    }

    fn worker_run_data_stream_with_sink(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        rows: LeanWorkerDataSinkTarget<'_>,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerStreamSummary, LeanWorkerError> {
        const OPERATION: &str = "worker_run_data_stream";
        check_cancelled(OPERATION, cancellation)?;
        let request_json = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker data-stream request JSON encode failed: {err}"),
        })?;
        self.prepare_request(false)?;
        self.send_request(Request::RunDataStream {
            export: export.to_owned(),
            request_json,
            progress: progress.is_some(),
        })?;
        self.record_request(false);
        self.stats.stream_requests = self.stats.stream_requests.saturating_add(1);
        match self.read_response_with_events(OPERATION, progress, cancellation, Some(rows), diagnostics)? {
            Response::StreamComplete { summary } => Ok(summary.into()),
            Response::StreamExportFailed { status_byte } => {
                Err(LeanWorkerError::StreamExportFailed { status: status_byte })
            }
            Response::StreamCallbackFailed {
                status_byte,
                description,
            } => Err(LeanWorkerError::StreamCallbackFailed {
                status: status_byte,
                description,
            }),
            Response::StreamRowMalformed { message } => Err(LeanWorkerError::StreamRowMalformed { message }),
            other => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_capability_metadata(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerCapabilityMetadata, LeanWorkerError> {
        let request_json = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability metadata request JSON encode failed: {err}"),
        })?;
        self.round_trip(
            "worker_capability_metadata",
            Request::CapabilityMetadata {
                export: export.to_owned(),
                request_json,
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::CapabilityMetadata { metadata } => Ok(metadata),
                Response::CapabilityMetadataMalformed { message } => {
                    Err(LeanWorkerError::CapabilityMetadataMalformed { message })
                }
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_capability_doctor(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDoctorReport, LeanWorkerError> {
        let request_json = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability doctor request JSON encode failed: {err}"),
        })?;
        self.round_trip(
            "worker_capability_doctor",
            Request::CapabilityDoctor {
                export: export.to_owned(),
                request_json,
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::CapabilityDoctor { report } => Ok(report),
                Response::CapabilityDoctorMalformed { message } => {
                    Err(LeanWorkerError::CapabilityDoctorMalformed { message })
                }
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_json_command(
        &mut self,
        export: &str,
        request_json: String,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<String, LeanWorkerError> {
        self.round_trip(
            "worker_json_command",
            Request::JsonCommand {
                export: export.to_owned(),
                request_json,
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::JsonCommand { response_json } => Ok(response_json),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    fn send_request(&mut self, request: Request) -> Result<(), LeanWorkerError> {
        self.ensure_running()?;
        self.write_request(request)
    }

    fn write_request(&mut self, request: Request) -> Result<(), LeanWorkerError> {
        let max_frame_bytes = self.config.max_frame_bytes;
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.dead_error());
        };
        write_frame(stdin, Message::Request(request), max_frame_bytes).map_err(|err| LeanWorkerError::Protocol {
            message: err.to_string(),
        })
    }

    fn begin_in_flight(&mut self, operation: &'static str) -> InFlightRequest {
        let request = InFlightRequest {
            id: self.next_request_id,
            operation,
            generation: self.generation,
        };
        self.next_request_id = self.next_request_id.next();
        self.state = WorkerSupervisorState::Busy {
            request: request.clone(),
        };
        request
    }

    fn mark_current_request_streaming(&mut self) {
        match &self.state {
            WorkerSupervisorState::Busy { request } | WorkerSupervisorState::Streaming { request } => {
                self.state = WorkerSupervisorState::Streaming {
                    request: request.clone(),
                };
            }
            _ => {}
        }
    }

    fn finish_in_flight(&mut self) {
        if matches!(
            self.state,
            WorkerSupervisorState::Busy { .. } | WorkerSupervisorState::Streaming { .. }
        ) {
            self.state = WorkerSupervisorState::Idle {
                generation: self.generation,
            };
        }
    }

    fn prepare_request(&mut self, import_like: bool) -> Result<(), LeanWorkerError> {
        self.ensure_running()?;
        if import_like {
            self.stats.import_like_admission_attempts = self.stats.import_like_admission_attempts.saturating_add(1);
            self.stats.last_import_like_rss_before_admission_kib = self.child_rss_kib();
        }

        if let Some(limit) = self.config.restart_policy.max_requests
            && self.requests_since_restart >= limit
        {
            self.restart_with_reason(LeanWorkerRestartReason::MaxRequests { limit })?;
            if import_like {
                self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
            }
            return Ok(());
        }

        if import_like
            && let Some(limit) = self.config.restart_policy.max_imports
            && self.imports_since_restart >= limit
        {
            self.restart_with_reason(LeanWorkerRestartReason::MaxImports { limit })?;
            self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
            return Ok(());
        }

        if let Some(limit_kib) = self.config.restart_policy.max_rss_kib {
            match self.child_rss_kib() {
                Some(current_kib) if current_kib >= limit_kib => {
                    self.stats.last_rss_kib = Some(current_kib);
                    self.restart_with_reason(LeanWorkerRestartReason::RssCeiling {
                        current_kib,
                        limit_kib,
                        last_import_stats: self.stats.last_import_stats.clone(),
                    })?;
                    if import_like {
                        self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
                    }
                    return Ok(());
                }
                Some(current_kib) => {
                    self.stats.last_rss_kib = Some(current_kib);
                }
                None => {
                    self.stats.rss_samples_unavailable = self.stats.rss_samples_unavailable.saturating_add(1);
                }
            }
        }

        if let Some(limit) = self.config.restart_policy.idle_restart_after {
            let idle_for = self.last_activity.elapsed();
            if idle_for >= limit {
                self.restart_with_reason(LeanWorkerRestartReason::Idle { idle_for, limit })?;
                if import_like {
                    self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
                }
                return Ok(());
            }
        }

        if import_like {
            self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
        }
        Ok(())
    }

    fn record_request(&mut self, import_like: bool) {
        self.stats.requests = self.stats.requests.saturating_add(1);
        self.requests_since_restart = self.requests_since_restart.saturating_add(1);
        if import_like {
            self.stats.imports = self.stats.imports.saturating_add(1);
            self.imports_since_restart = self.imports_since_restart.saturating_add(1);
        }
        self.last_activity = Instant::now();
    }

    fn restart_with_reason(&mut self, reason: LeanWorkerRestartReason) -> Result<(), LeanWorkerError> {
        self.restart_with_reason_before_spawn(reason, || {})
    }

    fn restart_with_reason_before_spawn(
        &mut self,
        reason: LeanWorkerRestartReason,
        before_spawn: impl FnOnce(),
    ) -> Result<(), LeanWorkerError> {
        let config = self.config.clone();
        let replacement_started = Instant::now();
        self.stats.replacement_attempts = self.stats.replacement_attempts.saturating_add(1);
        let stop_intent = if matches!(
            &reason,
            LeanWorkerRestartReason::Explicit
                | LeanWorkerRestartReason::MaxRequests { .. }
                | LeanWorkerRestartReason::MaxImports { .. }
                | LeanWorkerRestartReason::RssCeiling { .. }
                | LeanWorkerRestartReason::Idle { .. }
        ) {
            ShutdownIntent::Graceful
        } else {
            ShutdownIntent::KillOnly
        };
        if let Err(err) = self.shutdown_child(stop_intent) {
            self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
            self.stats.last_replacement_skipped_reason = Some("stop_failed".to_owned());
            return Err(err);
        }
        before_spawn();
        if let Err(err) = self.admit_restart(replacement_started) {
            self.stats.replacement_budget_skipped = self.stats.replacement_budget_skipped.saturating_add(1);
            self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
            self.stats.last_replacement_skipped_reason = Some("restart_limit_exceeded".to_owned());
            return Err(err);
        }
        self.stats.replacement_budget_admitted = self.stats.replacement_budget_admitted.saturating_add(1);
        let next_generation = self.generation.next();
        self.stats.record_restart(reason);
        self.requests_since_restart = 0;
        self.imports_since_restart = 0;
        let reason = self
            .stats
            .last_restart_reason
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), |reason| reason.stable_cause().to_owned());
        let mut next = match Self::spawn(&config) {
            Ok(next) => next,
            Err(err) => {
                self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
                self.stats.last_replacement_skipped_reason = Some("spawn_failed".to_owned());
                return Err(err);
            }
        };
        let next_request_id = self.next_request_id;
        let spawn_handshake = next.stats.last_spawn_handshake_elapsed.unwrap_or_default();
        let mut stats = self.stats.clone();
        stats.replacement_successes = stats.replacement_successes.saturating_add(1);
        stats.last_replacement_skipped_reason = None;
        stats.last_spawn_handshake_elapsed = Some(spawn_handshake);
        stats.last_replacement_timing = Some(LeanWorkerReplacementTiming {
            spawn_handshake,
            capability_load: stats.last_capability_load_elapsed.unwrap_or_default(),
            session_open_import: Duration::ZERO,
            first_command: stats.last_first_command_elapsed,
            warm_command: stats.last_warm_command_elapsed,
            replacement_total: replacement_started.elapsed(),
            replacement_reason: reason,
            replacement_budget_status: "synchronous-no-overlap".to_owned(),
        });
        next.stats = stats;
        next.generation = next_generation;
        next.next_request_id = next_request_id;
        next.state = WorkerSupervisorState::Idle {
            generation: next_generation,
        };
        next.restart_window.clone_from(&self.restart_window);
        next.last_activity = Instant::now();
        *self = next;
        Ok(())
    }

    fn admit_restart(&mut self, now: Instant) -> Result<(), LeanWorkerError> {
        let limit = self.config.restart_policy.restart_intensity;
        while self
            .restart_window
            .front()
            .is_some_and(|instant| now.saturating_duration_since(*instant) >= limit.window)
        {
            let _ = self.restart_window.pop_front();
        }
        let restarts = u64::try_from(self.restart_window.len()).unwrap_or(u64::MAX);
        if restarts >= limit.max_restarts {
            self.state = WorkerSupervisorState::RestartExhausted {
                generation: self.generation,
            };
            return Err(LeanWorkerError::RestartLimitExceeded {
                restarts,
                window: limit.window,
            });
        }
        self.restart_window.push_back(now);
        Ok(())
    }

    /// Absorb a child abort raised during a read-only request: respawn a fresh
    /// child so subsequent requests succeed, then return `Ok(())` so the caller
    /// can synthesise a degraded verdict. Any non-abort error (or a respawn
    /// failure) propagates unchanged.
    fn recover_child_abort(&mut self, operation: &'static str, err: LeanWorkerError) -> Result<(), LeanWorkerError> {
        if matches!(err, LeanWorkerError::ChildPanicOrAbort { .. }) {
            self.restart_with_reason(LeanWorkerRestartReason::ChildAbort { operation })
        } else {
            Err(err)
        }
    }

    fn hard_rss_limit_exceeded(&mut self) -> Option<(u64, u64)> {
        let limit_kib = self.config.rss_hard_limit_kib?;
        match self.child_rss_kib() {
            Some(current_kib) if current_kib >= limit_kib => {
                self.stats.last_rss_kib = Some(current_kib);
                Some((current_kib, limit_kib))
            }
            Some(current_kib) => {
                self.stats.last_rss_kib = Some(current_kib);
                None
            }
            None => {
                self.stats.rss_samples_unavailable = self.stats.rss_samples_unavailable.saturating_add(1);
                None
            }
        }
    }

    fn read_response(&mut self, operation: &'static str) -> Result<Response, LeanWorkerError> {
        self.read_response_with_events(operation, None, None, None, None)
    }

    fn read_response_with_progress(
        &mut self,
        operation: &'static str,
        progress: Option<&dyn LeanWorkerProgressSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<Response, LeanWorkerError> {
        self.read_response_with_events(operation, progress, cancellation, None, None)
    }

    /// Run one Request/Response round-trip and project the response into a
    /// typed value.
    ///
    /// Centralizes the cancel-check → send → record → read sequence so every
    /// `worker_*` helper above delegates here instead of repeating five
    /// identical lines plus a 22-variant wildcard arm. The `extract` closure
    /// receives the response together with the operation name; it returns the
    /// typed value, the typed error variant the protocol expects (e.g.,
    /// `*Malformed`), or `unexpected_response(operation, &other)` for any
    /// wire variant the operation never expects.
    fn round_trip<R>(
        &mut self,
        operation: &'static str,
        request: Request,
        import_like: bool,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
        extract: impl FnOnce(Response, &'static str) -> Result<R, LeanWorkerError>,
    ) -> Result<R, LeanWorkerError> {
        check_cancelled(operation, cancellation)?;
        self.prepare_request(import_like)?;
        self.send_request(request)?;
        self.record_request(import_like);
        let response = self.read_response_with_progress(operation, progress, cancellation)?;
        extract(response, operation)
    }

    fn read_response_with_events(
        &mut self,
        operation: &'static str,
        progress: Option<&dyn LeanWorkerProgressSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        data: Option<LeanWorkerDataSinkTarget<'_>>,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
    ) -> Result<Response, LeanWorkerError> {
        let started = Instant::now();
        let timeout = self.config.request_timeout;
        let deadline = started.checked_add(timeout);
        let streaming = data.is_some();
        let mut request_backpressure_waits = 0_u64;
        let stdout = self.stdout.take().ok_or_else(|| self.dead_error())?;
        let max_frame_bytes = self.config.max_frame_bytes;
        let generation = self.generation;
        let (sender, receiver) = mpsc::sync_channel(WORKER_EVENT_BUFFER_CAPACITY);
        let reader = thread::spawn(move || read_request_messages(stdout, sender, max_frame_bytes, generation));
        self.begin_in_flight(operation);

        loop {
            if let Some((current_kib, limit_kib)) = self.hard_rss_limit_exceeded() {
                if streaming {
                    self.record_stream_failure(started, request_backpressure_waits);
                }
                let last_import_stats = self.stats.last_import_stats.clone();
                if let Err(err) = self.restart_with_reason_before_spawn(
                    LeanWorkerRestartReason::RssHardLimit {
                        operation,
                        current_kib,
                        limit_kib,
                        last_import_stats: last_import_stats.clone(),
                    },
                    || {
                        drop(receiver);
                        drop(reader.join());
                    },
                ) {
                    self.finish_in_flight();
                    return Err(err);
                }
                self.finish_in_flight();
                return Err(LeanWorkerError::RssHardLimitExceeded {
                    operation,
                    current_kib,
                    limit_kib,
                    last_import_stats: last_import_stats.map(Box::new),
                    resource: Box::new(worker_resource_facts(
                        "worker_rss_hard_limit",
                        true,
                        Some(operation),
                        &self.stats,
                        Some(current_kib),
                        Some(limit_kib),
                        None,
                    )),
                });
            }
            let event = match deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now())) {
                Some(remaining) if remaining.is_zero() => {
                    if streaming {
                        self.record_stream_failure(started, request_backpressure_waits);
                    }
                    if let Err(err) = self.restart_with_reason_before_spawn(
                        LeanWorkerRestartReason::RequestTimeout {
                            operation,
                            duration: timeout,
                        },
                        || {
                            drop(receiver);
                            drop(reader.join());
                        },
                    ) {
                        self.finish_in_flight();
                        return Err(err);
                    }
                    self.finish_in_flight();
                    return Err(LeanWorkerError::Timeout {
                        operation,
                        duration: timeout,
                        resource: Box::new(worker_resource_facts(
                            "worker_timeout",
                            true,
                            Some(operation),
                            &self.stats,
                            self.stats.last_rss_kib,
                            None,
                            Some(timeout),
                        )),
                    });
                }
                Some(remaining) => {
                    let hard_watch_enabled = self.config.rss_hard_limit_kib.is_some();
                    let wait_for = if hard_watch_enabled {
                        remaining.min(self.config.rss_sample_interval)
                    } else {
                        remaining
                    };
                    match receiver.recv_timeout(wait_for) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) if hard_watch_enabled && wait_for < remaining => {
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if streaming {
                                self.record_stream_failure(started, request_backpressure_waits);
                            }
                            if let Err(err) = self.restart_with_reason_before_spawn(
                                LeanWorkerRestartReason::RequestTimeout {
                                    operation,
                                    duration: timeout,
                                },
                                || {
                                    drop(receiver);
                                    drop(reader.join());
                                },
                            ) {
                                self.finish_in_flight();
                                return Err(err);
                            }
                            self.finish_in_flight();
                            return Err(LeanWorkerError::Timeout {
                                operation,
                                duration: timeout,
                                resource: Box::new(worker_resource_facts(
                                    "worker_timeout",
                                    true,
                                    Some(operation),
                                    &self.stats,
                                    self.stats.last_rss_kib,
                                    None,
                                    Some(timeout),
                                )),
                            });
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            self.finish_in_flight();
                            drop(reader.join());
                            return Err(LeanWorkerError::Protocol {
                                message: "worker response reader exited without a terminal response".to_owned(),
                            });
                        }
                    }
                }
                None => match receiver.recv() {
                    Ok(event) => event,
                    Err(_err) => {
                        self.finish_in_flight();
                        drop(reader.join());
                        return Err(LeanWorkerError::Protocol {
                            message: "worker response reader exited without a terminal response".to_owned(),
                        });
                    }
                },
            };
            if event.generation() != self.generation {
                let actual_generation = event.generation();
                self.finish_in_flight();
                drop(reader.join());
                return Err(stale_worker_output_error(operation, self.generation, actual_generation));
            }
            request_backpressure_waits = request_backpressure_waits.saturating_add(event.backpressure_waits());
            self.stats.backpressure_waits = self.stats.backpressure_waits.saturating_add(event.backpressure_waits());

            let message = match event {
                RequestReaderEvent::Message { message, .. } => message,
                RequestReaderEvent::Terminal { message, stdout, .. } => {
                    self.stdout = Some(stdout);
                    match message {
                        Message::Response(Response::Error { code, message }) => {
                            self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                            drop(reader.join());
                            return Err(LeanWorkerError::Worker { code, message });
                        }
                        Message::Response(response) => {
                            let response = self.terminalize_request_response(
                                response,
                                streaming,
                                started,
                                request_backpressure_waits,
                            );
                            drop(reader.join());
                            return Ok(response);
                        }
                        other => {
                            self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                            drop(reader.join());
                            return Err(LeanWorkerError::Protocol {
                                message: format!("worker sent unexpected {operation} message: {other:?}"),
                            });
                        }
                    }
                }
                RequestReaderEvent::ReadError { message, eof, .. } => {
                    drop(reader.join());
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    return if eof {
                        Err(self.record_exit_error())
                    } else {
                        Err(LeanWorkerError::Protocol { message })
                    };
                }
            };

            match message {
                Message::ProgressTick(tick) => {
                    self.mark_current_request_streaming();
                    if let Err(err) =
                        report_parent_progress(progress, elapsed_event(tick.phase, tick.current, tick.total, started))
                    {
                        self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                        return Err(err);
                    }
                    if cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
                        if streaming {
                            self.record_stream_failure(started, request_backpressure_waits);
                        }
                        if let Err(err) = self.restart_with_reason_before_spawn(
                            LeanWorkerRestartReason::Cancelled { operation },
                            || {
                                drop(receiver);
                                drop(reader.join());
                            },
                        ) {
                            self.finish_in_flight();
                            return Err(err);
                        }
                        self.finish_in_flight();
                        return Err(LeanWorkerError::Cancelled {
                            operation,
                            resource: Box::new(worker_resource_facts(
                                "worker_cancelled",
                                true,
                                Some(operation),
                                &self.stats,
                                self.stats.last_rss_kib,
                                None,
                                None,
                            )),
                        });
                    }
                }
                Message::DataRow(row) => {
                    self.mark_current_request_streaming();
                    let payload_bytes = row.payload.get().len() as u64;
                    if let Err(err) = report_parent_data_row(data, row) {
                        self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                        return Err(err);
                    }
                    self.stats.data_rows_delivered = self.stats.data_rows_delivered.saturating_add(1);
                    self.stats.data_row_payload_bytes = self.stats.data_row_payload_bytes.saturating_add(payload_bytes);
                    if cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
                        if streaming {
                            self.record_stream_failure(started, request_backpressure_waits);
                        }
                        if let Err(err) = self.restart_with_reason_before_spawn(
                            LeanWorkerRestartReason::Cancelled { operation },
                            || {
                                drop(receiver);
                                drop(reader.join());
                            },
                        ) {
                            self.finish_in_flight();
                            return Err(err);
                        }
                        self.finish_in_flight();
                        return Err(LeanWorkerError::Cancelled {
                            operation,
                            resource: Box::new(worker_resource_facts(
                                "worker_cancelled",
                                true,
                                Some(operation),
                                &self.stats,
                                self.stats.last_rss_kib,
                                None,
                                None,
                            )),
                        });
                    }
                }
                Message::Diagnostic(diagnostic) => {
                    self.mark_current_request_streaming();
                    if let Err(err) = report_parent_diagnostic(diagnostics, diagnostic.into()) {
                        self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                        return Err(err);
                    }
                }
                Message::Response(response) => {
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    return Err(unexpected_response(operation, &response));
                }
                other => {
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    return Err(LeanWorkerError::Protocol {
                        message: format!("worker sent unexpected {operation} message: {other:?}"),
                    });
                }
            }
        }
    }

    fn ensure_running(&mut self) -> Result<(), LeanWorkerError> {
        if self.state.rejects_new_requests() {
            return Err(LeanWorkerError::ShutdownInProgress {
                operation: self.state.current_operation().unwrap_or("worker_request"),
            });
        }
        match self.status()? {
            LeanWorkerStatus::Running => Ok(()),
            LeanWorkerStatus::Exited(exit) if exit.success => Err(LeanWorkerError::ChildExited { exit }),
            LeanWorkerStatus::Exited(exit) => Err(LeanWorkerError::ChildPanicOrAbort { exit }),
        }
    }

    fn terminalize_request_response(
        &mut self,
        response: Response,
        streaming: bool,
        started: Instant,
        backpressure_waits: u64,
    ) -> Response {
        if streaming {
            if matches!(response, Response::StreamComplete { .. }) {
                self.record_stream_success(started);
            } else {
                self.record_stream_failure(started, backpressure_waits);
            }
        }
        self.finish_in_flight();
        response
    }

    fn terminalize_request_failure(&mut self, streaming: bool, started: Instant, backpressure_waits: u64) {
        if streaming {
            self.record_stream_failure(started, backpressure_waits);
        }
        self.finish_in_flight();
    }

    fn record_stream_success(&mut self, started: Instant) {
        self.stats.stream_successes = self.stats.stream_successes.saturating_add(1);
        self.stats.stream_elapsed = self.stats.stream_elapsed.saturating_add(started.elapsed());
    }

    fn record_stream_failure(&mut self, started: Instant, backpressure_waits: u64) {
        self.stats.stream_failures = self.stats.stream_failures.saturating_add(1);
        self.stats.stream_elapsed = self.stats.stream_elapsed.saturating_add(started.elapsed());
        if backpressure_waits > 0 {
            self.stats.backpressure_failures = self.stats.backpressure_failures.saturating_add(1);
        }
    }

    fn wait_for_exit(&mut self) -> Result<LeanWorkerExit, LeanWorkerError> {
        let Some(child) = self.child.as_mut() else {
            return Err(self.dead_error());
        };
        self.state = WorkerSupervisorState::Reaping {
            generation: self.generation,
        };
        let status = child.wait().map_err(|source| LeanWorkerError::Wait { source })?;
        Ok(self.finalize_child_exit(status))
    }

    fn wait_for_exit_bounded(
        &mut self,
        operation: &'static str,
        timeout: Duration,
    ) -> Result<(LeanWorkerExit, Duration), LeanWorkerError> {
        let started = Instant::now();
        loop {
            let Some(child) = self.child.as_mut() else {
                return Err(self.dead_error());
            };
            self.state = WorkerSupervisorState::Reaping {
                generation: self.generation,
            };
            match child.try_wait().map_err(|source| LeanWorkerError::Wait { source })? {
                Some(status) => return Ok((self.finalize_child_exit(status), started.elapsed())),
                None if started.elapsed() >= timeout => {
                    return Err(LeanWorkerError::WaitTimeout {
                        operation,
                        duration: timeout,
                    });
                }
                None => thread::sleep(Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed()))),
            }
        }
    }

    fn finalize_child_exit(&mut self, status: ExitStatus) -> LeanWorkerExit {
        let diagnostics = self.read_stderr();
        let exit = LeanWorkerExit::from_status(status, diagnostics);
        self.last_exit = Some(exit.clone());
        self.child = None;
        self.stdin = None;
        self.stdout = None;
        self.finish_in_flight();
        self.state = WorkerSupervisorState::Exited {
            generation: self.generation,
        };
        self.stats.exits = self.stats.exits.saturating_add(1);
        exit
    }

    fn record_exit_error(&mut self) -> LeanWorkerError {
        self.state = WorkerSupervisorState::Crashed {
            generation: self.generation,
        };
        match self.wait_for_exit() {
            Ok(exit) if exit.success => LeanWorkerError::ChildExited { exit },
            Ok(exit) => LeanWorkerError::ChildPanicOrAbort { exit },
            Err(err) => err,
        }
    }

    fn shutdown_child(&mut self, intent: ShutdownIntent) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        let started = Instant::now();
        let graceful_timeout = self.config.shutdown_timeout;
        match self.status()? {
            LeanWorkerStatus::Exited(exit) => {
                return Ok(LeanWorkerShutdownReport {
                    outcome: LeanWorkerShutdownOutcome::AlreadyExited,
                    exit,
                    graceful_timeout,
                    elapsed: started.elapsed(),
                    kill_elapsed: None,
                    wait_elapsed: Duration::ZERO,
                });
            }
            LeanWorkerStatus::Running => {}
        }

        self.state = if intent == ShutdownIntent::Graceful {
            WorkerSupervisorState::Stopping {
                generation: self.generation,
            }
        } else {
            WorkerSupervisorState::Killing {
                generation: self.generation,
            }
        };
        self.finish_in_flight();

        if intent == ShutdownIntent::Graceful {
            match self.write_request(Request::Terminate) {
                Ok(()) => return self.wait_for_graceful_shutdown(started, graceful_timeout),
                Err(LeanWorkerError::Protocol { .. } | LeanWorkerError::Worker { .. }) => {
                    return self.kill_and_report(
                        started,
                        graceful_timeout,
                        LeanWorkerShutdownOutcome::GracefulProtocolFailedKilled,
                    );
                }
                Err(err) => return Err(err),
            }
        }

        self.kill_and_report(started, graceful_timeout, LeanWorkerShutdownOutcome::KillOnly)
    }

    fn wait_for_graceful_shutdown(
        &mut self,
        started: Instant,
        graceful_timeout: Duration,
    ) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        let grace_started = Instant::now();
        let stdout = self.stdout.take().ok_or_else(|| self.dead_error())?;
        let max_frame_bytes = self.config.max_frame_bytes;
        let generation = self.generation;
        let (sender, receiver) = mpsc::sync_channel(WORKER_EVENT_BUFFER_CAPACITY);
        let reader = thread::spawn(move || read_request_messages(stdout, sender, max_frame_bytes, generation));

        loop {
            if let Some(child) = self.child.as_mut()
                && let Some(status) = child.try_wait().map_err(|source| LeanWorkerError::Wait { source })?
            {
                drop(receiver);
                drop(reader.join());
                let wait_elapsed = grace_started.elapsed();
                let exit = self.finalize_child_exit(status);
                return Ok(LeanWorkerShutdownReport {
                    outcome: LeanWorkerShutdownOutcome::Graceful,
                    exit,
                    graceful_timeout,
                    elapsed: started.elapsed(),
                    kill_elapsed: None,
                    wait_elapsed,
                });
            }

            let elapsed = grace_started.elapsed();
            if elapsed >= graceful_timeout {
                let kill_started = Instant::now();
                if let Some(child) = self.child.as_mut() {
                    child.kill().map_err(|source| LeanWorkerError::Kill { source })?;
                }
                drop(receiver);
                drop(reader.join());
                let (exit, wait_elapsed) =
                    self.wait_for_exit_bounded("kill_wait", LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT)?;
                return Ok(LeanWorkerShutdownReport {
                    outcome: LeanWorkerShutdownOutcome::GracefulTimedOutKilled,
                    exit,
                    graceful_timeout,
                    elapsed: started.elapsed(),
                    kill_elapsed: Some(kill_started.elapsed()),
                    wait_elapsed,
                });
            }

            let receive_timeout = Duration::from_millis(10).min(graceful_timeout.saturating_sub(elapsed));
            match receiver.recv_timeout(receive_timeout) {
                Ok(RequestReaderEvent::Terminal {
                    message: Message::Response(Response::Terminating),
                    stdout,
                    ..
                }) => {
                    drop(reader.join());
                    self.stdout = Some(stdout);
                    let remaining = graceful_timeout.saturating_sub(grace_started.elapsed());
                    match self.wait_for_exit_bounded("shutdown", remaining) {
                        Ok((exit, wait_elapsed)) => {
                            return Ok(LeanWorkerShutdownReport {
                                outcome: LeanWorkerShutdownOutcome::Graceful,
                                exit,
                                graceful_timeout,
                                elapsed: started.elapsed(),
                                kill_elapsed: None,
                                wait_elapsed,
                            });
                        }
                        Err(LeanWorkerError::WaitTimeout { .. }) => {
                            return self.kill_and_report(
                                started,
                                graceful_timeout,
                                LeanWorkerShutdownOutcome::GracefulTimedOutKilled,
                            );
                        }
                        Err(err) => return Err(err),
                    }
                }
                Ok(RequestReaderEvent::Terminal { stdout, .. }) => {
                    drop(reader.join());
                    self.stdout = Some(stdout);
                    return self.kill_and_report(
                        started,
                        graceful_timeout,
                        LeanWorkerShutdownOutcome::GracefulProtocolFailedKilled,
                    );
                }
                Ok(RequestReaderEvent::ReadError { eof: true, .. }) => {
                    drop(reader.join());
                    let remaining = graceful_timeout.saturating_sub(grace_started.elapsed());
                    match self.wait_for_exit_bounded("shutdown", remaining) {
                        Ok((exit, wait_elapsed)) => {
                            return Ok(LeanWorkerShutdownReport {
                                outcome: LeanWorkerShutdownOutcome::Graceful,
                                exit,
                                graceful_timeout,
                                elapsed: started.elapsed(),
                                kill_elapsed: None,
                                wait_elapsed,
                            });
                        }
                        Err(LeanWorkerError::WaitTimeout { .. }) => {
                            return self.kill_and_report(
                                started,
                                graceful_timeout,
                                LeanWorkerShutdownOutcome::GracefulTimedOutKilled,
                            );
                        }
                        Err(err) => return Err(err),
                    }
                }
                Ok(RequestReaderEvent::ReadError { .. }) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    drop(reader.join());
                    return self.kill_and_report(
                        started,
                        graceful_timeout,
                        LeanWorkerShutdownOutcome::GracefulProtocolFailedKilled,
                    );
                }
                Ok(RequestReaderEvent::Message { .. }) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn kill_and_report(
        &mut self,
        started: Instant,
        graceful_timeout: Duration,
        outcome: LeanWorkerShutdownOutcome,
    ) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        let kill_started = Instant::now();
        self.state = WorkerSupervisorState::Killing {
            generation: self.generation,
        };
        if let Some(child) = self.child.as_mut() {
            child.kill().map_err(|source| LeanWorkerError::Kill { source })?;
        }
        let (exit, wait_elapsed) = self.wait_for_exit_bounded("kill_wait", LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT)?;
        Ok(LeanWorkerShutdownReport {
            outcome,
            exit,
            graceful_timeout,
            elapsed: started.elapsed(),
            kill_elapsed: Some(kill_started.elapsed()),
            wait_elapsed,
        })
    }

    fn dead_error(&self) -> LeanWorkerError {
        let exit = self.last_exit.clone().unwrap_or_else(|| LeanWorkerExit {
            success: false,
            code: None,
            status: "worker is not running".to_owned(),
            diagnostics: String::new(),
        });
        if exit.success {
            LeanWorkerError::ChildExited { exit }
        } else {
            LeanWorkerError::ChildPanicOrAbort { exit }
        }
    }

    fn read_stderr(&mut self) -> String {
        let mut diagnostics = String::new();
        if let Some(mut pipe) = self.stderr.take() {
            drop(pipe.read_to_string(&mut diagnostics));
        }
        diagnostics
    }

    fn child_rss_kib(&mut self) -> Option<u64> {
        let child = self.child.as_mut()?;
        child_rss_kib(child.id())
    }
}

enum RequestReaderEvent {
    Message {
        generation: WorkerGeneration,
        message: Message,
        backpressure_waits: u64,
    },
    Terminal {
        generation: WorkerGeneration,
        message: Message,
        stdout: BufReader<ChildStdout>,
        backpressure_waits: u64,
    },
    ReadError {
        generation: WorkerGeneration,
        message: String,
        eof: bool,
        backpressure_waits: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownIntent {
    Graceful,
    KillOnly,
}

impl RequestReaderEvent {
    fn generation(&self) -> WorkerGeneration {
        match self {
            Self::Message { generation, .. }
            | Self::Terminal { generation, .. }
            | Self::ReadError { generation, .. } => *generation,
        }
    }

    fn backpressure_waits(&self) -> u64 {
        match self {
            Self::Message { backpressure_waits, .. }
            | Self::Terminal { backpressure_waits, .. }
            | Self::ReadError { backpressure_waits, .. } => *backpressure_waits,
        }
    }

    fn add_backpressure_wait(&mut self) {
        match self {
            Self::Message { backpressure_waits, .. }
            | Self::Terminal { backpressure_waits, .. }
            | Self::ReadError { backpressure_waits, .. } => {
                *backpressure_waits = backpressure_waits.saturating_add(1);
            }
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the request reader thread must own the sender"
)]
fn read_request_messages(
    mut stdout: BufReader<ChildStdout>,
    sender: mpsc::SyncSender<RequestReaderEvent>,
    max_frame_bytes: u32,
    generation: WorkerGeneration,
) {
    loop {
        match read_frame(&mut stdout, max_frame_bytes) {
            Ok(frame) if matches!(frame.message, Message::Response(_)) => {
                let _ = send_reader_event(
                    &sender,
                    RequestReaderEvent::Terminal {
                        generation,
                        message: frame.message,
                        stdout,
                        backpressure_waits: 0,
                    },
                );
                return;
            }
            Ok(frame) => {
                if send_reader_event(
                    &sender,
                    RequestReaderEvent::Message {
                        generation,
                        message: frame.message,
                        backpressure_waits: 0,
                    },
                )
                .is_err()
                {
                    return;
                }
            }
            Err(err) => {
                let _ = send_reader_event(
                    &sender,
                    RequestReaderEvent::ReadError {
                        generation,
                        message: err.to_string(),
                        eof: err.is_eof(),
                        backpressure_waits: 0,
                    },
                );
                return;
            }
        }
    }
}

fn send_reader_event(sender: &mpsc::SyncSender<RequestReaderEvent>, event: RequestReaderEvent) -> Result<(), ()> {
    match sender.try_send(event) {
        Ok(()) => Ok(()),
        Err(mpsc::TrySendError::Full(mut event)) => {
            event.add_backpressure_wait();
            sender.send(event).map_err(|_| ())
        }
        Err(mpsc::TrySendError::Disconnected(_event)) => Err(()),
    }
}

impl Drop for LeanWorker {
    fn drop(&mut self) {
        drop(self.shutdown_child(ShutdownIntent::Graceful));
    }
}

#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "Message is #[non_exhaustive] across the lean-rs-worker-protocol crate boundary; the wildcard arm uniformly rejects any non-handshake frame"
)]
fn expect_handshake(
    stdout: &mut BufReader<ChildStdout>,
    max_frame_bytes: u32,
) -> Result<LeanWorkerRuntimeMetadata, LeanWorkerError> {
    let frame = read_frame(stdout, max_frame_bytes).map_err(|err| {
        if err.is_eof() {
            LeanWorkerError::Handshake {
                message: "child closed stdout before handshake".to_owned(),
            }
        } else {
            LeanWorkerError::Handshake {
                message: err.to_string(),
            }
        }
    })?;
    match frame.message {
        Message::Handshake {
            worker_version,
            protocol_version,
        } if protocol_version == lean_rs_worker_protocol::protocol::PROTOCOL_VERSION => Ok(LeanWorkerRuntimeMetadata {
            worker_version,
            protocol_version,
            lean_version: None,
        }),
        other => Err(LeanWorkerError::Handshake {
            message: format!("unexpected handshake frame: {other:?}"),
        }),
    }
}

fn wait_with_stderr(child: &mut Child, stderr: Option<ChildStderr>) -> Result<LeanWorkerExit, LeanWorkerError> {
    let status = child.wait().map_err(|source| LeanWorkerError::Wait { source })?;
    let mut diagnostics = String::new();
    if let Some(mut pipe) = stderr {
        drop(pipe.read_to_string(&mut diagnostics));
    }
    Ok(LeanWorkerExit::from_status(status, diagnostics))
}

/// Cap so a single `Display` line cannot blow up downstream log/telemetry
/// pipelines that capture `err.to_string()` (`tracing::error!("{err}")`,
/// JSON log shippers). The full `exit.diagnostics` text is still available on
/// the field; this only limits what crosses the `Display` surface.
const DISPLAY_DIAGNOSTICS_MAX_BYTES: usize = 4 * 1024;

fn write_exit(f: &mut fmt::Formatter<'_>, prefix: &str, exit: &LeanWorkerExit) -> fmt::Result {
    let tail = exit.diagnostics.trim();
    if tail.is_empty() {
        write!(f, "{prefix} with {}", exit.status)
    } else {
        let truncated = truncate_for_display(tail, DISPLAY_DIAGNOSTICS_MAX_BYTES);
        write!(f, "{prefix} with {}: {truncated}", exit.status)
    }
}

fn import_stats_diagnostic(stats: Option<&LeanWorkerImportStats>) -> String {
    let Some(stats) = stats else {
        return String::from("last_import_stats=unavailable");
    };
    format!(
        "last_import_stats=available import_profile=level:{} import_all:{} load_exts:{} direct_import_count={} direct_imports={} effective_modules={} compacted_regions={} memory_mapped_regions={} compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} imported_constants={} extension_entries={}",
        stats.import_level,
        stats.import_all,
        stats.load_exts,
        stats.direct_import_names.len(),
        stats.direct_import_names.join(","),
        stats.effective_module_count,
        stats.compacted_region_count,
        stats.memory_mapped_region_count,
        stats.compacted_region_bytes,
        stats.memory_mapped_region_bytes,
        stats.non_memory_mapped_region_bytes,
        stats.imported_constant_count,
        stats.total_imported_extension_entries
    )
}

/// Truncate `text` to at most `max_bytes` bytes for human display, appending
/// `… (N bytes truncated)` when bytes are dropped.
///
/// The cut respects two invariants that matter for the actual log streams we
/// surface (Lake/`elan` stderr): the cut lands on a UTF-8 char boundary, and
/// it never lands inside an unterminated ANSI CSI escape (`ESC '[' …
/// 0x40..=0x7e`). If the naive cut would bisect a CSI, we back up to the byte
/// immediately before the `ESC`. Half-open escapes corrupt downstream terminals
/// and log viewers; the byte cost of backing up is negligible compared to the
/// 4 KiB cap.
fn truncate_for_display(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }

    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }

    let bytes = text.as_bytes();
    if let Some(esc_off) = bytes
        .get(..cut)
        .and_then(|prefix| prefix.iter().rposition(|&b| b == 0x1b))
    {
        // CSI is `ESC '[' params terminator-in-0x40..=0x7e`, with `[` excluded
        // from the terminator range. Earlier escape sequences must already be
        // terminated to have reached the parser state for this one.
        let scan_start = esc_off.saturating_add(2).min(cut);
        let terminated = bytes
            .get(scan_start..cut)
            .is_some_and(|tail| tail.iter().any(|&b| matches!(b, 0x40..=0x5a | 0x5c..=0x7e)));
        if !terminated {
            cut = esc_off;
        }
    }

    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }

    let truncated_bytes = text.len().saturating_sub(cut);
    let kept = text.get(..cut).unwrap_or("");
    format!("{kept}… ({truncated_bytes} bytes truncated)")
}

/// Synthesise a degraded module-query batch outcome after a child abort: every
/// requested selector reports `BudgetExceeded` so the caller sees an honest
/// "could not complete under resource pressure" per id rather than a hard error.
fn degraded_query_batch_outcome(
    selectors: &[LeanWorkerModuleQuerySelector],
    resource: LeanWorkerResourceExhaustedFacts,
) -> LeanWorkerModuleQueryBatchOutcome {
    let items = selectors
        .iter()
        .map(|selector| LeanWorkerModuleQueryBatchItem::BudgetExceeded {
            id: selector.id().to_owned(),
            message: "worker aborted during module query; result degraded under resource pressure".to_owned(),
        })
        .collect();
    LeanWorkerModuleQueryBatchOutcome::Ok {
        result: LeanWorkerModuleQueryBatchEnvelope {
            items,
            total_truncated: false,
        },
        imports: Vec::new(),
        facts: LeanWorkerModuleQueryCacheFacts {
            resource: Some(Box::new(resource)),
            ..LeanWorkerModuleQueryCacheFacts::uncached(0)
        },
    }
}

fn unexpected_response(operation: &'static str, response: &Response) -> LeanWorkerError {
    LeanWorkerError::Protocol {
        message: format!("worker sent unexpected {operation} response: {response:?}"),
    }
}

fn stale_worker_output_error(
    operation: &'static str,
    expected: WorkerGeneration,
    actual: WorkerGeneration,
) -> LeanWorkerError {
    LeanWorkerError::Protocol {
        message: format!(
            "worker sent stale {operation} frame from generation {}, current generation is {}",
            actual.0, expected.0
        ),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn fixture_capability_manifest(fixture_root: &Path) -> Result<PathBuf, LeanWorkerError> {
    let built = lean_toolchain::CargoLeanCapability::new(fixture_root, "LeanRsFixture")
        .package("lean_rs_fixture")
        .module("LeanRsFixture")
        .export_signature(fixture_mul_signature("lean_rs_fixture_u64_mul"))
        .export_signature(fixture_panic_signature("lean_rs_fixture_panic_unit"))
        .build_quiet()
        .map_err(|diagnostic| LeanWorkerError::CapabilityBuild { diagnostic })?;
    Ok(built.manifest_path().to_path_buf())
}

/// Parent-side collector that decodes per-row JSON-string payloads from
/// `list_declarations_strings` into an owned `Vec<String>`.
///
/// Each name lands as its own `Message::DataRow` so the 1 MiB protocol frame
/// cap binds per-name (any single Lean name is well under that) rather than
/// per-response. Decoding failures or sink panics are surfaced through the
/// usual `LeanWorkerError::Protocol` / `DataSinkPanic` paths.
#[derive(Debug, Default)]
struct DeclarationNameCollector {
    names: Mutex<Vec<String>>,
    decode_error: Mutex<Option<String>>,
}

impl DeclarationNameCollector {
    fn into_inner(self) -> Vec<String> {
        self.names.into_inner().unwrap_or_default()
    }
}

impl LeanWorkerRawDataSink for DeclarationNameCollector {
    fn report(&self, row: LeanWorkerRawDataRow) {
        match serde_json::from_str::<String>(row.payload.get()) {
            Ok(name) => {
                if let Ok(mut guard) = self.names.lock() {
                    guard.push(name);
                }
            }
            Err(err) => {
                if let Ok(mut slot) = self.decode_error.lock()
                    && slot.is_none()
                {
                    *slot = Some(format!("list_declarations_strings row payload decode failed: {err}"));
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn child_rss_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

#[cfg(not(target_os = "linux"))]
fn child_rss_kib(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<u64>().ok().filter(|value| *value > 0)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]
mod tests {
    use super::{
        DISPLAY_DIAGNOSTICS_MAX_BYTES, LeanWorkerConfig, LeanWorkerDeclarationVerificationFacts, LeanWorkerError,
        LeanWorkerExit, LeanWorkerLifecycleSnapshot, LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome,
        LeanWorkerModuleQuerySelector, LeanWorkerRestartPolicy, LeanWorkerRestartReason, LeanWorkerStats,
        MAX_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP, MIN_FRAME_BYTES, WorkerGeneration, stale_worker_output_error,
        truncate_for_display,
    };
    use std::path::PathBuf;
    use std::time::Duration;

    fn dummy_config() -> LeanWorkerConfig {
        LeanWorkerConfig::new(PathBuf::from("/nonexistent/lean-rs-worker-child"))
    }

    fn exit_with(diagnostics: &str, success: bool) -> LeanWorkerExit {
        let (code, status) = if success {
            (0_i32, "exit status: 0".to_owned())
        } else {
            (1_i32, "exit status: 1".to_owned())
        };
        LeanWorkerExit {
            success,
            code: Some(code),
            status,
            diagnostics: diagnostics.to_owned(),
        }
    }

    #[test]
    fn max_frame_bytes_default_matches_legacy_cap() {
        let config = dummy_config();
        assert_eq!(config.max_frame_bytes, MAX_FRAME_BYTES);
    }

    #[test]
    fn max_frame_bytes_clamps_below_floor() {
        let config = dummy_config().max_frame_bytes(1024);
        assert_eq!(config.max_frame_bytes, MIN_FRAME_BYTES);
    }

    #[test]
    fn max_frame_bytes_clamps_above_ceiling() {
        let config = dummy_config().max_frame_bytes(u32::MAX);
        assert_eq!(config.max_frame_bytes, MAX_FRAME_BYTES_HARD_CAP);
    }

    #[test]
    fn max_frame_bytes_passes_through_in_range() {
        let config = dummy_config().max_frame_bytes(8 * 1024 * 1024);
        assert_eq!(config.max_frame_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn rss_hard_limit_config_clamps_to_nonzero_policy() {
        let config = dummy_config().rss_hard_limit(0, Duration::ZERO);
        assert_eq!(config.rss_hard_limit_kib, Some(1));
        assert_eq!(config.rss_sample_interval, Duration::from_millis(1));
    }

    #[test]
    fn restart_intensity_policy_clamps_to_nonzero_window() {
        let policy = LeanWorkerRestartPolicy::default().max_restarts_per_window(0, Duration::ZERO);
        assert_eq!(policy.restart_intensity.max_restarts, 1);
        assert_eq!(policy.restart_intensity.window, Duration::from_millis(1));
    }

    #[test]
    fn conformance_stale_generation_output_is_protocol_failure() {
        let err = stale_worker_output_error("health", WorkerGeneration(2), WorkerGeneration(1));
        match err {
            LeanWorkerError::Protocol { message } => {
                assert!(message.contains("stale health frame"));
                assert!(message.contains("generation 1"));
                assert!(message.contains("current generation is 2"));
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[test]
    fn lifecycle_snapshot_exposes_restart_generation() {
        let stats = LeanWorkerStats {
            restarts: 3,
            exits: 2,
            last_restart_reason: Some(LeanWorkerRestartReason::RequestTimeout {
                operation: "test",
                duration: std::time::Duration::from_secs(1),
            }),
            last_rss_kib: Some(42),
            rss_samples_unavailable: 1,
            ..LeanWorkerStats::default()
        };
        let exit = exit_with("bye", false);
        let snapshot = LeanWorkerLifecycleSnapshot::from_worker(&stats, Some(exit.clone()));
        assert_eq!(snapshot.worker_generation, 3);
        assert_eq!(snapshot.restarts, 3);
        assert_eq!(snapshot.exits, 2);
        assert_eq!(snapshot.last_exit, Some(exit));
        assert_eq!(snapshot.last_rss_kib, Some(42));
        assert_eq!(snapshot.rss_samples_unavailable, 1);
    }

    #[test]
    fn restart_reason_exposes_stable_policy_cause() {
        assert_eq!(LeanWorkerRestartReason::Explicit.stable_cause(), "explicit");
        assert_eq!(
            LeanWorkerRestartReason::MaxRequests { limit: 1 }.stable_cause(),
            "max_requests"
        );
        assert_eq!(
            LeanWorkerRestartReason::MaxImports { limit: 1 }.stable_cause(),
            "max_imports"
        );
        assert_eq!(
            LeanWorkerRestartReason::RssCeiling {
                current_kib: 2,
                limit_kib: 1,
                last_import_stats: None,
            }
            .stable_cause(),
            "rss_ceiling"
        );
        assert_eq!(
            LeanWorkerRestartReason::RssHardLimit {
                operation: "test",
                current_kib: 2,
                limit_kib: 1,
                last_import_stats: None,
            }
            .stable_cause(),
            "rss_hard_limit"
        );
        assert_eq!(
            LeanWorkerRestartReason::RequestTimeout {
                operation: "test",
                duration: Duration::from_millis(1),
            }
            .stable_cause(),
            "timeout"
        );
        // The verify/proof-state abort guard surfaces the same `child_abort`
        // cause the parent's relabel heuristic already keys on.
        assert_eq!(
            LeanWorkerRestartReason::ChildAbort { operation: "test" }.stable_cause(),
            "child_abort"
        );
    }

    #[test]
    fn degraded_query_batch_outcome_marks_every_selector_budget_exceeded() {
        let selectors = vec![
            LeanWorkerModuleQuerySelector::ProofState {
                id: "a".to_owned(),
                line: 1,
                column: 1,
            },
            LeanWorkerModuleQuerySelector::Diagnostics { id: "b".to_owned() },
        ];
        let resource = super::resource_facts(
            "worker_child_abort",
            true,
            Some("worker_process_module_query_batch"),
            None,
            None,
            Some(1),
            Some(2),
            Some("child_abort".to_owned()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let LeanWorkerModuleQueryBatchOutcome::Ok { result, imports, facts } =
            super::degraded_query_batch_outcome(&selectors, resource.clone())
        else {
            panic!("degraded outcome should be Ok with per-selector items");
        };
        assert!(imports.is_empty());
        assert_eq!(facts.resource.as_deref(), Some(&resource));
        assert_eq!(result.items.len(), 2);
        // `filter_map` keeps only `BudgetExceeded` items, so a non-degraded item
        // would drop out and the id vector would no longer match.
        let ids: Vec<&str> = result
            .items
            .iter()
            .filter_map(|item| {
                if let LeanWorkerModuleQueryBatchItem::BudgetExceeded { id, .. } = item {
                    Some(id.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn unavailable_verification_facts_report_axioms_uncomputed() {
        let facts = LeanWorkerDeclarationVerificationFacts::unavailable();
        assert!(
            !facts.axioms_available,
            "degraded facts must not claim a computed axiom set"
        );
        assert!(facts.axioms.is_empty());
        assert!(facts.target.is_none());
    }

    #[test]
    fn display_child_panic_or_abort_includes_stderr_tail() {
        let exit = exit_with("could not dlopen X.dylib: image not found", false);
        let err = LeanWorkerError::ChildPanicOrAbort { exit };
        let rendered = err.to_string();
        assert!(rendered.contains("exit status"), "{rendered}");
        assert!(
            rendered.contains("could not dlopen X.dylib: image not found"),
            "{rendered}"
        );
        assert!(rendered.starts_with("worker exited fatally with "), "{rendered}");
    }

    #[test]
    fn display_child_exited_includes_stderr_tail() {
        let exit = exit_with("warning: lean-rs-worker exiting cleanly\n", true);
        let err = LeanWorkerError::ChildExited { exit };
        let rendered = err.to_string();
        assert!(rendered.starts_with("worker exited with "), "{rendered}");
        assert!(
            rendered.contains("warning: lean-rs-worker exiting cleanly"),
            "{rendered}"
        );
    }

    #[test]
    fn display_keeps_terse_format_when_diagnostics_empty() {
        let exit = exit_with("", false);
        let err = LeanWorkerError::ChildPanicOrAbort { exit };
        assert_eq!(err.to_string(), "worker exited fatally with exit status: 1");

        let exit = exit_with("   \n\t  ", true);
        let err = LeanWorkerError::ChildExited { exit };
        assert_eq!(err.to_string(), "worker exited with exit status: 0");
    }

    #[test]
    fn display_truncates_oversized_diagnostics_with_annotation() {
        let large: String = "x".repeat(DISPLAY_DIAGNOSTICS_MAX_BYTES * 2);
        let exit = exit_with(&large, false);
        let err = LeanWorkerError::ChildPanicOrAbort { exit };
        let rendered = err.to_string();
        assert!(rendered.contains("bytes truncated"), "{rendered}");
        assert!(
            rendered.len() < large.len() + 128,
            "rendered length {} unexpectedly large for original {}",
            rendered.len(),
            large.len()
        );
    }

    #[test]
    fn truncate_for_display_returns_input_when_under_cap() {
        let s = "short message";
        assert_eq!(truncate_for_display(s, 1024), s);
    }

    #[test]
    fn truncate_for_display_cuts_at_char_boundary() {
        // 4 copies of `é` (2 bytes each) → 8 bytes total. Capping at 5 must
        // not slice the multi-byte sequence in half.
        let s = "ééééé";
        let out = truncate_for_display(s, 5);
        let before_marker = out.split('…').next().unwrap_or("");
        assert!(before_marker.is_char_boundary(before_marker.len()));
        assert!(out.contains("bytes truncated"), "{out}");
    }

    #[test]
    fn truncate_for_display_does_not_split_ansi_csi() {
        // Construct: leading text, then ESC '[' '3' '1' 'm' (red), then more text.
        // Place the CSI so a naive cap lands inside it.
        let mut s = String::from("hello ");
        s.push('\x1b');
        s.push('[');
        s.push('3');
        s.push('1');
        s.push('m');
        s.push_str("RED");
        // Cap chosen to fall between ESC and the terminator `m`.
        let cap = s.find('1').expect("test fixture invariant: '1' present");
        let out = truncate_for_display(&s, cap);
        // The kept prefix must not contain a bare ESC.
        let before_marker = out.split('…').next().unwrap_or("");
        assert!(
            !before_marker.contains('\x1b'),
            "truncated prefix still contains ESC: {before_marker:?}"
        );
        assert!(out.contains("bytes truncated"), "{out}");
    }

    #[test]
    fn truncate_for_display_keeps_terminated_ansi_csi() {
        // A terminated CSI before the cap should survive truncation intact.
        let mut s = String::from("\x1b[31mRED\x1b[0m ");
        s.push_str(&"x".repeat(64));
        let out = truncate_for_display(&s, 20);
        assert!(out.contains("\x1b[31m"), "{out}");
        assert!(out.contains("bytes truncated"), "{out}");
    }
}
