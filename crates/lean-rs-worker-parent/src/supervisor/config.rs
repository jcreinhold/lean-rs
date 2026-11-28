//! Worker configuration: which child to run, its environment, and the
//! deadlines and limits the supervisor applies to it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lean_rs_worker_protocol::protocol::{MAX_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP, MIN_FRAME_BYTES};

use super::restart::LeanWorkerRestartPolicy;
use crate::process::LeanWorkerProcessTracker;

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Default deadline for one worker request after startup.
pub(crate) const LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/// Suggested deadline for long-running worker requests.
pub(crate) const LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING: Duration = Duration::from_mins(10);

/// Default deadline for graceful child shutdown before kill escalation.
pub(crate) const LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(2);

/// Default deadline for waiting on a killed child process to be reaped.
pub(crate) const LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT: Duration = Duration::from_secs(5);

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
///   Disabling the backtrace removes that dependency entirely.
///
/// Both are safe defaults—explicit `.env()` entries supplied here override
/// them, in case a caller knows the dependency is satisfied and wants a
/// demangled backtrace on the child's stderr.
#[derive(Clone, Debug)]
pub struct LeanWorkerConfig {
    pub(crate) application_launch_input: Option<String>,
    pub(super) executable: PathBuf,
    pub(super) process_tracker: LeanWorkerProcessTracker,
    pub(super) current_dir: Option<PathBuf>,
    pub(super) check_loader_environment: bool,
    pub(super) env: Vec<(OsString, OsString)>,
    pub(super) startup_timeout: Duration,
    pub(super) request_timeout: Duration,
    pub(super) shutdown_timeout: Duration,
    pub(super) restart_policy: LeanWorkerRestartPolicy,
    session_pool_capacity: Option<usize>,
    pub(super) rss_hard_limit_kib: Option<u64>,
    pub(super) rss_sample_interval: Duration,
    pub(super) max_frame_bytes: u32,
}

impl LeanWorkerConfig {
    /// Create a worker configuration for a child executable.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            application_launch_input: None,
            process_tracker: LeanWorkerProcessTracker::new(),
            current_dir: None,
            check_loader_environment: false,
            env: Vec::new(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            request_timeout: LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT,
            shutdown_timeout: LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT,
            restart_policy: LeanWorkerRestartPolicy::default(),
            session_pool_capacity: None,
            rss_hard_limit_kib: None,
            rss_sample_interval: Duration::from_millis(250),
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }

    /// Share confirmed-exit accounting across this worker's child generations.
    #[must_use]
    pub fn process_tracker(mut self, tracker: LeanWorkerProcessTracker) -> Self {
        self.process_tracker = tracker;
        self
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

    /// Refuse native loader environment overrides before each worker launch or restart.
    /// Other workers keep the default environment handling.
    #[must_use]
    pub fn checked_native_loader_environment(mut self) -> Self {
        self.check_loader_environment = true;
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
    /// The request timeout starts before writing the request frame. It covers
    /// live rows, diagnostics, progress events, and the terminal response. On
    /// timeout, the supervisor kills the child and replaces it when policy permits.
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

    /// How many imported environments the child may hold at once.
    ///
    /// This used to be derived from `max_imports`, which made the two equal by
    /// construction and, as a side effect, made the child's capacity eviction
    /// unreachable: the parent cycled the child exactly where the pool would
    /// have started evicting. The two answer different questions. Capacity is
    /// how many *distinct* profiles a workload alternates between, and a held
    /// environment costs about 70 MiB. The restart bound is how much
    /// unreclaimable residue one child may accumulate, and an import costs
    /// 2–4 GiB of it. Sizing one by the other prices a cheap thing like an
    /// expensive one.
    ///
    /// The supervisor's reuse hint tracks whatever this resolves to, so the
    /// parent predicts reuse for exactly the keys the child can still hold.
    /// Unset, the child keeps its own default.
    #[must_use]
    pub fn session_pool_capacity(mut self, capacity: usize) -> Self {
        self.session_pool_capacity = Some(capacity.max(1));
        self
    }

    /// The capacity the child will use, as the parent understands it.
    ///
    /// One resolver for both consumers — the environment variable handed to the
    /// child and the reuse hint's own bound — so the two cannot drift. The
    /// `max_imports` fallback preserves the old behaviour for an embedder that
    /// set only that.
    pub(super) fn resolved_session_pool_capacity(&self) -> Option<usize> {
        self.session_pool_capacity.or_else(|| {
            self.restart_policy
                .max_imports
                .and_then(|limit| usize::try_from(limit).ok())
        })
    }

    /// Kill and replace the worker during an in-flight request if its RSS is
    /// sampled at or above `limit_kib`.
    ///
    /// This is a protective hard stop, not a throughput optimization. The
    /// request that crosses the limit returns
    /// [`LeanWorkerError::RssHardLimitExceeded`] after the child is replaced.
    ///
    /// [`LeanWorkerError::RssHardLimitExceeded`]: crate::LeanWorkerError::RssHardLimitExceeded
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
