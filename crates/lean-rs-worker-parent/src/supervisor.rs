use crate::pipe::Pipe;
use crate::process::{CheckedSpawnError, LeanWorkerExitObservation, LeanWorkerProcessTracker, OwnedChild};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufReader, BufWriter, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::protocol::{
    HostSessionMode, MAX_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP, MIN_FRAME_BYTES, Message, Request, Response,
    read_frame, write_frame,
};
use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationBatchRequest,
    LeanWorkerDeclarationVerificationBatchResult, LeanWorkerDeclarationVerificationBatchRow,
    LeanWorkerDeclarationVerificationFacts, LeanWorkerDeclarationVerificationRequest,
    LeanWorkerDeclarationVerificationResult, LeanWorkerDeclarationVerificationStatus, LeanWorkerDoctorReport,
    LeanWorkerElabOptions, LeanWorkerElabResult, LeanWorkerHostRuntimeMetadata, LeanWorkerImportStats,
    LeanWorkerKernelResult, LeanWorkerMetaResult, LeanWorkerMetaTransparency, LeanWorkerModuleQuery,
    LeanWorkerModuleQueryBatchEnvelope, LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome,
    LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQueryOutcome, LeanWorkerModuleQuerySelector,
    LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerOutputBudgets, LeanWorkerProofAttemptRequest,
    LeanWorkerProofAttemptResult, LeanWorkerRendered, LeanWorkerResourceExhaustedFacts,
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
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_RESTART_INTENSITY_LIMIT: u64 = 16;
/// Reuse-hint capacity for an embedder that sets no `max_imports`. Mirrors the
/// child's own `SESSION_POOL_DEFAULT_CAPACITY`, which is what such a child uses.
const DEFAULT_REUSE_HINT_CAPACITY: usize = 4;
const DEFAULT_RESTART_INTENSITY_WINDOW: Duration = Duration::from_mins(1);

// Drain stderr from startup onward: pipe capacity must never stop Lean. Keep
// only the newest bytes, even when a worker writes without newlines.
const STDERR_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct StderrCapture {
    tail: Arc<Mutex<VecDeque<u8>>>,
    stopped: Arc<AtomicBool>,
    // The private reader returns (), invokes no user code, and shares only
    // mutex-protected bytes and an atomic stop flag. Parent unwinding cannot
    // expose an inconsistent value through its handle. Keep this assertion
    // narrow; revisit it if the reader ever carries user callbacks or data.
    reader: Option<std::panic::AssertUnwindSafe<thread::JoinHandle<()>>>,
}

impl StderrCapture {
    fn start(pipe: ChildStderr) -> std::io::Result<Self> {
        let mut pipe = Pipe::new(pipe)?;
        let stopped = Arc::clone(&pipe.stopped);
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)));
        let target = Arc::clone(&tail);
        let reader = thread::spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut bytes = target.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let discard = bytes.len().saturating_add(count).saturating_sub(STDERR_TAIL_BYTES);
                        bytes.drain(..discard);
                        bytes.extend(chunk.iter().take(count));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            tail,
            stopped,
            reader: Some(std::panic::AssertUnwindSafe(reader)),
        })
    }

    fn finish(mut self) -> String {
        // Stop our own pipe read even if another process inherited its write end.
        let started = Instant::now();
        while self.reader.as_ref().is_some_and(|reader| !reader.is_finished())
            && started.elapsed() < Duration::from_millis(100)
        {
            thread::sleep(Duration::from_millis(5));
        }
        self.stop();
        let bytes = self.tail.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&bytes.iter().copied().collect::<Vec<_>>()).into_owned()
    }

    fn stop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _joined = reader.0.join();
        }
    }
}

impl Drop for StderrCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

struct ReaderGuard {
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

/// Own a newly spawned process until all pipe/handshake/config setup succeeds.
/// `Child` itself does not terminate or reap its process when dropped.
#[derive(Debug)]
struct StartupChild(Option<OwnedChild>);

impl StartupChild {
    fn child_mut(&mut self) -> Result<&mut OwnedChild, LeanWorkerError> {
        self.0.as_mut().ok_or_else(|| LeanWorkerError::Setup {
            message: "startup child was already transferred".to_owned(),
        })
    }

    fn take_child(&mut self) -> Result<OwnedChild, LeanWorkerError> {
        self.0.take().ok_or_else(|| LeanWorkerError::Setup {
            message: "startup child was already transferred".to_owned(),
        })
    }

    fn cleanup(&mut self) -> Result<ExitStatus, LeanWorkerError> {
        let child = self.child_mut()?;
        if let Some(status) = child.try_wait().map_err(|source| LeanWorkerError::Wait { source })? {
            self.0.take();
            return Ok(status);
        }
        // A failed kill can race with a natural exit. Only wait/reap proves exit.
        let kill_error = child.kill().err();
        let started = Instant::now();
        loop {
            match child.try_wait().map_err(|source| LeanWorkerError::Wait { source })? {
                Some(status) => {
                    self.0.take();
                    return Ok(status);
                }
                None if started.elapsed() >= LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT => {
                    return Err(kill_error.map_or(
                        LeanWorkerError::WaitTimeout {
                            operation: "startup_cleanup",
                            duration: LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT,
                        },
                        |source| LeanWorkerError::Kill { source },
                    ));
                }
                None => thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

impl Drop for StartupChild {
    fn drop(&mut self) {
        if self.0.is_some() {
            drop(self.cleanup());
        }
    }
}

/// Failed setup keeps ownership when bounded cleanup cannot establish exit.
struct SpawnFailure {
    error: LeanWorkerError,
    pending: Option<Box<StartupChild>>,
    exit: Option<Box<LeanWorkerExit>>,
}

impl SpawnFailure {
    fn no_child(error: LeanWorkerError) -> Self {
        Self {
            error,
            pending: None,
            exit: None,
        }
    }

    fn after_spawn(
        mut child: StartupChild,
        error: LeanWorkerError,
        cleanup: impl FnOnce(&mut StartupChild) -> Result<ExitStatus, LeanWorkerError>,
    ) -> Self {
        // Handshake error/timeout cleanup may already have reaped and released it.
        if child.0.is_none() {
            let exit =
                if let LeanWorkerError::ChildExited { exit } | LeanWorkerError::ChildPanicOrAbort { exit } = &error {
                    Some(Box::new(exit.clone()))
                } else {
                    None
                };
            return Self {
                error,
                pending: None,
                exit,
            };
        }
        match cleanup(&mut child) {
            Ok(status) => Self {
                error,
                pending: None,
                exit: Some(Box::new(LeanWorkerExit::from_status(status, String::new()))),
            },
            Err(cleanup_error) => Self {
                error: LeanWorkerError::Setup {
                    message: format!("{error}; spawned child cleanup unconfirmed: {cleanup_error}"),
                },
                pending: Some(Box::new(child)),
                exit: None,
            },
        }
    }
}

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
///   `docs/architecture/worker-processes.md` for the boundary argument.
///
/// Both are safe defaults—explicit `.env()` entries supplied here override
/// them, in case a caller knows the dependency is satisfied and wants a
/// demangled backtrace on the child's stderr.
#[derive(Clone, Debug)]
pub struct LeanWorkerConfig {
    pub(crate) application_launch_input: Option<String>,
    pub(crate) shared_writer_executable: Option<PathBuf>,
    executable: PathBuf,
    process_tracker: LeanWorkerProcessTracker,
    current_dir: Option<PathBuf>,
    check_loader_environment: bool,
    shared_artifact_reads: bool,
    #[cfg(unix)]
    lifetime_directory: Option<PathBuf>,
    #[cfg(unix)]
    lifetime_socket_directory: Option<PathBuf>,
    #[cfg(unix)]
    native_guardian_executable: Option<PathBuf>,
    #[cfg(unix)]
    native_capacity_slot: Option<lean_toolchain::native_guardian::NativeCapacitySlot>,
    env: Vec<(OsString, OsString)>,
    startup_timeout: Duration,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    restart_policy: LeanWorkerRestartPolicy,
    session_pool_capacity: Option<usize>,
    rss_hard_limit_kib: Option<u64>,
    rss_sample_interval: Duration,
    max_frame_bytes: u32,
}

impl LeanWorkerConfig {
    /// Create a worker configuration for a child executable.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            application_launch_input: None,
            shared_writer_executable: None,
            process_tracker: LeanWorkerProcessTracker::new(),
            current_dir: None,
            check_loader_environment: false,
            shared_artifact_reads: false,
            #[cfg(unix)]
            lifetime_directory: None,
            #[cfg(unix)]
            lifetime_socket_directory: None,
            #[cfg(unix)]
            native_guardian_executable: None,
            #[cfg(unix)]
            native_capacity_slot: None,
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

    /// Enable an authenticated parent lifeline and durable startup ownership.
    ///
    /// The child establishes a private inherited process group and waits for
    /// its owned descriptor to be synced before starting Lean. EOF requests
    /// this worker's own exit; descendants retain capacity until the inherited
    /// group is absent. Regrouped descendants are outside this contract.
    #[cfg(unix)]
    #[must_use]
    pub fn lifetime_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.lifetime_directory = Some(directory.into());
        self
    }

    /// Select an existing private directory for the transient parent socket.
    ///
    /// Requires an absolute canonical path owned by this user with mode 0700.
    /// The full socket pathname must fit the platform limit. Invalid explicit
    /// roots fail before ownership reservation or child launch, without fallback.
    /// Without this option, the existing `/tmp` placement is unchanged.
    /// Requires [`Self::lifetime_directory`]. A parent crash before connection
    /// may leave a socket directory; this option does not add crash recovery.
    #[cfg(unix)]
    #[must_use]
    pub fn lifetime_socket_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.lifetime_socket_directory = Some(directory.into());
        self
    }

    /// Bind the independent guardian used with durable generation ownership.
    ///
    /// The guardian executable is link-free and must implement the private
    /// native-guardian protocol. A lifetime directory is also required.
    #[cfg(unix)]
    #[must_use]
    pub fn native_guardian(mut self, executable: impl Into<PathBuf>) -> Self {
        self.native_guardian_executable = Some(executable.into());
        self
    }

    /// Bind this generation to a bounded actor slot shared with a companion.
    ///
    /// The registry keeps occupied slots across broker death until their actual
    /// groups close. Each slot permits one main and one companion scope.
    #[cfg(unix)]
    #[must_use]
    pub fn native_capacity_slot(mut self, key: impl Into<String>, scope: impl Into<String>, limit: usize) -> Self {
        self.native_capacity_slot = Some(lean_toolchain::native_guardian::NativeCapacitySlot {
            key: key.into(),
            scope: scope.into(),
            limit,
        });
        self
    }

    /// Require actual worker-owned shared artifact read custody before host bodies.
    #[must_use]
    pub fn shared_artifact_reads(mut self, required: bool) -> Self {
        self.shared_artifact_reads = required;
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
    fn resolved_session_pool_capacity(&self) -> Option<usize> {
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
    max_import_residue_bytes: Option<u64>,
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

    /// Restart before an import-like request when completed imports have retained
    /// this many bytes the child cannot reclaim.
    ///
    /// This is the byte-denominated form of [`Self::max_imports`], and the one to
    /// prefer. An import retains its environment for the life of the process —
    /// `freeRegions` is unsound once extensions are loaded — so the only question
    /// a cycling policy can usefully ask is *how much* has been retained, not how
    /// many times. One import ranges from tens of megabytes to several gigabytes
    /// depending on the profile's closure, so a count that is right for one
    /// project is wrong for the next by two orders of magnitude.
    ///
    /// The quantity accumulated is `non_memory_mapped_region_bytes` from the
    /// child's own [`LeanWorkerImportStats`] — the compacted-region bytes Lean had
    /// to materialise privately rather than map from an `.olean` file. Measured
    /// against process `phys_footprint` on a Mathlib-scale project, it predicts
    /// retained bytes with slope 1.09 and R² = 0.96; `compacted_region_bytes` and
    /// the imported-extension-entry count both fall below 0.83. It also reports
    /// near-zero for the first import in a fresh process, which is correct: that
    /// import maps its regions at their preferred addresses and costs almost
    /// nothing, and only later imports in the same process must copy.
    ///
    /// A generation always completes at least one import, however large: the
    /// counter advances only on imports the child confirmed, and the check runs
    /// before a request, so a single import can never restart before itself.
    ///
    /// Pair this with a high [`Self::max_imports`] backstop. Nothing else stands
    /// between an accounting bug here and unbounded growth, and the backstop costs
    /// one integer compare.
    #[must_use]
    pub fn max_import_residue_bytes(mut self, limit: u64) -> Self {
        self.max_import_residue_bytes = Some(limit.max(1));
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

/// What one child generation has accumulated, and the cycling decision that
/// follows from it.
///
/// These counters reset together on every restart, which is the whole reason
/// they are one type: `LeanWorkerStats` is cloned forward onto the replacement
/// child, so anything per-generation kept there would never reset.
///
/// It also owns the *ordering* of the import bounds. Two places ask whether an
/// import bound has been crossed. Session opens add their confirmed private
/// region bytes; header events add only increases in the child's cumulative
/// count. Eviction never subtracts allocations. A failed header marks the
/// generation unknown so the next request replaces it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct GenerationBudget {
    requests: u64,
    imports: u64,
    import_residue_bytes: u64,
    header_import_bytes: u64,
    header_imports: u64,
    accounting_unknown: bool,
}

impl GenerationBudget {
    fn record_request(&mut self, import_like: bool) {
        self.requests = self.requests.saturating_add(1);
        if import_like {
            self.imports = self.imports.saturating_add(1);
        }
    }

    fn record_import(&mut self, import_stats: &LeanWorkerImportStats) {
        self.imports = self.imports.saturating_add(1);
        self.import_residue_bytes = self
            .import_residue_bytes
            .saturating_add(import_stats.non_memory_mapped_region_bytes);
    }

    fn record_header_imports(&mut self, bytes: u64, imports: u64, unknown: bool) {
        self.import_residue_bytes = self
            .import_residue_bytes
            .saturating_add(bytes.saturating_sub(self.header_import_bytes));
        self.header_import_bytes = self.header_import_bytes.max(bytes);
        self.header_imports = self.header_imports.max(imports);
        self.accounting_unknown |= unknown;
    }

    /// Why this generation must cycle before it performs another import, if it must.
    ///
    /// Residue first: when both bounds are configured the byte bound is the one
    /// meant to fire, and reporting a cycle as `max_imports` when bytes were what
    /// ran out sends an operator to the wrong knob.
    ///
    /// A generation always completes at least one import however large, because
    /// the counters only advance on completed imports and this is asked before a
    /// request. That is deliberate: the alternative is a child that can never
    /// serve a profile whose closure exceeds the whole budget.
    fn import_limit_reached(&self, policy: &LeanWorkerRestartPolicy) -> Option<LeanWorkerRestartReason> {
        if let Some(limit_bytes) = policy.max_import_residue_bytes
            && self.import_residue_bytes >= limit_bytes
        {
            return Some(LeanWorkerRestartReason::ImportResidue {
                residue_bytes: self.import_residue_bytes,
                limit_bytes,
            });
        }
        if let Some(limit) = policy.max_imports
            && self.imports >= limit
        {
            return Some(LeanWorkerRestartReason::MaxImports { limit });
        }
        None
    }

    fn request_limit_reached(&self, policy: &LeanWorkerRestartPolicy) -> Option<LeanWorkerRestartReason> {
        policy
            .max_requests
            .filter(|limit| self.requests >= *limit)
            .map(|limit| LeanWorkerRestartReason::MaxRequests { limit })
    }
}

/// Reason recorded for the latest worker cycle.
///
/// Non-exhaustive: cycling policy is expected to grow, and a caller that matches
/// on the cause it cares about should not break when it does. Use
/// [`Self::stable_cause`] for the flat wire-facing name.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeanWorkerRestartReason {
    /// The caller explicitly requested a process cycle.
    Explicit,
    /// Request count reached the configured limit before the next request.
    MaxRequests { limit: u64 },
    /// Import-like request count reached the configured limit before the next import.
    MaxImports { limit: u64 },
    /// Completed imports retained at least `limit_bytes` the child cannot reclaim.
    ImportResidue { residue_bytes: u64, limit_bytes: u64 },
    /// A failed header import may have left allocations that cannot be measured.
    ImportAccountingUnknown,
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
    ///
    /// `status` and `diagnostics` are carried because they are otherwise
    /// **unrecoverable**: the child's bounded stderr tail is captured at exit by
    /// the same code path that builds this reason, and nothing else retains it.
    /// A consumer that only sees `stable_cause() == "child_abort"` cannot
    /// distinguish a Lean out-of-memory panic from a segfault from a protocol
    /// bug — which is the difference between raising a limit and filing a bug.
    ChildAbort {
        operation: &'static str,
        /// Platform-rendered exit status, e.g. `signal: 6 (SIGABRT)`.
        status: String,
        /// Child stderr at the abort, trimmed and truncated for display.
        diagnostics: String,
    },
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
            Self::ImportResidue { .. } => "import_residue",
            Self::ImportAccountingUnknown => "import_accounting_unknown",
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
    ) -> Self {
        Self {
            worker_generation: stats.restarts,
            restarts: stats.restarts,
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

    /// Whether lifecycle recovery finished by proving whole-group absence
    /// after ECHILD, without recovering this request's actual exit status.
    ///
    /// A caller may stop watching and expire its retained unknown result. This
    /// fact does not produce a stop receipt or imply a successful proof.
    #[must_use]
    pub fn scope_closed_without_status(&self) -> bool {
        self.exit.scope_closed_without_status()
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

// Count only writes accepted by the pipe. BufWriter can accept a header before
// writing any bytes to the child, particularly when its first flush times out.
struct ScopedFrameWriter<'a> {
    pipe: &'a mut Pipe<ChildStdin>,
    sent: bool,
}

impl Write for ScopedFrameWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.pipe.write(bytes)?;
        self.sent |= n != 0;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pipe.flush()
    }
}

type StartupScope<'a> = (&'static str, &'a LeanWorkerCancellationToken);

fn startup_timeout_error(duration: Duration) -> LeanWorkerError {
    LeanWorkerError::Timeout {
        operation: "startup",
        duration,
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
            Some(duration),
            None,
            None,
            None,
            None,
            None,
            None,
        )),
    }
}

fn frame_timeout(started: Instant, configured: Duration, deadline: Option<Instant>) -> Duration {
    deadline.map_or(configured, |end| configured.min(end.saturating_duration_since(started)))
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

/// A transmission restriction, independent of outcomes, custody and capacity.
/// Activation is private/test-only until the public error contract and actual
/// host retirement adapter are reviewed together. No reply or Drop clears it.
#[derive(Debug)]
struct GenerationDispatchRestriction {
    owner: Arc<()>,
    native_generation_id: String,
    generation: WorkerGeneration,
    sealed: bool,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    warm_end: Option<lean_rs_worker_protocol::protocol::ManagedInspectionCommand>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct GenerationDispatchBinding {
    owner: Arc<()>,
}

#[cfg(test)]
#[derive(Debug, Eq, PartialEq)]
enum GenerationDispatchRejection {
    NativeGenerationUnavailable,
    NativeGenerationMismatch,
    AlreadyBound,
    ForeignBinding,
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
    /// Snapshot of the guarded lease attached to this exact current child.
    /// Cleared with that child; never reconstructed from a saved owner record.
    native_generation_id: Option<String>,
    generation_dispatch: Option<GenerationDispatchRestriction>,
    last_request: Option<InFlightRequest>,
    last_request_stop: Option<LeanWorkerRequestStopReceipt>,
    last_request_exit: Option<LeanWorkerPendingRequestStop>,
    /// A failed replacement's process is still owned until cleanup proves exit.
    /// Historical `last_exit` belongs to an earlier child and is not that proof.
    pending_startup: Option<Box<StartupChild>>,
    runtime_metadata: LeanWorkerRuntimeMetadata,
    host_runtime_metadata: Option<LeanWorkerHostRuntimeMetadata>,
    host_read_active: bool,
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
    restart_window: VecDeque<Instant>,
    state: WorkerSupervisorState,
}

#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "Response and Message are #[non_exhaustive] across the lean-rs-worker-protocol crate boundary; every wildcard arm here uniformly converts an unexpected variant into a protocol-level error rather than enumerating each known variant"
)]
impl LeanWorker {
    #[cfg(test)]
    fn arm_generation_dispatch(
        &mut self,
        expected_native_generation: &str,
    ) -> Result<GenerationDispatchBinding, GenerationDispatchRejection> {
        if self.generation_dispatch.is_some() {
            return Err(GenerationDispatchRejection::AlreadyBound);
        }
        if !matches!(self.state, WorkerSupervisorState::Idle { .. }) {
            return Err(GenerationDispatchRejection::NativeGenerationUnavailable);
        }
        let native_generation_id = self
            .current_native_generation_id()
            .ok_or(GenerationDispatchRejection::NativeGenerationUnavailable)?;
        if native_generation_id != expected_native_generation {
            return Err(GenerationDispatchRejection::NativeGenerationMismatch);
        }
        let binding = GenerationDispatchBinding {
            owner: Arc::clone(&self.request_scope),
        };
        self.generation_dispatch = Some(GenerationDispatchRestriction {
            owner: Arc::clone(&binding.owner),
            native_generation_id: native_generation_id.to_owned(),
            generation: self.generation,
            sealed: false,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            warm_end: None,
        });
        Ok(binding)
    }

    #[cfg(test)]
    fn seal_generation_dispatch(
        &mut self,
        binding: &GenerationDispatchBinding,
    ) -> Result<(), GenerationDispatchRejection> {
        let Some(restriction) = &self.generation_dispatch else {
            return Err(GenerationDispatchRejection::ForeignBinding);
        };
        if !Arc::ptr_eq(&restriction.owner, &binding.owner) {
            return Err(GenerationDispatchRejection::ForeignBinding);
        }
        self.seal_generation_dispatch_internal();
        Ok(())
    }

    fn current_native_generation_id(&self) -> Option<&str> {
        if self.child.is_none() || self.cleanup_pending() {
            return None;
        }
        self.native_generation_id.as_deref()
    }

    fn seal_generation_dispatch_internal(&mut self) {
        if let Some(restriction) = &mut self.generation_dispatch {
            restriction.sealed = true;
        }
    }

    fn seal_dispatch_error<T>(&mut self, result: Result<T, LeanWorkerError>) -> Result<T, LeanWorkerError> {
        if result.is_err() {
            self.seal_generation_dispatch_internal();
        }
        result
    }

    fn unexpected_dispatch_response(&mut self, operation: &'static str, response: &Response) -> LeanWorkerError {
        self.seal_generation_dispatch_internal();
        unexpected_response(operation, response)
    }

    fn check_request_cancelled(
        &mut self,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        let result = check_cancelled(operation, cancellation);
        self.seal_dispatch_error(result)
    }

    fn is_dispatch_cleanup(request: &Request) -> bool {
        matches!(request, Request::Terminate | Request::ReleaseHostRead)
    }

    fn check_generation_dispatch(&mut self, operation: &'static str, cleanup: bool) -> Result<(), LeanWorkerError> {
        // Cleanup is authorized by the actual currently owned child, even if
        // the pin detects a mismatch. It neither discharges the pin nor proves
        // closure of the generation named by the pin.
        if cleanup {
            return Ok(());
        }
        let refused = self.generation_dispatch.as_ref().is_some_and(|restriction| {
            restriction.sealed
                || !Arc::ptr_eq(&restriction.owner, &self.request_scope)
                || restriction.generation != self.generation
                || self.current_native_generation_id() != Some(restriction.native_generation_id.as_str())
                || !matches!(
                    self.state,
                    WorkerSupervisorState::Idle { .. }
                        | WorkerSupervisorState::Busy { .. }
                        | WorkerSupervisorState::Streaming { .. }
                )
        });
        if refused {
            self.seal_generation_dispatch_internal();
            // This class is internal to test-only activation. No new public
            // refusal contract or changed pool LeaseInvalidated semantics is
            // exported before the paired host adapter is reviewed.
            return Err(LeanWorkerError::ShutdownInProgress { operation });
        }
        Ok(())
    }

    /// Spawn a worker child and wait for its protocol handshake.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the child cannot be spawned, child setup
    /// fails, the child exits before handshaking, or the startup timeout
    /// expires.
    pub fn spawn(config: &LeanWorkerConfig) -> Result<Self, LeanWorkerError> {
        Self::spawn_tracked(config).map_err(|failure| failure.error)
    }

    fn spawn_tracked(config: &LeanWorkerConfig) -> Result<Self, SpawnFailure> {
        Self::spawn_tracked_scoped(config, None)
    }

    fn spawn_tracked_scoped(config: &LeanWorkerConfig, scope: Option<StartupScope<'_>>) -> Result<Self, SpawnFailure> {
        let spawn_started = Instant::now();
        let deadline = scope.and_then(|(_, token)| token.deadline());
        let startup_timeout = frame_timeout(spawn_started, config.startup_timeout, deadline);
        if let Some((operation, token)) = scope {
            check_cancelled(operation, Some(token)).map_err(SpawnFailure::no_child)?;
            if startup_timeout.is_zero() {
                return Err(SpawnFailure::no_child(startup_timeout_error(startup_timeout)));
            }
        }
        let mut command = Command::new(&config.executable);
        // This child is a runtime process. A parent running inside Cargo may inherit
        // a build-script OUT_DIR that is outside the worker’s artifact custody.
        command.env_remove("OUT_DIR");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("LEAN_ABORT_ON_PANIC", "1")
            .env("LEAN_BACKTRACE", "0")
            .env("RUST_BACKTRACE", "0");

        // How many imported environments the child may hold. Independent of the
        // restart bound: capacity is priced in held environments (~70 MiB each),
        // the restart bound in unreclaimable import residue (~2–4 GiB each). Set
        // before `config.env` so an embedder that names the variable explicitly
        // still wins.
        command.env(
            "LEAN_RS_WORKER_MAX_IMPORTS",
            config.restart_policy.max_imports.unwrap_or(0).to_string(),
        );
        command.env(
            "LEAN_RS_WORKER_IMPORT_RESIDUE_BYTES",
            config.restart_policy.max_import_residue_bytes.unwrap_or(0).to_string(),
        );
        if let Some(capacity) = config.resolved_session_pool_capacity() {
            command.env("LEAN_RS_WORKER_SESSION_POOL_CAPACITY", capacity.to_string());
        }

        if let Some(current_dir) = &config.current_dir {
            command.current_dir(current_dir);
        }
        if !config.shared_artifact_reads {
            // An ordinary deployment selects its own snapshot for this child.
            command.env_remove("LEAN_RS_PREPARED_HOST_ARTIFACTS");
        }
        #[cfg(unix)]
        if config.native_guardian_executable.is_none()
            && !config
                .env
                .iter()
                .any(|(key, _)| key == lean_toolchain::native_guardian::OWNERS_DIRECTORY_ENV)
        {
            let registry = lean_toolchain::native_guardian::registry_directory().map_err(|source| {
                SpawnFailure::no_child(LeanWorkerError::Setup {
                    message: format!("could not bind native ownership registry: {source}"),
                })
            })?;
            command.env(lean_toolchain::native_guardian::OWNERS_DIRECTORY_ENV, registry);
        }
        for (key, value) in &config.env {
            command.env(key, value);
        }
        lean_toolchain::shared_writer::configure_writer_executable(
            &mut command,
            config.shared_writer_executable.as_deref(),
        )
        .map_err(|error| {
            SpawnFailure::no_child(LeanWorkerError::Setup {
                message: error.to_string(),
            })
        })?;
        // This private handoff cannot be inherited or supplied through `.env()`.
        // It is derived only from the reviewed lifeline root below.
        command.env_remove("LEAN_RS_WORKER_ARTIFACT_IPC_ROOT");
        if config.check_loader_environment {
            crate::inherit_checked_native_loader_environment(&mut command).map_err(|error| {
                SpawnFailure::no_child(LeanWorkerError::Setup {
                    message: error.to_string(),
                })
            })?;
        }
        // Install this reserved value after inherited and explicit generic environment
        // choices, on the common path for both initial and replacement generations.
        if let Some(input) = &config.application_launch_input {
            command.env("LEAN_RS_WORKER_APPLICATION_INPUT", input);
        } else {
            command.env_remove("LEAN_RS_WORKER_APPLICATION_INPUT");
        }
        if config.shared_artifact_reads {
            command.env("LEAN_RS_WORKER_SHARED_ARTIFACT_READS", "1");
            #[cfg(unix)]
            if config.native_guardian_executable.is_some()
                && let Some(root) = &config.lifetime_socket_directory
            {
                command.env("LEAN_RS_WORKER_ARTIFACT_IPC_ROOT", root);
            }
        }

        #[cfg(unix)]
        if config.native_capacity_slot.is_some() && config.native_guardian_executable.is_none() {
            return Err(SpawnFailure::no_child(LeanWorkerError::Setup {
                message: "native actor capacity requires the independent guardian".to_owned(),
            }));
        }
        #[cfg(unix)]
        if let Some(executable) = &config.native_guardian_executable {
            if config.lifetime_directory.is_none() {
                return Err(SpawnFailure::no_child(LeanWorkerError::Setup {
                    message: "native guardian requires a durable lifetime directory".to_owned(),
                }));
            }
            if config.shared_artifact_reads {
                let registry = lean_toolchain::native_guardian::registry_directory().map_err(|source| {
                    SpawnFailure::no_child(LeanWorkerError::Setup {
                        message: format!("could not bind native ownership registry: {source}"),
                    })
                })?;
                if config.lifetime_directory.as_ref() != Some(&registry) {
                    return Err(SpawnFailure::no_child(LeanWorkerError::Setup {
                        message:
                            "protected native lifetime directory differs from the managed writer ownership registry"
                                .to_owned(),
                    }));
                }
            }
            command.env(lean_toolchain::native_guardian::EXECUTABLE_ENV, executable);
        }
        #[cfg(unix)]
        if config.lifetime_socket_directory.is_some() && config.lifetime_directory.is_none() {
            return Err(SpawnFailure::no_child(LeanWorkerError::Setup {
                message: "lifeline socket directory requires a durable lifetime directory".to_owned(),
            }));
        }
        #[cfg(unix)]
        let mut lifetime = config
            .lifetime_directory
            .as_deref()
            .map(|directory| {
                crate::crash_ownership::ParentLifetime::prepare_with_socket_directory(
                    directory,
                    config.lifetime_socket_directory.as_deref(),
                    config.native_guardian_executable.is_some(),
                    startup_timeout.saturating_sub(spawn_started.elapsed()),
                    config.native_capacity_slot.as_ref(),
                )
            })
            .transpose()
            .map_err(|source| {
                SpawnFailure::no_child(LeanWorkerError::Setup {
                    message: format!("could not prepare worker parent lifeline: {source}"),
                })
            })?;
        #[cfg(unix)]
        if let Some(lifetime) = &lifetime {
            lifetime.configure(&mut command);
        }

        let admit = || {
            if let Some((operation, token)) = scope {
                check_cancelled(operation, Some(token))?;
                if spawn_started.elapsed() >= startup_timeout {
                    return Err(startup_timeout_error(startup_timeout));
                }
            }
            Ok(())
        };
        admit().map_err(SpawnFailure::no_child)?;

        #[cfg(unix)]
        let spawned = match lifetime.as_mut() {
            Some(lifetime) if scope.is_some() => {
                config
                    .process_tracker
                    .spawn_guarded_checked(&mut command, lifetime, admit)
            }
            Some(lifetime) => config
                .process_tracker
                .spawn_guarded(&mut command, lifetime)
                .map_err(CheckedSpawnError::Io),
            None if scope.is_some() => config.process_tracker.spawn_checked(&mut command, admit),
            None => config
                .process_tracker
                .spawn(&mut command)
                .map_err(CheckedSpawnError::Io),
        };
        #[cfg(not(unix))]
        let spawned = if scope.is_some() {
            config.process_tracker.spawn_checked(&mut command, admit)
        } else {
            config
                .process_tracker
                .spawn(&mut command)
                .map_err(CheckedSpawnError::Io)
        };
        let mut child = StartupChild(Some(spawned.map_err(|failure| {
            SpawnFailure::no_child(match failure {
                CheckedSpawnError::Io(source) => LeanWorkerError::Spawn {
                    executable: config.executable.clone(),
                    source,
                },
                CheckedSpawnError::Refused {
                    reason,
                    retirement_error: None,
                } => reason,
                CheckedSpawnError::Refused {
                    reason,
                    retirement_error: Some(error),
                } => LeanWorkerError::Setup {
                    message: format!("{reason}; no child started, but owner record retirement is unconfirmed: {error}"),
                },
            })
        })?));
        let initialized = (|| {
            #[cfg(unix)]
            let native_generation_id = if let Some(lifetime) = lifetime {
                let remaining = startup_timeout.saturating_sub(spawn_started.elapsed());
                let lease =
                    lifetime
                        .accept(child.child_mut()?.id(), remaining)
                        .map_err(|source| LeanWorkerError::Setup {
                            message: format!("worker parent lifeline startup failed: {source}"),
                        })?;
                let native_generation_id = lease.guarded_generation_id().map(str::to_owned);
                // Install ownership before ACK: a partial ACK must retain the
                // exact inherited-group fence through startup cleanup.
                child
                    .child_mut()?
                    .attach_lifetime(lease)
                    .map_err(|source| LeanWorkerError::Setup {
                        message: format!("could not pin native worker group: {source}"),
                    })?;
                child
                    .child_mut()?
                    .acknowledge_lifetime()
                    .map_err(|source| LeanWorkerError::Setup {
                        message: format!("worker parent lifeline ACK failed: {source}"),
                    })?;
                native_generation_id
            } else {
                None
            };
            #[cfg(not(unix))]
            let native_generation_id = None;
            Self::initialize_child(
                config,
                spawn_started,
                startup_timeout,
                scope.is_some(),
                &mut child,
                native_generation_id,
            )
        })();
        initialized.map_err(|error| SpawnFailure::after_spawn(child, error, StartupChild::cleanup))
    }

    fn initialize_child(
        config: &LeanWorkerConfig,
        spawn_started: Instant,
        startup_timeout: Duration,
        scoped: bool,
        child: &mut StartupChild,
        native_generation_id: Option<String>,
    ) -> Result<Self, LeanWorkerError> {
        let mut stdin = child
            .child_mut()?
            .stdin
            .take()
            .map(Pipe::new)
            .transpose()
            .map_err(|source| LeanWorkerError::Wait { source })?
            .map(BufWriter::new)
            .ok_or_else(|| LeanWorkerError::Setup {
                message: "child stdin unavailable".to_owned(),
            })?;
        let stdout = child.child_mut()?.stdout.take().ok_or_else(|| LeanWorkerError::Setup {
            message: "child stdout unavailable".to_owned(),
        })?;
        let stderr = child
            .child_mut()?
            .stderr
            .take()
            .map(StderrCapture::start)
            .transpose()
            .map_err(|source| LeanWorkerError::Wait { source })?;
        let stdout = Pipe::new(stdout).map_err(|source| LeanWorkerError::Wait { source })?;
        let handshake_stop = Arc::clone(&stdout.stopped);

        let max_frame_bytes = config.max_frame_bytes;
        let (sender, receiver) = mpsc::channel();
        let _handshake_reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let result = expect_handshake(&mut stdout, max_frame_bytes);
            drop(sender.send((stdout, result)));
        });

        // Lifeline authentication and pipe setup already consumed startup time.
        // READY shares that budget, including the existing zero-wait cleanup path.
        let remaining = startup_timeout.saturating_sub(spawn_started.elapsed());
        let (stdout, runtime_metadata) = match receiver.recv_timeout(remaining) {
            Ok((stdout, Ok(metadata))) => (stdout, metadata),
            Ok((_stdout, Err(_handshake_err))) => {
                // The handshake-thread observed a protocol-level error reading
                // the child's first frame. In practice this means the child is
                // mid-`abort()` and hasn't quite died yet—using `try_wait`
                // (non-blocking) here loses the race and drops the child's
                // stderr. Kill if still alive, then go through the canonical
                // post-mortem path so `LeanWorkerExit.diagnostics` carries the
                // bootstrap stderr with the same diagnostic contract as a runtime crash.
                let exit = wait_with_stderr(child, stderr)?;
                return Err(if exit.success {
                    LeanWorkerError::ChildExited { exit }
                } else {
                    LeanWorkerError::ChildPanicOrAbort { exit }
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                handshake_stop.store(true, Ordering::Release);
                let _exit = wait_with_stderr(child, stderr)?;
                return Err(startup_timeout_error(startup_timeout));
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
        if scoped {
            if spawn_started.elapsed() >= startup_timeout {
                return Err(startup_timeout_error(startup_timeout));
            }
            stdin.get_mut().deadline = spawn_started.checked_add(startup_timeout);
        }
        let configured = write_frame(
            &mut stdin,
            Message::ConfigureFrameLimit { max_frame_bytes },
            max_frame_bytes,
        );
        stdin.get_mut().deadline = None;
        configured.map_err(|error| {
            if scoped
                && matches!(&error, lean_rs_worker_protocol::protocol::ProtocolError::Io(source)
                    if source.kind() == io::ErrorKind::TimedOut)
            {
                startup_timeout_error(startup_timeout)
            } else {
                LeanWorkerError::Protocol {
                    message: format!("failed to send ConfigureFrameLimit: {error}"),
                }
            }
        })?;
        if scoped && spawn_started.elapsed() >= startup_timeout {
            return Err(startup_timeout_error(startup_timeout));
        }

        let spawn_handshake_elapsed = spawn_started.elapsed();
        let child = child.take_child()?;
        Ok(Self {
            config: config.clone(),
            child: Some(child),
            stdin: Some(stdin),
            request_started: None,
            stdout: Some(stdout),
            stderr,
            last_exit: None,
            request_scope: Arc::new(()),
            native_generation_id,
            generation_dispatch: None,
            last_request: None,
            last_request_stop: None,
            last_request_exit: None,
            pending_startup: None,
            runtime_metadata,
            host_runtime_metadata: None,
            host_read_active: false,
            last_host_session_refusal: None,
            stats: LeanWorkerStats {
                last_spawn_handshake_elapsed: Some(spawn_handshake_elapsed),
                ..LeanWorkerStats::default()
            },
            budget: GenerationBudget::default(),
            open_session_keys: Vec::new(),
            reuse_hint_enabled: true,
            reuse_observed: false,
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
        self.prepare_host_request(false, "health", None, None)?;
        self.send_request("health", Request::Health)?;
        self.record_request(false);
        let result = match self.read_response("health")? {
            Response::HealthOk => Ok(()),
            other => Err(unexpected_response("health", &other)),
        };
        self.seal_dispatch_error(result)
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
        let prepared = fixture_capability_manifest(fixture_root.as_ref());
        let manifest = self.seal_dispatch_error(prepared)?;
        self.prepare_request(true, "load_fixture_capability", None)?;
        self.send_request(
            "load_fixture_capability",
            Request::LoadFixtureCapability {
                manifest_path: path_string(&manifest.path),
            },
        )?;
        self.record_request(true);
        let result = match self.read_response("load_fixture_capability")? {
            Response::CapabilityLoaded => Ok(()),
            other => Err(unexpected_response("load_fixture_capability", &other)),
        };
        self.seal_dispatch_error(result)
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
        let prepared = fixture_capability_manifest(fixture_root.as_ref());
        let manifest = self.seal_dispatch_error(prepared)?;
        self.prepare_request(true, "call_fixture_mul", None)?;
        self.send_request(
            "call_fixture_mul",
            Request::CallFixtureMul {
                manifest_path: path_string(&manifest.path),
                lhs,
                rhs,
            },
        )?;
        self.record_request(true);
        let result = match self.read_response("call_fixture_mul")? {
            Response::U64 { value } => Ok(value),
            other => Err(unexpected_response("call_fixture_mul", &other)),
        };
        self.seal_dispatch_error(result)
    }

    /// Return the current worker lifecycle status.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if checking the process status fails.
    pub fn status(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        let result = self.status_inner();
        self.seal_dispatch_error(result)
    }

    fn status_inner(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        if self.pending_startup.is_some() {
            return Err(self.dead_error());
        }
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
                let exit = self.finalize_child_exit(status);
                Ok(LeanWorkerStatus::Exited(exit))
            }
            None => Ok(LeanWorkerStatus::Running),
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

    /// `session_open_import_elapsed` is `None` when the builder opened no
    /// session — the ordinary case now that opening one is reserved for a
    /// metadata check. The first real session open records its own timing
    /// through [`open_worker_session`](Self::open_worker_session), which is the
    /// more representative sample anyway: it carries the caller's imports,
    /// where a bootstrap open carried the builder's placeholder set.
    pub(crate) fn record_capability_open_timing(
        &mut self,
        capability_load_elapsed: Duration,
        session_open_import_elapsed: Option<Duration>,
    ) {
        self.stats.last_capability_load_elapsed = Some(capability_load_elapsed);
        if let Some(elapsed) = session_open_import_elapsed {
            self.stats.last_session_open_import_elapsed = Some(elapsed);
        }
        if let Some(timing) = self.stats.last_replacement_timing.as_mut() {
            timing.capability_load = capability_load_elapsed;
            if let Some(elapsed) = session_open_import_elapsed {
                timing.session_open_import = elapsed;
            }
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
        LeanWorkerLifecycleSnapshot::from_worker(
            &self.stats,
            self.last_exit.clone(),
            self.budget.import_residue_bytes,
            self.config.restart_policy.max_import_residue_bytes,
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

    /// Measure the current child CPU time (user + system) in milliseconds.
    ///
    /// Cumulative since the child started; take a delta around a unit of work
    /// for a contention-immune cost measure. A `None` result means the platform
    /// did not provide a usable sample; it is not a worker failure.
    pub fn cumulative_cpu_millis(&mut self) -> Option<u64> {
        self.child_cpu_millis()
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
        self.child.as_ref().map(|child| child.id())
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
        self.try_shutdown()
    }

    /// Stop the child without consuming this owner if cleanup fails.
    ///
    /// # Errors
    /// Returns the cleanup error while retaining the child for another attempt.
    pub fn try_shutdown(&mut self) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
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
        let prepared = fixture_capability_manifest(fixture_root.as_ref());
        let manifest = self.seal_dispatch_error(prepared)?;
        self.prepare_request(true, "trigger_lean_panic", None)?;
        self.send_request(
            "trigger_lean_panic",
            Request::TriggerLeanPanic {
                manifest_path: path_string(&manifest.path),
            },
        )?;
        self.record_request(true);
        match self.read_response("trigger_lean_panic") {
            Ok(response) => Err(self.unexpected_dispatch_response("trigger_lean_panic", &response)),
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
        self.check_request_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false, OPERATION, cancellation)?;
        self.send_request_cancellable(Request::EmitTestRows { streams }, OPERATION, cancellation)?;
        self.record_request(false);
        match self.read_response_with_events(
            OPERATION,
            None,
            cancellation,
            data.map(LeanWorkerDataSinkTarget::Value),
            None,
        )? {
            Response::RowsComplete { count } => Ok(count),
            other => Err(self.unexpected_dispatch_response(OPERATION, &other)),
        }
    }

    #[doc(hidden)]
    /// Emit synthetic data rows and then let the fake child exit without a terminal response.
    ///
    /// # Errors
    ///
    /// Returns the terminal worker failure observed after tentative row delivery.
    pub fn __emit_test_rows_then_exit(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        data: Option<&dyn LeanWorkerDataSink>,
    ) -> Result<(), LeanWorkerError> {
        const OPERATION: &str = "emit_test_rows_then_exit";
        self.check_request_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false, OPERATION, cancellation)?;
        self.send_request_cancellable(Request::EmitTestRowsThenExit, OPERATION, cancellation)?;
        self.record_request(false);
        let response = self.read_response_with_events(
            OPERATION,
            None,
            cancellation,
            data.map(LeanWorkerDataSinkTarget::Value),
            None,
        )?;
        Err(self.unexpected_dispatch_response(OPERATION, &response))
    }

    #[doc(hidden)]
    /// Emit synthetic data rows and then abort the fake child before terminal success.
    ///
    /// # Errors
    ///
    /// Returns the terminal worker failure observed after tentative row delivery.
    pub fn __emit_test_rows_then_panic(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        data: Option<&dyn LeanWorkerDataSink>,
    ) -> Result<(), LeanWorkerError> {
        const OPERATION: &str = "emit_test_rows_then_panic";
        self.check_request_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false, OPERATION, cancellation)?;
        self.send_request_cancellable(Request::EmitTestRowsThenPanic, OPERATION, cancellation)?;
        self.record_request(false);
        let response = self.read_response_with_events(
            OPERATION,
            None,
            cancellation,
            data.map(LeanWorkerDataSinkTarget::Value),
            None,
        )?;
        Err(self.unexpected_dispatch_response(OPERATION, &response))
    }

    /// How many session keys the reuse hint may remember.
    ///
    /// Exactly the pool capacity the child was spawned with, resolved through
    /// the same function that set it, so the parent predicts reuse for exactly
    /// the keys the child can still be holding. An embedder that configures
    /// neither gets the child's own default; the two disagree only if the
    /// child's `LEAN_RS_WORKER_SESSION_POOL_CAPACITY` was set by hand.
    ///
    /// The child checks its actual cache before any new byte-budgeted import.
    /// Over-predicting costs one recycle response and bounded retry. Under-
    /// predicting costs one admission check that was not needed — free under a
    /// byte bound, since the check is idempotent and a re-examined budget that
    /// has not moved does not restart anything.
    fn reuse_hint_capacity(&self) -> usize {
        self.config
            .resolved_session_pool_capacity()
            .unwrap_or(DEFAULT_REUSE_HINT_CAPACITY)
            .max(1)
    }

    /// Record that the child holds `config`, MRU at the back, evicting the
    /// least recently used key past capacity — the child's own eviction order.
    fn note_open_session_key(&mut self, config: &LeanWorkerSessionConfig) {
        if let Some(index) = self.open_session_keys.iter().position(|key| key == config) {
            let key = self.open_session_keys.remove(index);
            self.open_session_keys.push(key);
            return;
        }
        self.open_session_keys.push(config.clone());
        let capacity = self.reuse_hint_capacity();
        while self.open_session_keys.len() > capacity {
            self.open_session_keys.remove(0);
        }
    }

    pub(crate) fn open_worker_session(
        &mut self,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<(), LeanWorkerError> {
        self.check_generation_dispatch("open_worker_session", false)?;
        self.host_runtime_metadata = None;
        self.last_host_session_refusal = None;
        let mut observed_pre_body_refusal = None;
        let result = (|| {
            self.open_worker_session_attempt(config, cancellation, progress, true)?;
            if config.host_runtime_expectation().is_some() || self.config.shared_artifact_reads {
                let actual = self.worker_host_runtime_metadata(cancellation, progress)?;
                if let Some(expected) = config.host_runtime_expectation()
                    && let Err(message) = expected.validate(&actual)
                {
                    // This exact metadata response was consumed. It is a
                    // pre-body refusal, unlike a transport/protocol failure
                    // while waiting for the response.
                    observed_pre_body_refusal.clone_from(&self.last_request);
                    return Err(LeanWorkerError::Protocol {
                        message: format!("host runtime compatibility: {message}"),
                    });
                }
                if self.config.shared_artifact_reads
                    && !actual.artifact_read_epochs.as_ref().is_some_and(|epochs| {
                        !epochs.is_empty()
                            && epochs.iter().any(|epoch| {
                                Path::new(&epoch.project_root) == Path::new(&config.project_root_string())
                                    && !epoch.epoch.is_empty()
                            })
                    })
                {
                    observed_pre_body_refusal.clone_from(&self.last_request);
                    return Err(LeanWorkerError::Protocol {
                        message: "worker did not report actual project artifact read custody".to_owned(),
                    });
                }
                self.host_runtime_metadata = Some(actual);
            }
            Ok(())
        })();
        if result.is_err() {
            // Preserve the exact framed pre-body refusal before cleanup sends
            // another request or replaces the generation. It is no exit claim.
            let refusal = observed_pre_body_refusal
                .or_else(|| {
                    matches!(&result, Err(LeanWorkerError::Worker { .. }))
                        .then(|| self.last_request.clone())
                        .flatten()
                })
                .filter(|request| {
                    matches!(
                        request.operation,
                        "open_worker_session" | "worker_host_runtime_metadata"
                    )
                });
            // Only an observed terminal boundary may release. An unknown
            // operation stays owned and is retired by the ordinary supervisor.
            drop(self.release_host_read());
            self.last_host_session_refusal = refusal;
        }
        self.seal_dispatch_error(result)
    }

    fn open_worker_session_attempt(
        &mut self,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
        allow_retry: bool,
    ) -> Result<(), LeanWorkerError> {
        const OPERATION: &str = "open_worker_session";
        self.check_request_cancelled(OPERATION, cancellation)?;
        self.release_host_read()?;
        // An explicit switch needs only its requested profile in the new child.
        if self.budget.accounting_unknown {
            self.restart_for_request(
                LeanWorkerRestartReason::ImportAccountingUnknown,
                OPERATION,
                cancellation,
            )?;
        }
        let before_restarts = self.stats.restarts;
        // A hint, not a decision: it only picks which admission checks run
        // before the frame goes out. What actually happened is whichever of
        // `HostSessionOpened` / `HostSessionReused` comes back, and the import
        // accounting below follows that, not this.
        let expect_reuse = self.reuse_hint_enabled && self.open_session_keys.iter().any(|key| key == config);
        self.prepare_request(!expect_reuse, OPERATION, cancellation)?;
        // `prepare_request` may have replaced the child — `max_requests`,
        // `idle_timeout`, and `max_rss_kib` all restart from inside it. A fresh
        // child necessarily imports, and reading that as "this worker binary
        // does not reuse sessions" would permanently disable the hint for a
        // reason that has nothing to do with the binary.
        let expect_reuse = expect_reuse && self.stats.restarts == before_restarts;
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
        if self.config.shared_artifact_reads {
            self.host_read_active = true;
        }
        self.send_request_cancellable(
            Request::OpenHostSession {
                project_root: config.project_root_string(),
                mode,
                imports: config.imports().to_vec(),
                import_profile: config.import_profile(),
            },
            OPERATION,
            cancellation,
        )?;
        self.record_request(false);
        // Every non-confirming outcome — an error response, a read failure, a
        // cancellation — leaves the child in a state the parent did not observe:
        // on the mismatch path it parks its outgoing session and may evict from
        // the pool before the import fails, and the parent cannot tell which
        // keys survived. Forgetting all of them is the safe direction, because a
        // hint that under-predicts costs one admission check while one that
        // over-predicts skips one.
        //
        // Only on those paths, though. Clearing *before* the read — which was
        // right when this was a single `Option` being invalidated — would cap
        // the list at one key no matter its capacity, and a one-key hint is
        // exactly the mispredict-every-switch behaviour the list replaces.
        let response = match self.read_response_with_progress(OPERATION, progress, cancellation) {
            Ok(response) => response,
            Err(err) => {
                self.open_session_keys.clear();
                return Err(err);
            }
        };
        match response {
            Response::HostArtifactEpochChanged | Response::HeaderImportRecycle
                if self.generation_dispatch.is_some() =>
            {
                self.seal_generation_dispatch_internal();
                Err(LeanWorkerError::ShutdownInProgress { operation: OPERATION })
            }
            Response::HostArtifactEpochChanged if allow_retry => {
                self.host_read_active = false;
                // No body ran. A full process cycle invalidates every pooled
                // environment and process-global Lean cache after any writer.
                self.restart_for_request(LeanWorkerRestartReason::Explicit, OPERATION, cancellation)?;
                self.open_worker_session_attempt(config, cancellation, progress, false)
            }
            Response::HeaderImportRecycle if allow_retry => {
                let reason = self
                    .budget
                    .import_limit_reached(&self.config.restart_policy)
                    .unwrap_or_else(|| LeanWorkerRestartReason::ImportResidue {
                        residue_bytes: self.budget.import_residue_bytes,
                        limit_bytes: self.config.restart_policy.max_import_residue_bytes.unwrap_or(0),
                    });
                self.restart_for_request(reason, OPERATION, cancellation)?;
                self.open_worker_session_attempt(config, cancellation, progress, false)
            }
            Response::HostSessionOpened { import_stats } => {
                if expect_reuse && !self.reuse_observed && self.open_session_keys.last() == Some(config) {
                    // Predicted a reuse, got an import, this child has never
                    // reused anything, and the *immediately preceding* open used
                    // this very key: it is a pre-reuse worker binary. Stop
                    // predicting for this generation so the `max_imports` check
                    // is never skipped again.
                    //
                    // The consecutive-key condition is what keeps this from
                    // firing on a healthy child. Any other miss has an innocent
                    // explanation the hint should survive — an entry evicted for
                    // capacity, or one dropped because a later import registered
                    // an environment extension underneath it. Without the
                    // condition, a child whose first two profiles merely differ
                    // in modules disables its own hint during warm-up, after
                    // which every open is admitted as `import_like` and the pool
                    // is dead on arrival.
                    //
                    // A consecutive identical reopen is the one miss no
                    // eviction can produce: nothing imports between the two
                    // calls, so the session cannot have gone stale, and the LRU
                    // cannot have evicted the entry that is currently at its
                    // most-recently-used end.
                    self.reuse_hint_enabled = false;
                }
                self.record_import(&import_stats);
                let session_open_import_elapsed = import_started.elapsed();
                self.stats.last_session_open_import_elapsed = Some(session_open_import_elapsed);
                if self.stats.restarts > before_restarts
                    && let Some(timing) = self.stats.last_replacement_timing.as_mut()
                {
                    timing.session_open_import = session_open_import_elapsed;
                }
                self.stats.last_import_stats = Some(import_stats);
                self.note_open_session_key(config);
                Ok(())
            }
            Response::HostSessionReused { import_stats } => {
                // No import happened, so nothing that measures imports moves:
                // not `stats.imports`, not `imports_since_restart`, and not
                // `last_session_open_import_elapsed`, which would otherwise
                // start reporting a round trip as an import time.
                self.reuse_observed = true;
                self.stats.last_import_stats = Some(import_stats);
                self.note_open_session_key(config);
                Ok(())
            }
            other => {
                self.open_session_keys.clear();
                Err(unexpected_response(OPERATION, &other))
            }
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

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_verify_declaration_batch(
        &mut self,
        request: &LeanWorkerDeclarationVerificationBatchRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationBatchResult, LeanWorkerError> {
        const OPERATION: &str = "worker_verify_declaration_batch";
        let outcome = self.round_trip(
            OPERATION,
            Request::VerifyDeclarationBatch {
                request: request.clone(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationVerificationBatch { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        );
        match outcome {
            Ok(result) => Ok(result),
            Err(err) => {
                self.recover_child_abort(OPERATION, err)?;
                Ok(degraded_declaration_verification_batch_result(request))
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
        self.check_request_cancelled(OPERATION, cancellation)?;
        self.prepare_host_request(false, OPERATION, cancellation, progress)?;
        self.send_request_cancellable(
            Request::ListDeclarationsStrings {
                filter,
                progress: progress.is_some(),
            },
            OPERATION,
            cancellation,
        )?;
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
            return self.seal_dispatch_error(Err(LeanWorkerError::Protocol { message }));
        }
        let result = match response {
            Response::RowsComplete { count } => {
                let names = collector.into_inner();
                let observed = u64::try_from(names.len()).unwrap_or(u64::MAX);
                if observed != count {
                    return self.seal_dispatch_error(Err(LeanWorkerError::Protocol {
                        message: format!(
                            "worker_list_declarations_strings: parent collected {observed} rows but child reported {count}"
                        ),
                    }));
                }
                Ok(names)
            }
            other => Err(unexpected_response(OPERATION, &other)),
        };
        self.seal_dispatch_error(result)
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
        self.check_request_cancelled(OPERATION, cancellation)?;
        let encoded = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker data-stream request JSON encode failed: {err}"),
        });
        let request_json = self.seal_dispatch_error(encoded)?;
        self.prepare_host_request(false, OPERATION, cancellation, progress)?;
        self.send_request_cancellable(
            Request::RunDataStream {
                export: export.to_owned(),
                request_json,
                progress: progress.is_some(),
            },
            OPERATION,
            cancellation,
        )?;
        self.record_request(false);
        self.stats.stream_requests = self.stats.stream_requests.saturating_add(1);
        let result = match self.read_response_with_events(OPERATION, progress, cancellation, Some(rows), diagnostics)? {
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
        };
        self.seal_dispatch_error(result)
    }

    pub(crate) fn worker_host_runtime_metadata(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerHostRuntimeMetadata, LeanWorkerError> {
        const OPERATION: &str = "worker_host_runtime_metadata";
        // Session opening just established the generation. Do not recycle in
        // between its acknowledgement and this metadata-only inspection.
        self.check_request_cancelled(OPERATION, cancellation)?;
        self.send_request_cancellable(Request::HostRuntimeMetadata, OPERATION, cancellation)?;
        // This inspection is part of opening a session, not another body
        // request for max_requests/import policy. Charging it could force a
        // recycle immediately before the body whose session we just checked.
        let response = self.read_response_with_progress(OPERATION, progress, cancellation)?;
        let result = if let Response::HostRuntimeMetadata { metadata } = response {
            Ok(metadata)
        } else {
            Err(unexpected_response(OPERATION, &response))
        };
        self.seal_dispatch_error(result)
    }

    /// Close this child's read aliases after an observed terminal host operation.
    ///
    /// # Errors
    /// Refuses unknown in-flight work and retains ownership on protocol failure.
    pub fn release_host_read(&mut self) -> Result<(), LeanWorkerError> {
        if !self.host_read_active {
            return Ok(());
        }
        if !matches!(self.state, WorkerSupervisorState::Idle { .. }) || self.cleanup_pending() {
            self.seal_generation_dispatch_internal();
            return Err(LeanWorkerError::ShutdownInProgress {
                operation: "release_host_read",
            });
        }
        let result = (|| {
            self.send_request_cancellable(Request::ReleaseHostRead, "release_host_read", None)?;
            match self.read_response("release_host_read")? {
                Response::HostReadReleased => Ok(()),
                other => Err(unexpected_response("release_host_read", &other)),
            }
        })();
        if result.is_ok() {
            self.host_read_active = false;
        } else {
            self.state = WorkerSupervisorState::Crashed {
                generation: self.generation,
            };
        }
        self.seal_dispatch_error(result)
    }

    /// Last metadata validated for this generation; absent before a checked
    /// session or after any shutdown/replacement attempt.
    #[must_use]
    pub fn host_runtime_metadata(&self) -> Option<&LeanWorkerHostRuntimeMetadata> {
        self.host_runtime_metadata.as_ref()
    }

    pub(crate) fn worker_capability_metadata(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerCapabilityMetadata, LeanWorkerError> {
        let encoded = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability metadata request JSON encode failed: {err}"),
        });
        let request_json = self.seal_dispatch_error(encoded)?;
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
        let encoded = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability doctor request JSON encode failed: {err}"),
        });
        let request_json = self.seal_dispatch_error(encoded)?;
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

    fn send_request(&mut self, operation: &'static str, request: Request) -> Result<(), LeanWorkerError> {
        self.send_request_cancellable(request, operation, None)
    }

    fn send_request_cancellable(
        &mut self,
        request: Request,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        self.check_generation_dispatch(operation, Self::is_dispatch_cleanup(&request))?;
        self.check_request_cancelled(operation, cancellation)?;
        self.ensure_running()?;
        let deadline = cancellation.and_then(LeanWorkerCancellationToken::deadline);
        if deadline.is_none() {
            let admitted = self.begin_in_flight(operation);
            self.seal_dispatch_error(admitted)?;
            if !matches!(&request, Request::ReleaseHostRead) {
                // A later dispatched request supersedes a prior pre-body refusal.
                self.last_host_session_refusal = None;
            }
        }
        let started = Instant::now();
        let result = self.write_request_bounded(request, operation, cancellation, self.config.request_timeout);
        if let Err(error) = result {
            let reason = match &error {
                // The scoped writer handled any necessary stop using the actual byte count.
                LeanWorkerError::Timeout { .. } if deadline.is_some() => return Err(error),
                LeanWorkerError::Cancelled { resource, .. }
                    if deadline.is_some_and(|end| !resource.work_entered_child || Instant::now() >= end) =>
                {
                    return Err(error);
                }
                LeanWorkerError::Cancelled { .. } => LeanWorkerRestartReason::Cancelled { operation },
                LeanWorkerError::Timeout { duration, .. } => LeanWorkerRestartReason::RequestTimeout {
                    operation,
                    duration: *duration,
                },
                _ => {
                    self.finish_in_flight();
                    return Err(error);
                }
            };
            self.restart_with_reason(reason)?;
            return Err(error);
        }
        self.request_started = Some(started);
        Ok(())
    }

    fn write_request(&mut self, request: Request) -> Result<(), LeanWorkerError> {
        self.write_request_bounded(request, "shutdown_send", None, self.config.shutdown_timeout)
    }

    fn write_request_bounded(
        &mut self,
        request: Request,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        timeout: Duration,
    ) -> Result<(), LeanWorkerError> {
        let cleanup = Self::is_dispatch_cleanup(&request);
        if self.generation_dispatch.is_some() && !cleanup {
            self.check_request_cancelled(operation, cancellation)?;
        }
        let result = self.write_request_bounded_inner(request, operation, cancellation, timeout, None);
        self.seal_dispatch_error(result)
    }

    fn write_request_bounded_inner(
        &mut self,
        request: Request,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        timeout: Duration,
        managed_expiry_ns: Option<u64>,
    ) -> Result<(), LeanWorkerError> {
        // Keep the authoritative check beside the actual frame write, even if
        // another internal caller later reuses this bounded writer.
        self.check_generation_dispatch(operation, Self::is_dispatch_cleanup(&request))?;
        let deadline = cancellation.and_then(LeanWorkerCancellationToken::deadline);
        let planned = if deadline.is_some() {
            // ensure_running may have blocked. Check again before admitting a frame.
            self.check_request_cancelled(operation, cancellation)?;
            Some(self.plan_in_flight(operation)?)
        } else {
            None
        };
        let max_frame_bytes = self.config.max_frame_bytes;
        let Some(mut stdin) = self.stdin.take() else {
            return Err(self.dead_error());
        };
        let started = Instant::now();
        let timeout = frame_timeout(started, timeout, deadline);
        stdin.get_mut().deadline = started.checked_add(timeout);
        stdin.get_mut().cancellation = cancellation.cloned();
        stdin.get_mut().managed_expiry_ns = managed_expiry_ns;
        let (result, entered) = if let Some((in_flight, next)) = planned {
            let release = matches!(&request, Request::ReleaseHostRead);
            // Completed frames have flushed the buffer. The pipe checks the
            // token and deadline immediately before each real write, after encoding.
            debug_assert!(stdin.buffer().is_empty());
            let mut writer = ScopedFrameWriter {
                pipe: stdin.get_mut(),
                sent: false,
            };
            let result = write_frame(&mut writer, Message::Request(request), max_frame_bytes);
            if writer.sent {
                self.publish_in_flight(in_flight, next);
                if !release {
                    self.last_host_session_refusal = None;
                }
            }
            (result, writer.sent)
        } else {
            (
                write_frame(&mut stdin, Message::Request(request), max_frame_bytes),
                true,
            )
        };
        stdin.get_mut().deadline = None;
        stdin.get_mut().cancellation = None;
        stdin.get_mut().managed_expiry_ns = None;
        let cancelled = cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled);
        match result {
            Ok(()) => {
                self.stdin = Some(stdin);
                Ok(())
            }
            Err(lean_rs_worker_protocol::protocol::ProtocolError::Io(error)) => {
                self.seal_generation_dispatch_internal();
                if deadline.is_some() && !entered && error.kind() == io::ErrorKind::TimedOut {
                    self.stdin = Some(stdin);
                } else {
                    // Prevent BufWriter::drop from retrying a partial buffered write.
                    stdin.get_ref().stopped.store(true, Ordering::Release);
                    drop(stdin);
                    if deadline.is_some_and(|end| {
                        error.kind() == io::ErrorKind::TimedOut && (!cancelled || Instant::now() >= end)
                    }) {
                        self.stop_for_scoped_expiry();
                    } else {
                        self.shutdown_child(ShutdownIntent::KillOnly)?;
                    }
                }
                if cancelled {
                    Err(LeanWorkerError::Cancelled {
                        operation,
                        resource: Box::new(worker_resource_facts(
                            "worker_cancelled",
                            entered,
                            Some(operation),
                            &self.stats,
                            self.stats.last_rss_kib,
                            None,
                            None,
                        )),
                    })
                } else if error.kind() == std::io::ErrorKind::TimedOut {
                    Err(LeanWorkerError::Timeout {
                        operation,
                        duration: timeout,
                        resource: Box::new(worker_resource_facts(
                            "worker_timeout",
                            entered,
                            Some(operation),
                            &self.stats,
                            self.stats.last_rss_kib,
                            None,
                            Some(timeout),
                        )),
                    })
                } else {
                    Err(LeanWorkerError::Protocol {
                        message: error.to_string(),
                    })
                }
            }
            Err(error) => {
                // Encoding/frame-size errors occur before any bytes are written.
                self.stdin = Some(stdin);
                Err(LeanWorkerError::Protocol {
                    message: error.to_string(),
                })
            }
        }
    }

    fn interrupt_framed_request(
        &mut self,
        scoped_frame_deadline: Option<Instant>,
        reason: LeanWorkerRestartReason,
        release_reader: impl FnOnce(),
    ) -> Result<(), LeanWorkerError> {
        if scoped_frame_deadline.is_some_and(|end| {
            matches!(reason, LeanWorkerRestartReason::RequestTimeout { .. }) || Instant::now() >= end
        }) {
            self.stop_for_scoped_expiry();
            release_reader();
            Ok(())
        } else {
            self.restart_with_reason_before_spawn(reason, release_reader)
        }
    }

    fn stop_for_scoped_expiry(&mut self) {
        // The operation has timed out even if cleanup cannot confirm exit.
        // Keep that case fenced and observable via cleanup_pending and the request's stop receipt.
        self.state = WorkerSupervisorState::Killing {
            generation: self.generation,
        };
        drop(self.shutdown_child(ShutdownIntent::KillOnly));
    }

    fn begin_in_flight(&mut self, operation: &'static str) -> Result<InFlightRequest, LeanWorkerError> {
        let (request, next) = self.plan_in_flight(operation)?;
        self.publish_in_flight(request.clone(), next);
        Ok(request)
    }

    fn plan_in_flight(&self, operation: &'static str) -> Result<(InFlightRequest, WorkerRequestId), LeanWorkerError> {
        let next_request_id = self.next_request_id.next().ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker request identifier space exhausted before dispatch".to_owned(),
        })?;
        let process_id = self
            .child
            .as_ref()
            .map(|child| child.id())
            .ok_or_else(|| self.dead_error())?;
        let request = InFlightRequest {
            id: self.next_request_id,
            operation,
            generation: self.generation,
            process_id,
        };
        Ok((request, next_request_id))
    }

    fn publish_in_flight(&mut self, request: InFlightRequest, next_request_id: WorkerRequestId) {
        self.last_request_exit = self.child.as_ref().map(|child| LeanWorkerPendingRequestStop {
            request: request.clone(),
            exit: child.exit_observation(),
        });
        self.next_request_id = next_request_id;
        self.state = WorkerSupervisorState::Busy {
            request: request.clone(),
        };
        self.last_request = Some(request);
    }

    fn await_in_flight(&mut self, operation: &'static str) -> Result<InFlightRequest, LeanWorkerError> {
        let mut request = match &self.state {
            WorkerSupervisorState::Busy { request } => request.clone(),
            _ => {
                return Err(LeanWorkerError::Protocol {
                    message: format!("cannot await {operation} without a dispatched request"),
                });
            }
        };
        request.operation = operation;
        if let Some(pending) = self.last_request_exit.as_mut()
            && pending.request.id == request.id
            && pending.request.generation == request.generation
            && pending.request.process_id == request.process_id
        {
            pending.request = request.clone();
        }
        self.last_request = Some(request.clone());
        self.state = WorkerSupervisorState::Busy {
            request: request.clone(),
        };
        Ok(request)
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

    fn prepare_host_request(
        &mut self,
        import_like: bool,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<(), LeanWorkerError> {
        let config = self.open_session_keys.last().cloned();
        let generation = self.stats.restarts;
        self.prepare_request(import_like, operation, cancellation)?;
        if (self.stats.restarts != generation || (self.config.shared_artifact_reads && !self.host_read_active))
            && let Some(config) = config
        {
            self.open_worker_session(&config, cancellation, progress)?;
        }
        Ok(())
    }

    fn prepare_request(
        &mut self,
        import_like: bool,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        self.check_request_cancelled(operation, cancellation)?;
        self.check_generation_dispatch("worker_request", false)?;
        self.ensure_running()?;
        if self.budget.accounting_unknown {
            self.restart_for_request(
                LeanWorkerRestartReason::ImportAccountingUnknown,
                operation,
                cancellation,
            )?;
        }
        if import_like {
            self.stats.import_like_admission_attempts = self.stats.import_like_admission_attempts.saturating_add(1);
            self.stats.last_import_like_rss_before_admission_kib = self.child_rss_kib();
        }

        if let Some(reason) = self.budget.request_limit_reached(&self.config.restart_policy) {
            self.restart_for_request(reason, operation, cancellation)?;
            if import_like {
                self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
            }
            return Ok(());
        }

        if import_like && let Some(reason) = self.budget.import_limit_reached(&self.config.restart_policy) {
            self.restart_for_request(reason, operation, cancellation)?;
            self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
            return Ok(());
        }

        if let Some(limit_kib) = self.config.restart_policy.max_rss_kib {
            match self.child_rss_kib() {
                Some(current_kib) if current_kib >= limit_kib => {
                    self.stats.last_rss_kib = Some(current_kib);
                    self.restart_for_request(
                        LeanWorkerRestartReason::RssCeiling {
                            current_kib,
                            limit_kib,
                            last_import_stats: self.stats.last_import_stats.clone(),
                        },
                        operation,
                        cancellation,
                    )?;
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
                self.restart_for_request(
                    LeanWorkerRestartReason::Idle { idle_for, limit },
                    operation,
                    cancellation,
                )?;
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
        if import_like {
            self.stats.imports = self.stats.imports.saturating_add(1);
        }
        self.budget.record_request(import_like);
        self.last_activity = Instant::now();
    }

    /// Count an import that the child confirmed it performed, and charge its
    /// unreclaimable bytes to the current generation.
    ///
    /// Separate from [`Self::record_request`] because a session open is a
    /// request whether or not it imports: only the child's answer says which
    /// happened, and it arrives after the request is already recorded. That
    /// answer carries the import's own attribution, which is the only reason
    /// this can be denominated in bytes at all — no `ps` fork, no sampling, and
    /// nothing that counts the clean `.olean` pages an RSS reading is dominated
    /// by. A reused session reports the same stats but performed no import, so
    /// it charges nothing; the caller only reaches here on `HostSessionOpened`.
    fn record_import(&mut self, import_stats: &LeanWorkerImportStats) {
        self.stats.imports = self.stats.imports.saturating_add(1);
        // Modules came in but no region bytes were attributed to them: the shim
        // is not reporting what this policy is denominated in. Count it rather
        // than guessing a figure, and let `max_imports` be the live bound.
        if import_stats.effective_module_count > 0 && import_stats.compacted_region_bytes == 0 {
            self.stats.import_stats_unusable = self.stats.import_stats_unusable.saturating_add(1);
        }
        self.budget.record_import(import_stats);
    }

    fn restart_for_request(
        &mut self,
        reason: LeanWorkerRestartReason,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        let scope = cancellation
            .filter(|token| token.deadline().is_some())
            .map(|token| (operation, token));
        self.restart_with_reason_before_spawn_scoped_using(reason, || {}, scope, Self::spawn_tracked_scoped)
    }

    fn restart_with_reason(&mut self, reason: LeanWorkerRestartReason) -> Result<(), LeanWorkerError> {
        self.restart_with_reason_before_spawn(reason, || {})
    }

    fn restart_with_reason_before_spawn(
        &mut self,
        reason: LeanWorkerRestartReason,
        before_spawn: impl FnOnce(),
    ) -> Result<(), LeanWorkerError> {
        self.restart_with_reason_before_spawn_scoped_using(reason, before_spawn, None, Self::spawn_tracked_scoped)
    }

    #[cfg(test)]
    fn restart_with_reason_before_spawn_using(
        &mut self,
        reason: LeanWorkerRestartReason,
        before_spawn: impl FnOnce(),
        spawn: impl FnOnce(&LeanWorkerConfig) -> Result<Self, SpawnFailure>,
    ) -> Result<(), LeanWorkerError> {
        self.restart_with_reason_before_spawn_scoped_using(reason, before_spawn, None, |config, _| spawn(config))
    }

    fn restart_with_reason_before_spawn_scoped_using(
        &mut self,
        reason: LeanWorkerRestartReason,
        before_spawn: impl FnOnce(),
        scope: Option<StartupScope<'_>>,
        spawn: impl FnOnce(&LeanWorkerConfig, Option<StartupScope<'_>>) -> Result<Self, SpawnFailure>,
    ) -> Result<(), LeanWorkerError> {
        if self.generation_dispatch.is_some() {
            self.seal_generation_dispatch_internal();
            let interrupt = matches!(
                reason,
                LeanWorkerRestartReason::Cancelled { .. }
                    | LeanWorkerRestartReason::RequestTimeout { .. }
                    | LeanWorkerRestartReason::RssHardLimit { .. }
                    | LeanWorkerRestartReason::ChildAbort { .. }
            );
            if interrupt {
                // Interrupt callers return their original failure after this
                // stop-only success; it never means a replacement is ready.
                // Preserve cleanup failures instead of hiding unresolved work.
                let stopped = self.shutdown_child(ShutdownIntent::KillOnly);
                before_spawn();
                stopped?;
                return Ok(());
            }
            // Policy/explicit cycling is refused, with no spawn or unnecessary
            // stop. The callback still owns any captured reader cleanup.
            before_spawn();
            return Err(LeanWorkerError::ShutdownInProgress {
                operation: "generation_dispatch",
            });
        }
        let config = self.config.clone();
        let replacement_started = Instant::now();
        self.stats.replacement_attempts = self.stats.replacement_attempts.saturating_add(1);
        let stop_intent = if matches!(
            &reason,
            LeanWorkerRestartReason::Explicit
                | LeanWorkerRestartReason::MaxRequests { .. }
                | LeanWorkerRestartReason::MaxImports { .. }
                | LeanWorkerRestartReason::ImportResidue { .. }
                | LeanWorkerRestartReason::ImportAccountingUnknown
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
        // Cleanup belongs to the retired generation even after the request
        // expires. A successor is new work and cannot spend that budget again.
        if let Some((operation, token)) = scope
            && let Err(error) = check_cancelled(operation, Some(token))
        {
            self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
            self.stats.last_replacement_skipped_reason = Some("request_scope_expired".to_owned());
            return Err(error);
        }
        if let Err(err) = self.admit_restart(replacement_started) {
            self.stats.replacement_budget_skipped = self.stats.replacement_budget_skipped.saturating_add(1);
            self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
            self.stats.last_replacement_skipped_reason = Some("restart_limit_exceeded".to_owned());
            return Err(err);
        }
        self.stats.replacement_budget_admitted = self.stats.replacement_budget_admitted.saturating_add(1);
        let next_generation = self.generation.next().ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker generation identifier space exhausted before replacement".to_owned(),
        })?;
        self.stats.record_restart(reason);
        self.budget = GenerationBudget::default();
        let reason = self
            .stats
            .last_restart_reason
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), |reason| reason.stable_cause().to_owned());
        let mut next = match spawn(&config, scope) {
            Ok(next) => next,
            Err(failure) => {
                self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
                self.stats.last_replacement_skipped_reason = Some("spawn_failed".to_owned());
                self.pending_startup = failure.pending;
                if let Some(exit) = failure.exit {
                    self.last_exit = Some(*exit);
                    self.stats.exits = self.stats.exits.saturating_add(1);
                }
                return Err(failure.error);
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
        next.request_scope = Arc::clone(&self.request_scope);
        next.last_request.clone_from(&self.last_request);
        next.last_request_stop.clone_from(&self.last_request_stop);
        next.last_request_exit.clone_from(&self.last_request_exit);
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
        if self.generation_dispatch.is_some() {
            self.seal_generation_dispatch_internal();
            return Err(err);
        }
        if let LeanWorkerError::ChildPanicOrAbort { exit } = &err {
            self.restart_with_reason(child_abort_reason(operation, exit))
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
        self.check_request_cancelled(operation, cancellation)?;
        let session_config = self.open_session_keys.last().cloned();
        self.prepare_host_request(import_like, operation, cancellation, progress)?;
        self.send_request_cancellable(request.clone(), operation, cancellation)?;
        self.record_request(import_like);
        let mut response = self.read_response_with_progress(operation, progress, cancellation)?;
        if matches!(response, Response::HeaderImportRecycle) {
            if self.generation_dispatch.is_some() {
                self.seal_generation_dispatch_internal();
                return Err(LeanWorkerError::ShutdownInProgress { operation });
            }
            let config = session_config.ok_or_else(|| LeanWorkerError::Protocol {
                message: "header recycle requested without an active session".to_owned(),
            })?;
            let reason = if self.budget.accounting_unknown {
                LeanWorkerRestartReason::ImportAccountingUnknown
            } else {
                LeanWorkerRestartReason::ImportResidue {
                    residue_bytes: self.budget.import_residue_bytes,
                    limit_bytes: self.config.restart_policy.max_import_residue_bytes.unwrap_or(0),
                }
            };
            self.restart_for_request(reason, operation, cancellation)?;
            self.open_worker_session(&config, cancellation, progress)?;
            // Exactly one retry: the new generation admits its first necessary
            // header even when that header alone exceeds the byte budget.
            self.check_request_cancelled(operation, cancellation)?;
            self.send_request_cancellable(request, operation, cancellation)?;
            self.record_request(import_like);
            response = self.read_response_with_progress(operation, progress, cancellation)?;
        }
        let result = extract(response, operation);
        self.seal_dispatch_error(result)
    }

    fn read_response_with_events(
        &mut self,
        operation: &'static str,
        progress: Option<&dyn LeanWorkerProgressSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        data: Option<LeanWorkerDataSinkTarget<'_>>,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
    ) -> Result<Response, LeanWorkerError> {
        let result = self.read_response_with_events_inner(operation, progress, cancellation, data, diagnostics);
        self.seal_dispatch_error(result)
    }

    fn read_response_with_events_inner(
        &mut self,
        operation: &'static str,
        progress: Option<&dyn LeanWorkerProgressSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        data: Option<LeanWorkerDataSinkTarget<'_>>,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
    ) -> Result<Response, LeanWorkerError> {
        let started = self.request_started.take().unwrap_or_else(Instant::now);
        let operation_deadline = cancellation.and_then(LeanWorkerCancellationToken::deadline);
        let timeout = frame_timeout(started, self.config.request_timeout, operation_deadline);
        let deadline = started.checked_add(timeout);
        let scoped_frame_deadline = operation_deadline.and(deadline);
        #[cfg(all(test, unix))]
        let mut deadline = deadline;
        let streaming = data.is_some();
        let mut request_backpressure_waits = 0_u64;
        let stdout = self.stdout.take().ok_or_else(|| self.dead_error())?;
        let reader_stop = Arc::clone(&stdout.get_ref().stopped);
        reader_stop.store(false, Ordering::Release);
        let max_frame_bytes = self.config.max_frame_bytes;
        let request = self.await_in_flight(operation)?;
        let generation = request.generation;
        let request_id = Some(request.id);
        let (sender, receiver) = mpsc::sync_channel(WORKER_EVENT_BUFFER_CAPACITY);
        let reader =
            thread::spawn(move || read_request_messages(stdout, sender, max_frame_bytes, generation, request_id));
        let reader = ReaderGuard {
            stopped: reader_stop,
            thread: Some(reader),
        };

        loop {
            #[cfg(unix)]
            if self.child.as_mut().is_some_and(OwnedChild::poll_guardian) {
                self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                drop(receiver);
                drop(reader);
                self.shutdown_child(ShutdownIntent::KillOnly)?;
                return Err(LeanWorkerError::Setup {
                    message: "native guardian failed; owned request group stopped without a proof result".to_owned(),
                });
            }
            if cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
                if streaming {
                    self.record_stream_failure(started, request_backpressure_waits);
                }
                if let Err(err) = self.interrupt_framed_request(
                    scoped_frame_deadline,
                    LeanWorkerRestartReason::Cancelled { operation },
                    || {
                        drop(receiver);
                        drop(reader);
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
            if let Some((current_kib, limit_kib)) = self.hard_rss_limit_exceeded()
                && scoped_frame_deadline.is_none_or(|end| Instant::now() < end)
            {
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
                        drop(reader);
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
            let event = match deadline.map(|deadline| deadline.saturating_duration_since(Instant::now())) {
                Some(remaining) if remaining.is_zero() => {
                    if streaming {
                        self.record_stream_failure(started, request_backpressure_waits);
                    }
                    if let Err(err) = self.interrupt_framed_request(
                        scoped_frame_deadline,
                        LeanWorkerRestartReason::RequestTimeout {
                            operation,
                            duration: timeout,
                        },
                        || {
                            drop(receiver);
                            drop(reader);
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
                    let mut wait_for = if hard_watch_enabled {
                        remaining.min(self.config.rss_sample_interval)
                    } else {
                        remaining
                    };
                    if cancellation.is_some() {
                        wait_for = wait_for.min(CANCELLATION_POLL_INTERVAL);
                    }
                    #[cfg(unix)]
                    if self.child.as_ref().is_some_and(OwnedChild::has_guardian) {
                        wait_for = wait_for.min(CANCELLATION_POLL_INTERVAL);
                    }
                    match receiver.recv_timeout(wait_for) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout)
                            if scoped_frame_deadline.is_some() || wait_for < remaining =>
                        {
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if streaming {
                                self.record_stream_failure(started, request_backpressure_waits);
                            }
                            if let Err(err) = self.interrupt_framed_request(
                                scoped_frame_deadline,
                                LeanWorkerRestartReason::RequestTimeout {
                                    operation,
                                    duration: timeout,
                                },
                                || {
                                    drop(receiver);
                                    drop(reader);
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
                            drop(reader);
                            return Err(LeanWorkerError::Protocol {
                                message: "worker response reader exited without a terminal response".to_owned(),
                            });
                        }
                    }
                }
                None if cancellation.is_some() || {
                    #[cfg(unix)]
                    {
                        self.child.as_ref().is_some_and(OwnedChild::has_guardian)
                    }
                    #[cfg(not(unix))]
                    {
                        false
                    }
                } =>
                {
                    match receiver.recv_timeout(CANCELLATION_POLL_INTERVAL) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            self.finish_in_flight();
                            drop(reader);
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
                        drop(reader);
                        return Err(LeanWorkerError::Protocol {
                            message: "worker response reader exited without a terminal response".to_owned(),
                        });
                    }
                },
            };
            #[cfg(all(test, unix))]
            let event = dispatch_pin_tests::observe_fixture_terminal(event, &mut deadline);
            // A response can already be queued when the parent resumes after expiry.
            // Let the existing interruption paths resolve expiry and scoped cancellation.
            if deadline.is_some_and(|end| Instant::now() >= end)
                || (scoped_frame_deadline.is_some()
                    && cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled))
            {
                continue;
            }
            let stale = if event.generation() != self.generation {
                Some(stale_worker_output_error(
                    operation,
                    self.generation,
                    event.generation(),
                ))
            } else if let Some(actual) = event.request_id()
                && self.state.current_request_id() != Some(actual)
            {
                Some(stale_worker_request_output_error(
                    operation,
                    self.state.current_request_id(),
                    actual,
                ))
            } else {
                None
            };
            if let Some(error) = stale {
                // This reader cannot provide a trusted terminal response, even for shutdown.
                // Fence dispatch now; the owner reports the independent stop result at shutdown.
                self.state = WorkerSupervisorState::Killing {
                    generation: self.generation,
                };
                drop(receiver);
                drop(reader);
                return Err(error);
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
                            drop(reader);
                            return Err(LeanWorkerError::Worker { code, message });
                        }
                        Message::Response(response) => {
                            let response = self.terminalize_request_response(
                                response,
                                streaming,
                                started,
                                request_backpressure_waits,
                            );
                            drop(reader);
                            return Ok(response);
                        }
                        other => {
                            self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                            drop(reader);
                            return Err(LeanWorkerError::Protocol {
                                message: format!("worker sent unexpected {operation} message: {other:?}"),
                            });
                        }
                    }
                }
                RequestReaderEvent::ReadError { message, eof, .. } => {
                    drop(reader);
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    return if eof {
                        Err(self.record_exit_error())
                    } else {
                        self.shutdown_child(ShutdownIntent::KillOnly)?;
                        Err(LeanWorkerError::Protocol { message })
                    };
                }
            };

            match message {
                Message::HeaderImportState {
                    bytes,
                    imports,
                    accounting_unknown,
                } => {
                    self.budget.record_header_imports(bytes, imports, accounting_unknown);
                }
                Message::ProgressTick(tick) => {
                    self.mark_current_request_streaming();
                    if let Err(err) =
                        report_parent_progress(progress, elapsed_event(tick.phase, tick.current, tick.total, started))
                    {
                        self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                        self.shutdown_child(ShutdownIntent::KillOnly)?;
                        return Err(err);
                    }
                    if cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
                        if streaming {
                            self.record_stream_failure(started, request_backpressure_waits);
                        }
                        if let Err(err) = self.interrupt_framed_request(
                            scoped_frame_deadline,
                            LeanWorkerRestartReason::Cancelled { operation },
                            || {
                                drop(receiver);
                                drop(reader);
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
                        self.shutdown_child(ShutdownIntent::KillOnly)?;
                        return Err(err);
                    }
                    self.stats.data_rows_delivered = self.stats.data_rows_delivered.saturating_add(1);
                    self.stats.data_row_payload_bytes = self.stats.data_row_payload_bytes.saturating_add(payload_bytes);
                    if cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
                        if streaming {
                            self.record_stream_failure(started, request_backpressure_waits);
                        }
                        if let Err(err) = self.interrupt_framed_request(
                            scoped_frame_deadline,
                            LeanWorkerRestartReason::Cancelled { operation },
                            || {
                                drop(receiver);
                                drop(reader);
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
                        self.shutdown_child(ShutdownIntent::KillOnly)?;
                        return Err(err);
                    }
                }
                Message::Response(response) => {
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    self.shutdown_child(ShutdownIntent::KillOnly)?;
                    return Err(unexpected_response(operation, &response));
                }
                other => {
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    self.shutdown_child(ShutdownIntent::KillOnly)?;
                    return Err(LeanWorkerError::Protocol {
                        message: format!("worker sent unexpected {operation} message: {other:?}"),
                    });
                }
            }
        }
    }

    fn ensure_running(&mut self) -> Result<(), LeanWorkerError> {
        let result = self.ensure_running_inner();
        self.seal_dispatch_error(result)
    }

    fn ensure_running_inner(&mut self) -> Result<(), LeanWorkerError> {
        if self.state.rejects_new_requests() {
            return Err(LeanWorkerError::ShutdownInProgress {
                operation: self.state.current_operation().unwrap_or("worker_request"),
            });
        }
        match self.status()? {
            LeanWorkerStatus::Running if self.cleanup_pending() => Err(LeanWorkerError::ShutdownInProgress {
                operation: self.state.current_operation().unwrap_or("worker_request"),
            }),
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
        if matches!(
            &response,
            Response::HeaderImportRecycle
                | Response::HostArtifactEpochChanged
                | Response::StreamExportFailed { .. }
                | Response::StreamCallbackFailed { .. }
                | Response::StreamRowMalformed { .. }
        ) {
            self.seal_generation_dispatch_internal();
        }
        if streaming {
            if matches!(
                response,
                Response::StreamComplete { .. } | Response::RowsComplete { .. }
            ) {
                self.record_stream_success(started);
            } else {
                self.record_stream_failure(started, backpressure_waits);
            }
        }
        self.finish_in_flight();
        response
    }

    fn terminalize_request_failure(&mut self, streaming: bool, started: Instant, backpressure_waits: u64) {
        self.seal_generation_dispatch_internal();
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
        match self.wait_for_exit_bounded("stdout_closed", self.config.shutdown_timeout) {
            Ok((exit, _)) => Ok(exit),
            Err(LeanWorkerError::WaitTimeout { .. }) => {
                self.shutdown_child(ShutdownIntent::KillOnly).map(|report| report.exit)
            }
            Err(error) => Err(error),
        }
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
        if let (Some(child), Some(request)) = (&self.child, &self.last_request)
            && request.generation == self.generation
            && request.process_id == child.id()
        {
            self.last_request_stop = Some(LeanWorkerRequestStopReceipt {
                request: request.clone(),
                exit: exit.clone(),
            });
        }
        self.last_exit = Some(exit.clone());
        self.child = None;
        self.native_generation_id = None;
        self.seal_generation_dispatch_internal();
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
        self.shutdown_child_with_cleanup(intent, StartupChild::cleanup)
    }

    fn shutdown_child_with_cleanup(
        &mut self,
        intent: ShutdownIntent,
        cleanup: impl FnOnce(&mut StartupChild) -> Result<ExitStatus, LeanWorkerError>,
    ) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        self.seal_generation_dispatch_internal();
        self.host_runtime_metadata = None;
        let started = Instant::now();
        let graceful_timeout = self.config.shutdown_timeout;
        if let Some(mut pending) = self.pending_startup.take() {
            let status = match cleanup(&mut pending) {
                Ok(status) => status,
                Err(error) => {
                    self.pending_startup = Some(pending);
                    return Err(error);
                }
            };
            let exit = self.finalize_child_exit(status);
            return Ok(LeanWorkerShutdownReport {
                outcome: LeanWorkerShutdownOutcome::KillOnly,
                exit,
                graceful_timeout,
                elapsed: started.elapsed(),
                kill_elapsed: Some(started.elapsed()),
                wait_elapsed: started.elapsed(),
            });
        }
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

        if matches!(self.state, WorkerSupervisorState::Killing { .. }) && self.stdout.is_none() {
            return self.kill_and_report(started, graceful_timeout, LeanWorkerShutdownOutcome::KillOnly);
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
                Err(LeanWorkerError::Timeout { .. }) => {
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
        let reader_stop = Arc::clone(&stdout.get_ref().stopped);
        reader_stop.store(false, Ordering::Release);
        let max_frame_bytes = self.config.max_frame_bytes;
        let generation = self.generation;
        let (sender, receiver) = mpsc::sync_channel(WORKER_EVENT_BUFFER_CAPACITY);
        let reader = thread::spawn(move || read_request_messages(stdout, sender, max_frame_bytes, generation, None));
        let reader = ReaderGuard {
            stopped: reader_stop,
            thread: Some(reader),
        };

        loop {
            if let Some(child) = self.child.as_mut()
                && let Some(status) = child.try_wait().map_err(|source| LeanWorkerError::Wait { source })?
            {
                drop(receiver);
                drop(reader);
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
                drop(reader);
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
                    drop(reader);
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
                    drop(reader);
                    self.stdout = Some(stdout);
                    return self.kill_and_report(
                        started,
                        graceful_timeout,
                        LeanWorkerShutdownOutcome::GracefulProtocolFailedKilled,
                    );
                }
                Ok(RequestReaderEvent::ReadError { eof: true, .. }) => {
                    drop(reader);
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
                    drop(reader);
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
        if self.child.is_none()
            && self.pending_startup.is_none()
            && let Some(exit) = self.last_exit.clone()
        {
            return Ok(LeanWorkerShutdownReport {
                outcome,
                exit,
                graceful_timeout,
                elapsed: started.elapsed(),
                kill_elapsed: Some(started.elapsed()),
                wait_elapsed: Duration::ZERO,
            });
        }
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
        if self.pending_startup.is_some() {
            return LeanWorkerError::Setup {
                message: "replacement child exit remains unconfirmed; structured shutdown is required".to_owned(),
            };
        }
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
        self.stderr.take().map_or_else(String::new, StderrCapture::finish)
    }

    fn child_rss_kib(&mut self) -> Option<u64> {
        let child = self.child.as_mut()?;
        child_rss_kib(child.id())
    }

    fn child_cpu_millis(&mut self) -> Option<u64> {
        let child = self.child.as_mut()?;
        child_cpu_millis(child.id())
    }
}

enum RequestReaderEvent {
    Message {
        generation: WorkerGeneration,
        request_id: Option<WorkerRequestId>,
        message: Message,
        backpressure_waits: u64,
    },
    Terminal {
        generation: WorkerGeneration,
        request_id: Option<WorkerRequestId>,
        message: Message,
        stdout: BufReader<Pipe<ChildStdout>>,
        backpressure_waits: u64,
    },
    ReadError {
        generation: WorkerGeneration,
        request_id: Option<WorkerRequestId>,
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

    fn request_id(&self) -> Option<WorkerRequestId> {
        match self {
            Self::Message { request_id, .. }
            | Self::Terminal { request_id, .. }
            | Self::ReadError { request_id, .. } => *request_id,
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
    mut stdout: BufReader<Pipe<ChildStdout>>,
    sender: mpsc::SyncSender<RequestReaderEvent>,
    max_frame_bytes: u32,
    generation: WorkerGeneration,
    request_id: Option<WorkerRequestId>,
) {
    let stopped = Arc::clone(&stdout.get_ref().stopped);
    loop {
        match read_frame(&mut stdout, max_frame_bytes) {
            Ok(frame) if matches!(frame.message, Message::Response(_)) => {
                let _ = send_reader_event(
                    &sender,
                    &stopped,
                    RequestReaderEvent::Terminal {
                        generation,
                        request_id,
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
                    &stopped,
                    RequestReaderEvent::Message {
                        generation,
                        request_id,
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
                    &stopped,
                    RequestReaderEvent::ReadError {
                        generation,
                        request_id,
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

fn send_reader_event(
    sender: &mpsc::SyncSender<RequestReaderEvent>,
    stopped: &AtomicBool,
    mut event: RequestReaderEvent,
) -> Result<(), ()> {
    loop {
        if stopped.load(Ordering::Acquire) {
            return Err(());
        }
        match sender.try_send(event) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(mut returned)) => {
                returned.add_backpressure_wait();
                event = returned;
                thread::sleep(Duration::from_millis(5));
            }
            Err(mpsc::TrySendError::Disconnected(_event)) => return Err(()),
        }
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
    stdout: &mut BufReader<Pipe<ChildStdout>>,
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

fn wait_with_stderr(
    child: &mut StartupChild,
    stderr: Option<StderrCapture>,
) -> Result<LeanWorkerExit, LeanWorkerError> {
    let status = child.cleanup()?;
    let diagnostics = stderr.map_or_else(String::new, StderrCapture::finish);
    Ok(LeanWorkerExit::from_status(status, diagnostics))
}

/// Cap so a single `Display` line cannot blow up downstream log/telemetry
/// pipelines that capture `err.to_string()` (`tracing::error!("{err}")`,
/// JSON log shippers). The full `exit.diagnostics` text is still available on
/// the field; this only limits what crosses the `Display` surface.
const DISPLAY_DIAGNOSTICS_MAX_BYTES: usize = 4 * 1024;

/// Carry the one record of *why* a child died into the restart reason.
///
/// The child's stderr is drained continuously into a bounded tail and captured
/// when the supervisor reaps it. A consumer that sees only `stable_cause()` cannot tell
/// a Lean out-of-memory abort from a segfault from a protocol bug — which is the
/// difference between raising a limit and filing a bug. Truncated on the same
/// budget as the `Display` rendering, since the reason is cloned onto every
/// lifetime snapshot that follows it.
fn child_abort_reason(operation: &'static str, exit: &LeanWorkerExit) -> LeanWorkerRestartReason {
    LeanWorkerRestartReason::ChildAbort {
        operation,
        status: exit.status.clone(),
        diagnostics: truncate_for_display(exit.diagnostics.trim(), DISPLAY_DIAGNOSTICS_MAX_BYTES),
    }
}

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

fn degraded_declaration_verification_batch_result(
    request: &LeanWorkerDeclarationVerificationBatchRequest,
) -> LeanWorkerDeclarationVerificationBatchResult {
    LeanWorkerDeclarationVerificationBatchResult::Ok {
        results: request
            .targets
            .iter()
            .map(|target| LeanWorkerDeclarationVerificationBatchRow {
                id: target.id.clone(),
                target: target.target.clone(),
                verification_status: LeanWorkerDeclarationVerificationStatus::BudgetExceeded,
                facts: Box::new(LeanWorkerDeclarationVerificationFacts::unavailable()),
            })
            .collect(),
        imports: Vec::new(),
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

fn stale_worker_request_output_error(
    operation: &'static str,
    expected: Option<WorkerRequestId>,
    actual: WorkerRequestId,
) -> LeanWorkerError {
    let expected = expected.map_or_else(|| "none".to_owned(), |request_id| request_id.0.to_string());
    LeanWorkerError::Protocol {
        message: format!(
            "worker sent stale {operation} frame from request {}, current request is {expected}",
            actual.0
        ),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

struct FixtureManifest {
    path: PathBuf,
    #[cfg(unix)]
    _read: lean_toolchain::shared_writer::LocalArtifactRead,
}

fn fixture_capability_manifest(fixture_root: &Path) -> Result<FixtureManifest, LeanWorkerError> {
    probe_fixture_manifest(fixture_root).map_err(|diagnostic| LeanWorkerError::CapabilityBuild { diagnostic })
}

fn probe_fixture_manifest(fixture_root: &Path) -> Result<FixtureManifest, lean_toolchain::LinkDiagnostics> {
    let failure = |reason: String| lean_toolchain::LinkDiagnostics::LakeOutputUnresolved {
        project_root: fixture_root.to_path_buf(),
        target_name: "LeanRsFixture".into(),
        reason,
    };
    #[cfg(not(unix))]
    return Err(failure("fixture read custody is unsupported on this platform".into()));

    #[cfg(unix)]
    {
        use lean_toolchain::shared_writer::{LocalArtifactRead, project_artifact_roots, writer_executable_from_env};
        let roots = project_artifact_roots(fixture_root).map_err(|error| failure(error.to_string()))?;
        let builder = || {
            lean_toolchain::CargoLeanCapability::new(fixture_root, "LeanRsFixture")
                .package("lean_rs_fixture")
                .module("LeanRsFixture")
                .export_signature(fixture_mul_signature("lean_rs_fixture_u64_mul"))
                .export_signature(fixture_panic_signature("lean_rs_fixture_panic_unit"))
        };
        let ready = || -> std::io::Result<Option<FixtureManifest>> {
            let read = LocalArtifactRead::try_acquire(&roots)?;
            if project_artifact_roots(fixture_root)? != roots {
                return Err(std::io::Error::other(
                    "fixture artifact roots changed under read custody",
                ));
            }
            let built = builder().probe_ready_strict().map_err(std::io::Error::other)?;
            read.validate()?;
            Ok(built.map(|built| FixtureManifest {
                path: built.manifest_path().to_path_buf(),
                _read: read,
            }))
        };
        match ready() {
            Ok(Some(manifest)) => return Ok(manifest),
            Ok(None) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A new project may not yet have locks. A lost lock beside
                // an SDK cache is not cold; refuse instead of repairing it.
                match std::fs::symlink_metadata(fixture_root.join(".lake/lean-rs-build-cache")) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err(failure(error.to_string())),
                }
            }
            Err(error) => return Err(failure(error.to_string())),
        }
        if writer_executable_from_env()
            .map_err(|error| failure(error.to_string()))?
            .is_none()
        {
            return Err(failure(
                "fixture cache is not ready; a configured writer is required".into(),
            ));
        }
        // No reader is held while the existing writer builds a cold fixture.
        drop(builder().build_quiet()?);
        ready()
            .map_err(|error| failure(error.to_string()))?
            .ok_or_else(|| failure("fixture cache is not ready after the writer returned".into()))
    }
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

/// Cumulative child CPU time (user + system) in milliseconds.
///
/// Sampling this before and after a unit of work gives a contention-immune
/// cost measure: unlike wallclock it does not inflate when the child waits for
/// a busy CPU, and unlike `getrusage(RUSAGE_CHILDREN)` it works on a live,
/// long-lived child. A `None` result means the platform did not provide a
/// usable sample; it is not a worker failure.
#[cfg(target_os = "linux")]
fn child_cpu_millis(pid: u32) -> Option<u64> {
    // /proc/<pid>/stat: fields 14 (utime) and 15 (stime), in clock ticks. The
    // comm field (2) may contain spaces and parentheses, so split relative to
    // the final ')' — the remaining tokens start at field 3. The kernel reports
    // these fields in USER_HZ units, a stable ABI constant of 100 on every
    // supported architecture, independent of CONFIG_HZ.
    const USER_HZ: u64 = 100;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let stime_ticks = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime_ticks.saturating_add(stime_ticks).saturating_mul(1_000) / USER_HZ)
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "proc_pidinfo is the only live-process CPU-time API on macOS that populates user/system time (proc_pid_rusage returns zeros); the buffer is a POD proc_taskinfo the kernel interface writes once"
)]
fn child_cpu_millis(pid: u32) -> Option<u64> {
    // SAFETY: `proc_taskinfo` is plain-old-data; `proc_pidinfo` with
    // PROC_PIDTASKINFO writes exactly one such struct into the buffer and
    // returns its size on success.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let expected = std::mem::size_of::<libc::proc_taskinfo>();
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            (&raw mut info).cast::<libc::c_void>(),
            expected as libc::c_int,
        )
    };
    if usize::try_from(written).ok()? != expected {
        return None;
    }
    // pti_total_user/pti_total_system are mach absolute-time ticks, NOT
    // nanoseconds despite the header's wording; scale by the timebase
    // (125/3 on Apple Silicon) to get nanoseconds.
    let mut timebase = mach2::mach_time::mach_timebase_info_data_t::default();
    // SAFETY: writes one POD timebase struct; kern_return_t 0 is success.
    let rc = unsafe { mach2::mach_time::mach_timebase_info(&raw mut timebase) };
    if rc != 0 || timebase.denom == 0 {
        return None;
    }
    let ticks = info.pti_total_user.saturating_add(info.pti_total_system);
    let nanos = ticks
        .saturating_mul(u64::from(timebase.numer))
        .checked_div(u64::from(timebase.denom))?;
    Some(nanos / 1_000_000)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn child_cpu_millis(pid: u32) -> Option<u64> {
    // Same `ps` fallback shape as `child_rss_kib`. `cputime` prints cumulative
    // CPU as [[DD-]HH:]MM:SS — one-second granularity, which suffices for the
    // sampling deltas callers take (their thresholds are seconds-scale).
    let output = Command::new("ps")
        .args(["-o", "cputime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ps_cputime(output.stdout.as_slice())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parse_ps_cputime(text: impl AsRef<[u8]>) -> Option<u64> {
    let text = String::from_utf8_lossy(text.as_ref());
    // macOS prints [[DD-]HH:]MM:SS with a ".hh" hundredths fraction on the
    // seconds; Linux prints the same without the fraction. Truncate it.
    let text = text.trim().split('.').next()?.trim();
    let (days, text) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let mut parts = text.splitn(3, ':');
    let first = parts.next()?.parse::<u64>().ok()?;
    let second = parts.next()?.parse::<u64>().ok()?;
    let (hours, minutes, seconds) = match parts.next() {
        Some(third) => (first, second, third.parse::<u64>().ok()?),
        None => (0, first, second),
    };
    let total_seconds = days
        .saturating_mul(86_400)
        .saturating_add(hours.saturating_mul(3_600))
        .saturating_add(minutes.saturating_mul(60))
        .saturating_add(seconds);
    Some(total_seconds.saturating_mul(1_000))
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
        DISPLAY_DIAGNOSTICS_MAX_BYTES, GenerationBudget, LeanWorkerConfig,
        LeanWorkerDeclarationVerificationBatchResult, LeanWorkerDeclarationVerificationFacts,
        LeanWorkerDeclarationVerificationStatus, LeanWorkerError, LeanWorkerExit, LeanWorkerImportStats,
        LeanWorkerLifecycleSnapshot, LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome,
        LeanWorkerModuleQuerySelector, LeanWorkerRestartPolicy, LeanWorkerRestartReason, LeanWorkerStats,
        MAX_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP, MIN_FRAME_BYTES, WorkerGeneration, WorkerRequestId,
        child_abort_reason, stale_worker_output_error, stale_worker_request_output_error, truncate_for_display,
    };
    use lean_rs_worker_protocol::types::{
        LeanWorkerDeclarationVerificationBatchItem, LeanWorkerDeclarationVerificationBatchRequest,
        LeanWorkerDeclarationVerificationTarget, LeanWorkerOutputBudgets, LeanWorkerSorryPolicy,
    };
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn checked_worker_rejects_loader_override_before_spawn() {
        let config = LeanWorkerConfig::new("unused-loader-test-executable")
            .env("DYLD_LIBRARY_PATH", "/unused-loader-test-directory")
            .checked_native_loader_environment();
        let result = super::LeanWorker::spawn(&config);
        assert!(matches!(result, Err(LeanWorkerError::Setup { message })
            if message.starts_with("native loader environment override refused:")));
    }

    #[cfg(unix)]
    #[test]
    fn stderr_drain_does_not_block_a_verbose_child_and_keeps_bounded_tail() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "head -c 1048576 /dev/zero >&2; printf finished >&2"])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn verbose child");
        let capture =
            super::StderrCapture::start(child.stderr.take().expect("stderr pipe")).expect("nonblocking stderr");
        let started = std::time::Instant::now();
        loop {
            if child.try_wait().expect("poll child").is_some() {
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                child.kill().expect("kill stalled child");
                child.wait().expect("reap child");
                panic!("stderr blocked the child");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let tail = capture.finish();
        assert_eq!(tail.len(), super::STDERR_TAIL_BYTES);
        assert!(tail.ends_with("finished"));
    }

    #[cfg(unix)]
    #[test]
    fn stderr_capture_does_not_wait_for_inherited_pipe_to_close() {
        let mut child = std::process::Command::new("sleep")
            .arg("1")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn pipe holder");
        let capture =
            super::StderrCapture::start(child.stderr.take().expect("stderr pipe")).expect("nonblocking stderr");
        let started = std::time::Instant::now();
        assert!(capture.finish().is_empty());
        assert!(started.elapsed() < Duration::from_millis(500));
        child.kill().expect("kill pipe holder");
        child.wait().expect("reap pipe holder");
    }

    #[cfg(unix)]
    pub(super) fn blocked_stdin_worker() -> super::LeanWorker {
        piped_worker(std::process::Command::new("sleep").arg("60"))
    }

    #[cfg(unix)]
    fn piped_worker(command: &mut std::process::Command) -> super::LeanWorker {
        let mut worker = sleeping_worker();
        worker
            .shutdown_child(super::ShutdownIntent::KillOnly)
            .expect("stop initial child");
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn piped fixture child");
        worker.stdin = child
            .stdin
            .take()
            .map(|pipe| std::io::BufWriter::new(super::Pipe::new(pipe).expect("nonblocking stdin")));
        worker.stdout = child
            .stdout
            .take()
            .map(|pipe| std::io::BufReader::new(super::Pipe::new(pipe).expect("nonblocking stdout")));
        worker.child = Some(super::LeanWorkerProcessTracker::new().own(child));
        worker.last_exit = None;
        worker.state = super::WorkerSupervisorState::Idle {
            generation: worker.generation,
        };
        worker
    }

    #[cfg(unix)]
    #[test]
    fn scoped_deadline_stops_old_child_without_starting_an_expired_successor() {
        let mut worker = sleeping_worker();
        let old_pid = worker.child.as_ref().expect("old worker").id();
        let scope = super::LeanWorkerCancellationToken::new().with_deadline(super::Instant::now());
        let error = worker
            .restart_with_reason_before_spawn_scoped_using(
                LeanWorkerRestartReason::Cancelled {
                    operation: "scoped_restart",
                },
                || assert!(!child_exists(old_pid), "cleanup precedes successor admission"),
                Some(("scoped_restart", &scope)),
                |_, _| panic!("an expired request started a successor"),
            )
            .expect_err("successor admission expired");
        assert!(matches!(
            error,
            LeanWorkerError::Timeout { operation: "scoped_restart", resource, .. } if !resource.work_entered_child
        ));
        assert!(!child_exists(old_pid));
        assert!(worker.pending_startup.is_none());
        assert_eq!(worker.stats.replacement_successes, 0);
    }

    #[cfg(unix)]
    #[test]
    fn scoped_deadline_caps_request_recycle_startup_and_keeps_configured_budget() {
        // The old child reads the graceful stop frame, then exits without a reply.
        let old_script = "import sys; stream = sys.stdin.buffer; stream.read(int.from_bytes(stream.read(4), 'big'))";
        let mut worker = piped_worker(std::process::Command::new("python3").args(["-c", old_script]));
        let old_pid = worker.child.as_ref().expect("old worker").id();
        let normal_timeout = Duration::from_secs(5);
        worker.config = LeanWorkerConfig::new("/bin/sh")
            .startup_timeout(normal_timeout)
            .shutdown_timeout(Duration::from_millis(100))
            .restart_policy(LeanWorkerRestartPolicy::default().max_requests(1));
        worker.record_request(false);

        let budget = Duration::from_millis(300);
        let started = super::Instant::now();
        let deadline = started.checked_add(budget).expect("short startup deadline");
        let scope = super::LeanWorkerCancellationToken::new().with_deadline(deadline);
        // The policy recycles before dispatch. The replacement shell never sends READY.
        let error = worker
            .worker_json_command("fixture", "{}".to_owned(), Some(&scope), None)
            .expect_err("READY must not receive the normal five-second timeout");
        assert!(
            matches!(
                &error,
                LeanWorkerError::Timeout { operation: "startup", duration, .. }
                    if !duration.is_zero() && *duration <= budget
            ),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "startup spent a fresh budget"
        );
        assert!(!child_exists(old_pid));
        assert_eq!(worker.config.process_tracker.active_processes(), 0);
        assert!(worker.pending_startup.is_none());
        assert_eq!(worker.config.startup_timeout, normal_timeout);
        assert_eq!(worker.stats.replacement_attempts, 1);
        assert_eq!(worker.stats.replacement_successes, 0);
        assert_eq!(worker.stats.requests, 1);
        assert!(worker.last_request.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn scoped_deadline_refuses_expired_frame_and_retires_unanswered_frame() {
        use crate::session::LeanWorkerCancellationToken;
        use std::time::Instant;

        // Answer the first complete request, then hold the second unanswered.
        let response =
            lean_rs_worker_protocol::protocol::Frame::new(super::Message::Response(super::Response::JsonCommand {
                response_json: "{}".to_owned(),
            }));
        let script = r"import sys, time
def request():
    n = int.from_bytes(sys.stdin.buffer.read(4), 'big')
    assert n and len(sys.stdin.buffer.read(n)) == n
request()
reply = sys.argv[1].encode()
sys.stdout.buffer.write(len(reply).to_bytes(4, 'big') + reply)
sys.stdout.buffer.flush()
request()
time.sleep(60)";
        let mut worker = piped_worker(std::process::Command::new("python3").args([
            "-c",
            script,
            &serde_json::to_string(&response).expect("fixture frame"),
        ]));
        let pid = worker.child.as_ref().expect("owned child").id();
        let checkpoint = worker.request_checkpoint();
        let token = LeanWorkerCancellationToken::new();
        let now = Instant::now();
        let later = now.checked_add(Duration::from_secs(5)).expect("short test deadline");
        let expired = token.with_deadline(now).with_deadline(later);
        let request = |worker: &mut super::LeanWorker, scope: &LeanWorkerCancellationToken| {
            worker.worker_json_command("fixture", "{}".to_owned(), Some(scope), None)
        };
        let error = request(&mut worker, &expired).expect_err("expired before dispatch");
        assert!(matches!(error, LeanWorkerError::Timeout { resource, .. } if !resource.work_entered_child));
        assert!(worker.last_request.is_none());
        assert!(worker.child.is_some());
        assert!(!token.is_cancelled());

        // The scope still has time, but the pipe deadline fires before its first write.
        worker.set_request_timeout(Duration::ZERO);
        let error = request(&mut worker, &token.with_deadline(later)).expect_err("zero frame budget");
        assert!(matches!(error, LeanWorkerError::Timeout { resource, .. } if !resource.work_entered_child));
        assert!(worker.pending_request_stop_since(&checkpoint).is_none());
        assert!(worker.last_request.is_none());

        // Leave the worker unpinned: an attempted replacement would fail to spawn.
        worker.set_request_timeout(Duration::from_secs(5));
        let budget = Duration::from_secs(2);
        let started = Instant::now();
        let end = started.checked_add(budget).expect("short test deadline");
        let scope = token.with_deadline(end);
        assert_eq!(request(&mut worker, &scope).expect("first frame answered"), "{}");
        let remaining = end.saturating_duration_since(Instant::now());
        assert!(remaining < budget && !remaining.is_zero());
        let error = request(&mut worker, &scope).expect_err("second frame never answered");
        assert!(matches!(error, LeanWorkerError::Timeout { duration, resource, .. }
            if duration <= remaining && resource.work_entered_child));
        assert_eq!(worker.stats.requests, 2);
        assert_eq!(worker.stats.replacement_attempts, 0);
        assert_eq!(worker.request_timeout(), Duration::from_secs(5));
        let receipt = worker
            .confirmed_request_stop_since(&checkpoint)
            .expect("original child reaped");
        assert_eq!(receipt.process_id(), pid);
        assert_eq!(receipt.operation(), "worker_json_command");
        assert!(!child_exists(pid));
        expired.cancel();
        assert!(scope.is_cancelled());
    }

    #[cfg(unix)]
    #[test]
    fn blocked_stdin_write_times_out_and_reaps_child() {
        let mut worker = blocked_stdin_worker();
        let started = std::time::Instant::now();
        let result = worker.write_request_bounded(
            lean_rs_worker_protocol::protocol::Request::JsonCommand {
                export: "unused".into(),
                request_json: "x".repeat(256 * 1024),
            },
            "test_send",
            None,
            Duration::from_millis(100),
        );
        assert!(matches!(result, Err(LeanWorkerError::Timeout { .. })), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(worker.child.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn blocked_stdin_write_is_cancellable_and_reaps_child() {
        let mut worker = blocked_stdin_worker();
        let token = crate::session::LeanWorkerCancellationToken::new();
        let cancelling = token.clone();
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancelling.cancel();
        });
        let started = std::time::Instant::now();
        let result = worker.write_request_bounded(
            lean_rs_worker_protocol::protocol::Request::JsonCommand {
                export: "unused".into(),
                request_json: "x".repeat(256 * 1024),
            },
            "test_send",
            Some(&token),
            Duration::from_secs(30),
        );
        cancel.join().expect("canceller stopped");
        assert!(matches!(result, Err(LeanWorkerError::Cancelled { .. })), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(worker.child.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn partial_write_stop_receipt_survives_failed_replacement() {
        let mut worker = blocked_stdin_worker();
        worker.config.request_timeout = Duration::from_millis(100);
        let checkpoint = worker.request_checkpoint();
        let pid = worker.child.as_ref().expect("owned child").id();
        let result = worker.send_request_cancellable(
            lean_rs_worker_protocol::protocol::Request::JsonCommand {
                export: "unused".into(),
                request_json: "x".repeat(256 * 1024),
            },
            "partial_write",
            None,
        );
        // The old child was reaped; only replacement fails (dummy executable).
        assert!(matches!(result, Err(LeanWorkerError::Spawn { .. })), "{result:?}");
        let receipt = worker
            .confirmed_request_stop_since(&checkpoint)
            .expect("confirmed partial-write stop");
        assert_eq!(receipt.process_id(), pid);
        assert_eq!(receipt.worker_generation(), 0);
        assert_eq!(receipt.operation(), "partial_write");
        assert!(!child_exists(pid));
    }

    #[cfg(unix)]
    #[test]
    fn exhausted_request_identifiers_reject_before_dispatch() {
        let mut worker = sleeping_worker();
        worker.next_request_id = WorkerRequestId(u64::MAX);
        let checkpoint = worker.request_checkpoint();
        assert!(matches!(
            worker.begin_in_flight("exhausted"),
            Err(LeanWorkerError::Protocol { .. })
        ));
        assert!(worker.confirmed_request_stop_since(&checkpoint).is_none());
        assert!(worker.last_request.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn newer_unreaped_request_cannot_reuse_older_stop_receipt() {
        let mut worker = sleeping_worker();
        let checkpoint = worker.request_checkpoint();
        worker.begin_in_flight("old_request").expect("record old request");
        worker
            .restart_with_reason_before_spawn_using(
                LeanWorkerRestartReason::Cancelled {
                    operation: "old_request",
                },
                || {},
                |_| Ok(sleeping_worker()),
            )
            .expect("replace reaped old child");
        assert!(worker.confirmed_request_stop_since(&checkpoint).is_some());
        worker.begin_in_flight("new_request").expect("record new request");
        assert!(
            worker
                .child
                .as_mut()
                .expect("new child")
                .try_wait()
                .expect("poll new child")
                .is_none()
        );
        assert!(worker.confirmed_request_stop_since(&checkpoint).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn status_reap_produces_only_matching_request_receipt() {
        let mut worker = sleeping_worker();
        let checkpoint = worker.request_checkpoint();
        worker.begin_in_flight("status_reap").expect("record request");
        let pid = worker.child.as_ref().expect("owned child").id();
        worker.child.as_mut().expect("owned child").kill().expect("stop child");
        let started = std::time::Instant::now();
        while matches!(
            worker.status().expect("observe child"),
            super::LeanWorkerStatus::Running
        ) {
            assert!(started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(5));
        }
        let receipt = worker
            .confirmed_request_stop_since(&checkpoint)
            .expect("status observed exit");
        assert_eq!(receipt.process_id(), pid);
        assert_eq!(receipt.operation(), "status_reap");
        assert!(
            worker
                .confirmed_request_stop_since(&worker.request_checkpoint())
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn graceful_shutdown_with_expired_send_returns_reaped_report() {
        let mut worker = blocked_stdin_worker();
        worker.config.shutdown_timeout = Duration::ZERO;
        let report = worker
            .shutdown_child(super::ShutdownIntent::Graceful)
            .expect("shutdown report");
        assert_eq!(
            report.outcome,
            super::LeanWorkerShutdownOutcome::GracefulProtocolFailedKilled
        );
        assert!(worker.child.is_none());
        assert!(worker.last_exit.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn stopped_stdout_reader_joins_while_pipe_holder_remains_alive() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn pipe holder");
        let pipe = super::Pipe::new(child.stdout.take().expect("stdout pipe")).expect("nonblocking pipe");
        let stop = std::sync::Arc::clone(&pipe.stopped);
        let reader = std::thread::spawn(move || {
            let mut pipe = pipe;
            std::io::Read::read(&mut pipe, &mut [0_u8; 1])
        });
        let started = std::time::Instant::now();
        stop.store(true, std::sync::atomic::Ordering::Release);
        assert!(reader.join().expect("reader joined").is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(child.try_wait().expect("poll holder").is_none());
        child.kill().expect("kill holder");
        child.wait().expect("reap holder");
    }

    #[cfg(unix)]
    #[test]
    fn stdout_eof_wait_is_bounded_for_a_child_that_stays_alive() {
        let mut worker = blocked_stdin_worker();
        worker.config.shutdown_timeout = Duration::from_millis(20);
        let started = std::time::Instant::now();
        worker.wait_for_exit().expect("kill and reap after bounded EOF wait");
        assert!(worker.child.is_none());
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn reader_guard_stops_a_full_event_channel_without_receiver_drop() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let event = || super::RequestReaderEvent::ReadError {
            generation: WorkerGeneration::default(),
            request_id: None,
            message: "test".into(),
            eof: false,
            backpressure_waits: 0,
        };
        sender.send(event()).expect("fill event channel");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let _sent = super::send_reader_event(&sender, &thread_stop, event());
        });
        let guard = super::ReaderGuard {
            stopped: stop,
            thread: Some(thread),
        };
        let started = std::time::Instant::now();
        drop(guard);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(unix)]
    #[test]
    fn reader_guard_stops_a_partial_frame_without_pipe_eof() {
        let mut child = std::process::Command::new("python3")
            .args(["-c", "import os,time; os.write(1,b'x'); time.sleep(60)"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn partial frame holder");
        let pipe = super::Pipe::new(child.stdout.take().expect("stdout pipe")).expect("nonblocking pipe");
        let stop = std::sync::Arc::clone(&pipe.stopped);
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            super::read_request_messages(
                std::io::BufReader::new(pipe),
                sender,
                MAX_FRAME_BYTES,
                WorkerGeneration::default(),
                None,
            );
        });
        let guard = super::ReaderGuard {
            stopped: stop,
            thread: Some(thread),
        };
        std::thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        drop(guard);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(child.try_wait().expect("poll holder").is_none());
        child.kill().expect("kill holder");
        child.wait().expect("reap holder");
    }

    #[cfg(unix)]
    #[test]
    fn consuming_shutdown_error_still_reaps_owned_child() {
        let worker = sleeping_worker();
        let observation = worker.child.as_ref().expect("owned child").exit_observation();
        assert!(observation.confirmed_exit().is_none());
        worker.shutdown().expect_err("fixture has no protocol stdin");
        let start = std::time::Instant::now();
        while observation.confirmed_exit().is_none() {
            assert!(start.elapsed() < Duration::from_secs(3));
            std::thread::yield_now();
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_cleanup_keeps_process_count_until_confirmed_exit() {
        let tracker = super::LeanWorkerProcessTracker::new();
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn retained child");
        let mut worker = sleeping_worker();
        worker
            .shutdown_child(super::ShutdownIntent::KillOnly)
            .expect("stop initial child");
        worker.pending_startup = Some(Box::new(super::StartupChild(Some(tracker.own(child)))));
        let failure = worker.shutdown_child_with_cleanup(super::ShutdownIntent::KillOnly, |_| {
            Err(LeanWorkerError::WaitTimeout {
                operation: "injected_wait_failure",
                duration: Duration::ZERO,
            })
        });
        assert!(failure.is_err());
        assert_eq!(tracker.active_processes(), 1);
        assert!(worker.pending_startup.is_some());
        worker.try_shutdown().expect("retry cleanup confirms exit");
        assert_eq!(tracker.active_processes(), 0);
        drop(worker);
        assert_eq!(tracker.active_processes(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn pending_stop_is_exact_and_resolves_only_after_owned_reap() {
        let mut worker = sleeping_worker();
        let checkpoint = worker.request_checkpoint();
        worker.begin_in_flight("late_owned_reap").expect("bind exact request");
        let pending = worker.pending_request_stop_since(&checkpoint).expect("bound observer");
        assert!(pending.confirmed_stop().is_none());
        assert!(
            worker
                .pending_request_stop_since(&worker.request_checkpoint())
                .is_none()
        );
        // This owned sleep fixture has no protocol stdin/stdout. Exercise
        // actual kill/reap rather than attempting a graceful wire request.
        worker
            .shutdown_child(super::ShutdownIntent::KillOnly)
            .expect("owned cleanup confirms exit");
        let receipt = pending.confirmed_stop().expect("observer resolved by wait");
        assert_eq!(receipt.operation(), "late_owned_reap");
        assert_eq!(receipt.worker_generation(), 0);
        assert_eq!(receipt.request_id(), 0);
    }

    fn dummy_config() -> LeanWorkerConfig {
        LeanWorkerConfig::new(PathBuf::from("/nonexistent/lean-rs-worker-child"))
    }

    #[cfg(unix)]
    pub(super) fn sleeping_worker() -> super::LeanWorker {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("owned old test child");
        super::LeanWorker {
            config: dummy_config(),
            child: Some(super::LeanWorkerProcessTracker::new().own(child)),
            stdin: None,
            request_started: None,
            stdout: None,
            stderr: None,
            last_exit: None,
            request_scope: std::sync::Arc::new(()),
            native_generation_id: None,
            generation_dispatch: None,
            last_request: None,
            last_request_stop: None,
            last_request_exit: None,
            pending_startup: None,
            runtime_metadata: crate::session::LeanWorkerRuntimeMetadata {
                worker_version: "test".to_owned(),
                protocol_version: 1,
                lean_version: None,
            },
            host_runtime_metadata: None,
            host_read_active: false,
            last_host_session_refusal: None,
            stats: LeanWorkerStats::default(),
            budget: GenerationBudget::default(),
            open_session_keys: Vec::new(),
            reuse_hint_enabled: true,
            reuse_observed: false,
            last_activity: std::time::Instant::now(),
            generation: WorkerGeneration::default(),
            next_request_id: WorkerRequestId::default(),
            restart_window: std::collections::VecDeque::new(),
            state: super::WorkerSupervisorState::Idle {
                generation: WorkerGeneration::default(),
            },
        }
    }

    #[cfg(unix)]
    fn child_exists(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("check test child existence")
            .success()
    }

    #[cfg(unix)]
    #[test]
    fn failed_replacement_retains_unconfirmed_child_instead_of_reusing_old_exit() {
        let mut worker = sleeping_worker();
        let checkpoint = worker.request_checkpoint();
        worker.begin_in_flight("test_replacement").expect("record request");
        let old_pid = worker.child.as_ref().expect("old child").id();
        let replacement_pid = std::cell::Cell::new(0);
        let error = worker
            .restart_with_reason_before_spawn_using(
                LeanWorkerRestartReason::Cancelled {
                    operation: "test_replacement",
                },
                || {},
                |_config| {
                    assert!(!child_exists(old_pid), "replacement must follow confirmed old exit");
                    let replacement = std::process::Command::new("sleep")
                        .arg("60")
                        .spawn()
                        .expect("replacement test child");
                    replacement_pid.set(replacement.id());
                    Err(super::SpawnFailure::after_spawn(
                        super::StartupChild(Some(super::LeanWorkerProcessTracker::new().own(replacement))),
                        LeanWorkerError::Protocol {
                            message: "injected post-spawn setup failure".to_owned(),
                        },
                        |_child| {
                            Err(LeanWorkerError::Wait {
                                source: std::io::Error::other("injected cleanup uncertainty"),
                            })
                        },
                    ))
                },
            )
            .expect_err("replacement setup must fail");
        assert!(matches!(error, LeanWorkerError::Setup { .. }));
        assert!(worker.last_exit.is_some(), "old exit remains historical evidence");
        let receipt = worker
            .confirmed_request_stop_since(&checkpoint)
            .expect("original child exit")
            .clone();
        assert_eq!(receipt.process_id(), old_pid);
        assert!(
            worker.pending_startup.is_some(),
            "failed replacement ownership must survive the error"
        );
        assert!(child_exists(replacement_pid.get()));
        assert_eq!(worker.confirmed_request_stop_since(&checkpoint), Some(&receipt));
        assert!(worker.status().is_err(), "the old exit cannot prove replacement exit");
        assert!(
            worker.health().is_err(),
            "no request may use an unconfirmed replacement"
        );
        let failed_shutdown = worker.shutdown_child_with_cleanup(super::ShutdownIntent::Graceful, |_child| {
            Err(LeanWorkerError::Wait {
                source: std::io::Error::other("injected persistent cleanup uncertainty"),
            })
        });
        assert!(matches!(failed_shutdown, Err(LeanWorkerError::Wait { .. })));
        assert!(worker.pending_startup.is_some());
        assert!(worker.status().is_err());
        assert!(child_exists(replacement_pid.get()));
        assert_eq!(worker.confirmed_request_stop_since(&checkpoint), Some(&receipt));
        let confirmed = worker
            .shutdown_child(super::ShutdownIntent::Graceful)
            .expect("retry cleanup and reap replacement");
        assert_eq!(confirmed.outcome, super::LeanWorkerShutdownOutcome::KillOnly);
        assert!(!child_exists(replacement_pid.get()));
        assert!(worker.pending_startup.is_none());
        assert_eq!(
            worker.confirmed_request_stop_since(&checkpoint),
            Some(&receipt),
            "startup cleanup must not replace the original request's process identity"
        );
        assert_eq!(worker.stats.exits, 2);
        let repeated = worker
            .shutdown_child(super::ShutdownIntent::Graceful)
            .expect("already confirmed exit");
        assert_eq!(repeated.outcome, super::LeanWorkerShutdownOutcome::AlreadyExited);
    }

    #[cfg(unix)]
    #[test]
    fn failed_replacement_releases_only_after_confirmed_cleanup_or_no_spawn() {
        for spawned in [false, true] {
            let mut worker = sleeping_worker();
            let replacement_pid = std::cell::Cell::new(0);
            let result = worker.restart_with_reason_before_spawn_using(
                LeanWorkerRestartReason::Cancelled {
                    operation: "test_confirmed_failure",
                },
                || {},
                |_config| {
                    if !spawned {
                        return Err(super::SpawnFailure::no_child(LeanWorkerError::Spawn {
                            executable: PathBuf::from("missing"),
                            source: std::io::Error::from(std::io::ErrorKind::NotFound),
                        }));
                    }
                    let replacement = std::process::Command::new("sleep")
                        .arg("60")
                        .spawn()
                        .expect("replacement test child");
                    replacement_pid.set(replacement.id());
                    Err(super::SpawnFailure::after_spawn(
                        super::StartupChild(Some(super::LeanWorkerProcessTracker::new().own(replacement))),
                        LeanWorkerError::Protocol {
                            message: "injected setup failure".to_owned(),
                        },
                        super::StartupChild::cleanup,
                    ))
                },
            );
            assert!(result.is_err());
            assert!(worker.pending_startup.is_none());
            if spawned {
                assert!(!child_exists(replacement_pid.get()));
            }
            assert!(matches!(worker.status(), Ok(super::LeanWorkerStatus::Exited(_))));
            assert_eq!(worker.stats.exits, if spawned { 2 } else { 1 });
            assert_eq!(
                worker
                    .shutdown_child(super::ShutdownIntent::Graceful)
                    .expect("confirmed shutdown")
                    .outcome,
                super::LeanWorkerShutdownOutcome::AlreadyExited
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn tracked_spawn_timeout_confirms_exit_and_spawn_failure_has_no_child() {
        let no_spawn = super::LeanWorker::spawn_tracked(&dummy_config()).expect_err("missing executable");
        assert!(matches!(no_spawn.error, LeanWorkerError::Spawn { .. }));
        assert!(no_spawn.pending.is_none());
        for startup_timeout in [Duration::ZERO, Duration::from_millis(10)] {
            let timeout =
                super::LeanWorker::spawn_tracked(&LeanWorkerConfig::new("/bin/sh").startup_timeout(startup_timeout))
                    .expect_err("shell does not speak the worker protocol");
            assert!(matches!(
                timeout.error,
                LeanWorkerError::Timeout {
                    operation: "startup",
                    ..
                }
            ));
            assert!(
                timeout.pending.is_none(),
                "confirmed startup timeout must not quarantine a nonexistent child"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn startup_failure_guard_reaps_owned_child_and_preserves_other_child() {
        use std::process::{Command, Stdio};
        let child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::piped())
            .spawn()
            .expect("owned test child");
        let pid = child.id();
        let other = Command::new("sleep").arg("60").spawn().expect("unrelated test child");
        let mut other = super::StartupChild(Some(super::LeanWorkerProcessTracker::new().own(other)));
        let failed_setup = (|| -> Result<(), LeanWorkerError> {
            let mut owned = super::StartupChild(Some(super::LeanWorkerProcessTracker::new().own(child)));
            let _stdin = owned.child_mut()?.stdin.take().ok_or_else(|| LeanWorkerError::Setup {
                message: "test child stdin unavailable".to_owned(),
            })?;
            // Inject the error after pipe setup, before ownership transfers to
            // LeanWorker, as a failed ConfigureFrameLimit write would do.
            Err(LeanWorkerError::Protocol {
                message: "injected frame-limit write failure".to_owned(),
            })
        })();
        assert!(matches!(failed_setup, Err(LeanWorkerError::Protocol { .. })));
        let exists = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .expect("check owned child existence");
        assert!(
            !exists.success(),
            "failed startup left its owned child alive or unreaped"
        );
        assert!(
            other
                .child_mut()
                .expect("unrelated child still owned")
                .try_wait()
                .expect("check unrelated child")
                .is_none()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn child_cpu_millis_delta_moves_on_a_busy_child() {
        use super::child_cpu_millis;
        let mut child = std::process::Command::new("yes")
            .arg("busy")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn yes: {e}"));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let first = child_cpu_millis(child.id());
        std::thread::sleep(std::time::Duration::from_millis(300));
        let second = child_cpu_millis(child.id());
        child.kill().ok();
        child.wait().ok();
        let (Some(first), Some(second)) = (first, second) else {
            panic!("samples must be Some, got {first:?} {second:?}");
        };
        assert!(second > first, "busy child delta must move: {first} -> {second}");
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn child_cpu_millis_samples_a_live_process() {
        use super::child_cpu_millis;
        let sample = child_cpu_millis(std::process::id());
        assert!(sample.is_some(), "proc_pid_rusage must sample the current process");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn ps_cputime_parses_every_shape() {
        use super::parse_ps_cputime;
        assert_eq!(parse_ps_cputime("  0:07.42  "), Some(7_000));
        assert_eq!(parse_ps_cputime("12:34"), Some(754_000));
        assert_eq!(parse_ps_cputime("1:02:03.45"), Some(3_723_000));
        assert_eq!(parse_ps_cputime("2-03:04:05"), Some(183_845_000));
        assert_eq!(parse_ps_cputime(""), None);
        assert_eq!(parse_ps_cputime("abc"), None);
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

    fn import_stats_with_residue(non_memory_mapped_region_bytes: u64) -> LeanWorkerImportStats {
        LeanWorkerImportStats {
            direct_import_names: vec!["Probe".to_owned()],
            effective_module_count: 12,
            compacted_region_count: 12,
            memory_mapped_region_count: 0,
            compacted_region_bytes: non_memory_mapped_region_bytes.max(1),
            memory_mapped_region_bytes: 0,
            non_memory_mapped_region_bytes,
            imported_bytes: non_memory_mapped_region_bytes,
            imported_constant_count: 100,
            extension_count: 3,
            total_imported_extension_entries: 30,
            import_level: "server".to_owned(),
            import_all: false,
            load_exts: true,
        }
    }

    #[test]
    fn header_import_accounting_is_cumulative_and_unknown_is_sticky() {
        let mut budget = GenerationBudget::default();
        budget.record_import(&import_stats_with_residue(100));
        budget.record_header_imports(200, 1, false);
        budget.record_header_imports(200, 1, false);
        assert_eq!(budget.import_residue_bytes, 300);
        assert_eq!(budget.imports, 1, "max_imports counts session imports only");
        assert_eq!(budget.header_imports, 1);
        budget.record_header_imports(500, 2, false);
        assert_eq!(budget.import_residue_bytes, 600);
        assert_eq!(budget.imports, 1);
        assert_eq!(budget.header_imports, 2);
        budget.record_header_imports(0, 0, true);
        budget.record_header_imports(500, 2, false);
        assert_eq!(budget.import_residue_bytes, 600);
        assert!(budget.accounting_unknown);
        assert!(!GenerationBudget::default().accounting_unknown);
    }

    #[test]
    fn residue_accumulates_only_over_confirmed_imports() {
        let mut budget = GenerationBudget::default();
        budget.record_request(false);
        budget.record_request(false);
        assert_eq!(
            budget.import_residue_bytes, 0,
            "a request that did not import charges nothing"
        );

        budget.record_import(&import_stats_with_residue(1_000));
        budget.record_import(&import_stats_with_residue(2_500));
        assert_eq!(budget.import_residue_bytes, 3_500);
        assert_eq!(budget.imports, 2);
    }

    #[test]
    fn a_single_import_over_the_whole_budget_still_runs() {
        let policy = LeanWorkerRestartPolicy::default().max_import_residue_bytes(1_000);
        let mut budget = GenerationBudget::default();
        // The check runs before the request, so an empty generation admits.
        assert_eq!(budget.import_limit_reached(&policy), None);
        budget.record_import(&import_stats_with_residue(9_000_000));
        // And only then does it owe a cycle. A profile whose closure exceeds the
        // entire budget must still be servable.
        assert!(matches!(
            budget.import_limit_reached(&policy),
            Some(LeanWorkerRestartReason::ImportResidue { .. })
        ));
    }

    #[test]
    fn the_residue_bound_is_reported_ahead_of_the_import_count_backstop() {
        let policy = LeanWorkerRestartPolicy::default()
            .max_import_residue_bytes(1_000)
            .max_imports(1);
        let mut budget = GenerationBudget::default();
        budget.record_import(&import_stats_with_residue(1_000));
        // Both bounds are crossed by this one import. The byte bound is the one
        // an operator would act on, so it is the one named.
        assert_eq!(
            budget.import_limit_reached(&policy),
            Some(LeanWorkerRestartReason::ImportResidue {
                residue_bytes: 1_000,
                limit_bytes: 1_000,
            })
        );
    }

    #[test]
    fn the_import_count_backstop_still_fires_when_residue_is_unreported() {
        // A child that reports no region bytes — the failure this backstop
        // exists for. The byte bound can never fire, so the count must.
        let policy = LeanWorkerRestartPolicy::default()
            .max_import_residue_bytes(1_000)
            .max_imports(3);
        let mut budget = GenerationBudget::default();
        for _ in 0..3 {
            budget.record_import(&import_stats_with_residue(0));
        }
        assert_eq!(budget.import_residue_bytes, 0);
        assert_eq!(
            budget.import_limit_reached(&policy),
            Some(LeanWorkerRestartReason::MaxImports { limit: 3 })
        );
    }

    #[test]
    fn an_unset_residue_bound_never_cycles_however_much_is_imported() {
        let policy = LeanWorkerRestartPolicy::default();
        let mut budget = GenerationBudget::default();
        for _ in 0..64 {
            budget.record_import(&import_stats_with_residue(4 * 1024 * 1024 * 1024));
        }
        assert_eq!(budget.import_limit_reached(&policy), None);
        assert_eq!(budget.request_limit_reached(&policy), None);
    }

    #[test]
    fn residue_accounting_matches_the_sum_of_confirmed_imports() {
        // The closed form: residue is the sum of `non_memory_mapped_region_bytes`
        // over exactly the opens the child confirmed, and reuses contribute
        // nothing however often they report the same stats.
        let calls: Vec<(u64, bool)> = vec![
            (2_000, false),
            (0, true),
            (3_500, false),
            (3_500, true),
            (3_500, true),
            (1, false),
            (0, false),
        ];
        let mut budget = GenerationBudget::default();
        for (residue, was_reuse) in &calls {
            budget.record_request(false);
            if !was_reuse {
                budget.record_import(&import_stats_with_residue(*residue));
            }
        }
        let expected: u64 = calls
            .iter()
            .filter(|(_, was_reuse)| !was_reuse)
            .map(|(residue, _)| residue)
            .sum();
        assert_eq!(budget.import_residue_bytes, expected);
        assert_eq!(budget.imports, 4);
        assert_eq!(budget.requests, u64::try_from(calls.len()).expect("small count"));
    }

    #[test]
    fn a_default_config_leaves_pool_capacity_to_the_child() {
        let config = LeanWorkerConfig::new("worker");
        assert_eq!(config.resolved_session_pool_capacity(), None);
    }

    #[test]
    fn pool_capacity_falls_back_to_max_imports_but_an_explicit_value_wins() {
        let policy = LeanWorkerRestartPolicy::default().max_imports(3);
        let derived = LeanWorkerConfig::new("worker").restart_policy(policy.clone());
        assert_eq!(derived.resolved_session_pool_capacity(), Some(3));

        // The decoupling: capacity is sized by what a held environment costs,
        // the restart bound by what an import retains. Setting one must not move
        // the other.
        let explicit = LeanWorkerConfig::new("worker")
            .restart_policy(policy)
            .session_pool_capacity(8);
        assert_eq!(explicit.resolved_session_pool_capacity(), Some(8));
        assert_eq!(explicit.restart_policy.max_imports, Some(3));
    }

    #[test]
    fn pool_capacity_clamps_to_a_servable_minimum() {
        let config = LeanWorkerConfig::new("worker").session_pool_capacity(0);
        assert_eq!(config.resolved_session_pool_capacity(), Some(1));
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
    fn conformance_stale_request_output_is_protocol_failure() {
        let err = stale_worker_request_output_error("emit_test_rows", Some(WorkerRequestId(2)), WorkerRequestId(1));
        match err {
            LeanWorkerError::Protocol { message } => {
                assert!(message.contains("stale emit_test_rows frame"));
                assert!(message.contains("request 1"));
                assert!(message.contains("current request is 2"));
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
        let snapshot = LeanWorkerLifecycleSnapshot::from_worker(&stats, Some(exit.clone()), 0, None);
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
            LeanWorkerRestartReason::ChildAbort {
                operation: "test",
                status: String::from("signal: 6 (SIGABRT)"),
                diagnostics: String::new(),
            }
            .stable_cause(),
            "child_abort"
        );
    }

    /// A child abort is unattributable unless the exit carries forward: the
    /// stderr pipe is drained once, at reap, and kept nowhere else. The case
    /// that motivated this is a Lean heap-ceiling overrun, which reaches the
    /// parent as a bare SIGABRT and is indistinguishable from a segfault
    /// without the child's own message.
    #[test]
    fn a_child_abort_carries_the_exit_status_and_the_child_message() {
        let exit = LeanWorkerExit {
            success: false,
            code: Some(134),
            status: String::from("exit status: 134"),
            diagnostics: String::from(
                "\n  fatal runtime error: Rust cannot catch foreign exceptions, aborting\n\
                 lean-rs-worker child: SIGABRT, exiting immediately\n",
            ),
        };
        let LeanWorkerRestartReason::ChildAbort {
            operation,
            status,
            diagnostics,
        } = child_abort_reason("verify_declaration", &exit)
        else {
            panic!("child_abort_reason must produce ChildAbort");
        };
        assert_eq!(operation, "verify_declaration");
        assert_eq!(status, "exit status: 134");
        // Trimmed, so the reason renders on one line, but the cause survives.
        assert!(diagnostics.starts_with("fatal runtime error"), "{diagnostics}");
        assert!(diagnostics.contains("SIGABRT"), "{diagnostics}");
    }

    /// The reason is cloned onto every lifecycle snapshot taken after it, so an
    /// unbounded stderr capture would be retained per snapshot.
    #[test]
    fn a_child_abort_truncates_a_runaway_stderr_capture() {
        let exit = LeanWorkerExit {
            success: false,
            code: None,
            status: String::from("signal: 6 (SIGABRT)"),
            diagnostics: "x".repeat(DISPLAY_DIAGNOSTICS_MAX_BYTES * 4),
        };
        let LeanWorkerRestartReason::ChildAbort { diagnostics, .. } = child_abort_reason("health", &exit) else {
            panic!("child_abort_reason must produce ChildAbort");
        };
        assert!(diagnostics.contains("bytes truncated"), "{diagnostics}");
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
    fn declaration_outline_degraded_query_batch_outcome_uses_selector_id() {
        let selectors = vec![LeanWorkerModuleQuerySelector::DeclarationOutline {
            id: "outline".to_owned(),
        }];
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
        let LeanWorkerModuleQueryBatchOutcome::Ok { result, facts, .. } =
            super::degraded_query_batch_outcome(&selectors, resource.clone())
        else {
            panic!("degraded outcome should be Ok with per-selector items");
        };
        assert_eq!(facts.resource.as_deref(), Some(&resource));
        assert!(matches!(
            result.items.as_slice(),
            [LeanWorkerModuleQueryBatchItem::BudgetExceeded { id, .. }] if id == "outline"
        ));
    }

    #[test]
    fn command_message_degraded_query_batch_outcome_uses_diagnostics_selector_id() {
        let selectors = vec![LeanWorkerModuleQuerySelector::Diagnostics {
            id: "messages".to_owned(),
        }];
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
        let LeanWorkerModuleQueryBatchOutcome::Ok { result, facts, .. } =
            super::degraded_query_batch_outcome(&selectors, resource.clone())
        else {
            panic!("degraded outcome should be Ok with per-selector items");
        };
        assert_eq!(facts.resource.as_deref(), Some(&resource));
        assert!(matches!(
            result.items.as_slice(),
            [LeanWorkerModuleQueryBatchItem::BudgetExceeded { id, .. }] if id == "messages"
        ));
    }

    #[test]
    fn declaration_verification_batch_degraded_result_preserves_target_order() {
        let request = LeanWorkerDeclarationVerificationBatchRequest {
            source: "theorem a : True := by trivial\n".to_owned(),
            targets: vec![
                LeanWorkerDeclarationVerificationBatchItem {
                    id: "first".to_owned(),
                    target: LeanWorkerDeclarationVerificationTarget::Name { name: "a".to_owned() },
                },
                LeanWorkerDeclarationVerificationBatchItem {
                    id: "second".to_owned(),
                    target: LeanWorkerDeclarationVerificationTarget::Name { name: "b".to_owned() },
                },
            ],
            sorry_policy: LeanWorkerSorryPolicy::Deny,
            report_axioms: true,
            budgets: LeanWorkerOutputBudgets::default(),
        };
        let LeanWorkerDeclarationVerificationBatchResult::Ok { results, imports } =
            super::degraded_declaration_verification_batch_result(&request)
        else {
            panic!("degraded batch verification should be an Ok batch");
        };
        assert!(imports.is_empty());
        let ids: Vec<&str> = results.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["first", "second"]);
        assert!(results.iter().all(|row| {
            row.verification_status == LeanWorkerDeclarationVerificationStatus::BudgetExceeded
                && !row.facts.axioms_available
                && row.facts.axioms.is_empty()
        }));
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

#[cfg(all(test, unix))]
#[path = "supervisor/dispatch_pin_tests.rs"]
mod dispatch_pin_tests;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "supervisor/managed_once.rs"]
mod managed_once;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use managed_once::{ManagedFinish, SaveManagedInspection};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use managed_once::{ManagedInspectionOutcome, ManagedWarmInspectionOutcome};
