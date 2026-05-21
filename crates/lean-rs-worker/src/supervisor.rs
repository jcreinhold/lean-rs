use std::ffi::OsString;
use std::fmt;
use std::io::{BufReader, BufWriter, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::{Message, Request, Response, read_frame, write_frame};
use crate::session::LeanWorkerDataSinkTarget;
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerCapabilityMetadata, LeanWorkerDataSink, LeanWorkerDiagnosticSink,
    LeanWorkerDoctorReport, LeanWorkerElabOptions, LeanWorkerElabResult, LeanWorkerKernelResult,
    LeanWorkerProgressSink, LeanWorkerRawDataSink, LeanWorkerRuntimeMetadata, LeanWorkerSessionConfig,
    LeanWorkerStreamSummary, check_cancelled, elapsed_event, report_parent_data_row, report_parent_diagnostic,
    report_parent_progress,
};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Default deadline for one worker request after startup.
pub const LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/// Suggested deadline for long-running worker requests.
pub const LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING: Duration = Duration::from_mins(10);

/// Configuration for starting a `lean-rs-worker` child process.
///
/// The executable should be the `lean-rs-worker-child` binary. The supervisor
/// sets `LEAN_ABORT_ON_PANIC=1` by default so Lean internal panics become fatal
/// child exits instead of attempting in-process recovery; explicit environment
/// entries supplied here override that default.
#[derive(Clone, Debug)]
pub struct LeanWorkerConfig {
    executable: PathBuf,
    current_dir: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    startup_timeout: Duration,
    request_timeout: Duration,
    restart_policy: LeanWorkerRestartPolicy,
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
            restart_policy: LeanWorkerRestartPolicy::default(),
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
}

impl LeanWorkerRestartPolicy {
    /// Disable automatic policy restarts.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
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
    RssCeiling { current_kib: u64, limit_kib: u64 },
    /// Worker was idle at least as long as the configured limit.
    Idle { idle_for: Duration, limit: Duration },
    /// Parent-side cancellation replaced the child during an in-flight request.
    Cancelled { operation: &'static str },
    /// Parent-side request timeout replaced the child during an in-flight request.
    RequestTimeout {
        operation: &'static str,
        duration: Duration,
    },
}

/// Snapshot of worker lifecycle counters.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanWorkerStats {
    /// Requests that entered a worker child.
    pub requests: u64,
    /// Import-like requests that entered a worker child.
    pub imports: u64,
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
            LeanWorkerRestartReason::Idle { .. } => {
                self.idle_restarts = self.idle_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::Cancelled { .. } => {
                self.cancelled_restarts = self.cancelled_restarts.saturating_add(1);
            }
            LeanWorkerRestartReason::RequestTimeout { .. } => {
                self.timeout_restarts = self.timeout_restarts.saturating_add(1);
            }
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
    },
    /// A parent-side cancellation token was observed.
    Cancelled { operation: &'static str },
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
    WorkerPoolExhausted { max_workers: usize },
    /// The public supervisor does not support the requested operation.
    UnsupportedRequest { operation: &'static str },
    /// Waiting for a child process failed.
    Wait { source: std::io::Error },
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
            Self::CapabilityBuild { diagnostic } => {
                write!(f, "worker capability Lake target build failed: {diagnostic}")
            }
            Self::Setup { message } => write!(f, "worker child setup failed: {message}"),
            Self::Handshake { message } => write!(f, "worker handshake failed: {message}"),
            Self::Protocol { message } => write!(f, "worker protocol failed: {message}"),
            Self::Worker { code, message } => write!(f, "worker returned {code}: {message}"),
            Self::ChildExited { exit } => write!(f, "worker exited with {}", exit.status),
            Self::ChildPanicOrAbort { exit } => {
                write!(f, "worker exited fatally with {}", exit.status)
            }
            Self::Timeout { operation, duration } => {
                write!(f, "worker operation {operation} timed out after {duration:?}")
            }
            Self::Cancelled { operation } => write!(f, "worker operation {operation} was cancelled"),
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
            Self::WorkerPoolExhausted { max_workers } => {
                write!(
                    f,
                    "worker pool cannot admit another session key; max_workers={max_workers}"
                )
            }
            Self::UnsupportedRequest { operation } => {
                write!(f, "worker operation {operation} is not supported")
            }
            Self::Wait { source } => write!(f, "failed to wait for worker child: {source}"),
        }
    }
}

impl std::error::Error for LeanWorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Wait { source } => Some(source),
            Self::CapabilityBuild { diagnostic } => Some(diagnostic),
            Self::WorkerChildUnresolved { .. } => None,
            Self::Setup { .. }
            | Self::Handshake { .. }
            | Self::Protocol { .. }
            | Self::Worker { .. }
            | Self::ChildExited { .. }
            | Self::ChildPanicOrAbort { .. }
            | Self::Timeout { .. }
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
            | Self::UnsupportedRequest { .. } => None,
        }
    }
}

/// Supervisor for one `lean-rs-worker` child process.
///
/// Dropping a live supervisor attempts to terminate the child and then waits
/// for it. Drop never panics; explicit `terminate` is preferred when callers
/// need the exit status.
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
}

impl LeanWorker {
    /// Spawn a worker child and wait for its protocol handshake.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the child cannot be spawned, child setup
    /// fails, the child exits before handshaking, or the startup timeout
    /// expires.
    pub fn spawn(config: &LeanWorkerConfig) -> Result<Self, LeanWorkerError> {
        let mut command = Command::new(&config.executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("LEAN_ABORT_ON_PANIC", "1")
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

        let stdin = child
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

        let (sender, receiver) = mpsc::channel();
        let _handshake_reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let result = expect_handshake(&mut stdout);
            drop(sender.send((stdout, result)));
        });

        let (stdout, runtime_metadata) = match receiver.recv_timeout(config.startup_timeout) {
            Ok((stdout, Ok(metadata))) => (stdout, metadata),
            Ok((_stdout, Err(err))) => {
                let mut worker = Self {
                    config: config.clone(),
                    child: Some(child),
                    stdin: Some(stdin),
                    stdout: None,
                    stderr,
                    last_exit: None,
                    runtime_metadata: LeanWorkerRuntimeMetadata {
                        worker_version: String::new(),
                        protocol_version: crate::protocol::PROTOCOL_VERSION,
                        lean_version: None,
                    },
                    stats: LeanWorkerStats::default(),
                    requests_since_restart: 0,
                    imports_since_restart: 0,
                    last_activity: Instant::now(),
                };
                let exit = worker.try_record_exit();
                return Err(match exit {
                    Some(exit) if !exit.success => LeanWorkerError::ChildPanicOrAbort { exit },
                    Some(exit) => LeanWorkerError::ChildExited { exit },
                    None => err,
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                drop(child.kill());
                let _exit = wait_with_stderr(&mut child, stderr)?;
                return Err(LeanWorkerError::Timeout {
                    operation: "startup",
                    duration: config.startup_timeout,
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(LeanWorkerError::Handshake {
                    message: "handshake reader exited without a result".to_owned(),
                });
            }
        };

        Ok(Self {
            config: config.clone(),
            child: Some(child),
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr,
            last_exit: None,
            runtime_metadata,
            stats: LeanWorkerStats::default(),
            requests_since_restart: 0,
            imports_since_restart: 0,
            last_activity: Instant::now(),
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
            other @ (Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response("health", &other)),
        }
    }

    /// Load the prompt fixture capability in the worker child.
    ///
    /// This prompt-57 method proves the supervisor path. Prompt 59 adds the
    /// supported host-session adapter instead of expanding this fixture surface.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, fixture loading fails,
    /// or protocol communication fails.
    pub fn load_fixture_capability(&mut self, fixture_root: impl AsRef<Path>) -> Result<(), LeanWorkerError> {
        self.prepare_request(true)?;
        self.send_request(Request::LoadFixtureCapability {
            fixture_root: path_string(fixture_root.as_ref()),
        })?;
        self.record_request(true);
        match self.read_response("load_fixture_capability")? {
            Response::CapabilityLoaded => Ok(()),
            other @ (Response::HealthOk
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response("load_fixture_capability", &other)),
        }
    }

    /// Call the prompt fixture multiplication export in the worker child.
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
        self.prepare_request(true)?;
        self.send_request(Request::CallFixtureMul {
            fixture_root: path_string(fixture_root.as_ref()),
            lhs,
            rhs,
        })?;
        self.record_request(true);
        match self.read_response("call_fixture_mul")? {
            Response::U64 { value } => Ok(value),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response("call_fixture_mul", &other)),
        }
    }

    /// Return the current worker lifecycle status.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if checking the process status fails.
    pub fn status(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        if let Some(exit) = &self.last_exit {
            return Ok(LeanWorkerStatus::Exited(exit.clone()));
        }
        let Some(child) = self.child.as_mut() else {
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
        child.kill().map_err(|source| LeanWorkerError::Wait { source })?;
        Ok(())
    }

    /// Ask the child to terminate cleanly and wait for it.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is already dead, the protocol
    /// fails, or waiting for the child process fails.
    pub fn terminate(mut self) -> Result<LeanWorkerExit, LeanWorkerError> {
        self.send_request(Request::Terminate)?;
        match self.read_response("terminate")? {
            Response::Terminating => self.wait_for_exit(),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Error { .. }) => Err(unexpected_response("terminate", &other)),
        }
    }

    #[doc(hidden)]
    /// Trigger the prompt fixture panic path.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker does not exit fatally or if the
    /// protocol fails before the panic path runs.
    pub fn __trigger_lean_panic_fixture(
        mut self,
        fixture_root: impl AsRef<Path>,
    ) -> Result<LeanWorkerExit, LeanWorkerError> {
        self.prepare_request(true)?;
        self.send_request(Request::TriggerLeanPanic {
            fixture_root: path_string(fixture_root.as_ref()),
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
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
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
        self.prepare_request(true)?;
        self.send_request(Request::OpenHostSession {
            project_root: config.project_root_string(),
            package: config.package().to_owned(),
            lib_name: config.lib_name().to_owned(),
            imports: config.imports().to_vec(),
        })?;
        self.record_request(true);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::HostSessionOpened => Ok(()),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_elaborate(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerElabResult, LeanWorkerError> {
        const OPERATION: &str = "worker_elaborate";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false)?;
        self.send_request(Request::Elaborate {
            source: source.to_owned(),
            options: options.wire(),
        })?;
        self.record_request(false);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::Elaboration { outcome } => Ok(outcome.into()),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_kernel_check(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerKernelResult, LeanWorkerError> {
        const OPERATION: &str = "worker_kernel_check";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false)?;
        self.send_request(Request::KernelCheck {
            source: source.to_owned(),
            options: options.wire(),
            progress: progress.is_some(),
        })?;
        self.record_request(false);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::KernelCheck { outcome } => Ok(outcome.into()),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_declaration_kinds(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        const OPERATION: &str = "worker_declaration_kinds";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false)?;
        self.send_request(Request::DeclarationKinds {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            progress: progress.is_some(),
        })?;
        self.record_request(false);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::Strings { values } => Ok(values),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_declaration_names(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        const OPERATION: &str = "worker_declaration_names";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false)?;
        self.send_request(Request::DeclarationNames {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            progress: progress.is_some(),
        })?;
        self.record_request(false);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::Strings { values } => Ok(values),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
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
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::RowsComplete { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_capability_metadata(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerCapabilityMetadata, LeanWorkerError> {
        const OPERATION: &str = "worker_capability_metadata";
        check_cancelled(OPERATION, cancellation)?;
        let request_json = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability metadata request JSON encode failed: {err}"),
        })?;
        self.prepare_request(false)?;
        self.send_request(Request::CapabilityMetadata {
            export: export.to_owned(),
            request_json,
        })?;
        self.record_request(false);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::CapabilityMetadata { metadata } => Ok(metadata.into()),
            Response::CapabilityMetadataMalformed { message } => {
                Err(LeanWorkerError::CapabilityMetadataMalformed { message })
            }
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_capability_doctor(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDoctorReport, LeanWorkerError> {
        const OPERATION: &str = "worker_capability_doctor";
        check_cancelled(OPERATION, cancellation)?;
        let request_json = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability doctor request JSON encode failed: {err}"),
        })?;
        self.prepare_request(false)?;
        self.send_request(Request::CapabilityDoctor {
            export: export.to_owned(),
            request_json,
        })?;
        self.record_request(false);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::CapabilityDoctor { report } => Ok(report.into()),
            Response::CapabilityDoctorMalformed { message } => {
                Err(LeanWorkerError::CapabilityDoctorMalformed { message })
            }
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::JsonCommand { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    pub(crate) fn worker_json_command(
        &mut self,
        export: &str,
        request_json: String,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<String, LeanWorkerError> {
        const OPERATION: &str = "worker_json_command";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_request(false)?;
        self.send_request(Request::JsonCommand {
            export: export.to_owned(),
            request_json,
        })?;
        self.record_request(false);
        match self.read_response_with_progress(OPERATION, progress, cancellation)? {
            Response::JsonCommand { response_json } => Ok(response_json),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
            | Response::U64 { .. }
            | Response::HostSessionOpened
            | Response::Elaboration { .. }
            | Response::KernelCheck { .. }
            | Response::Strings { .. }
            | Response::StreamComplete { .. }
            | Response::StreamExportFailed { .. }
            | Response::StreamCallbackFailed { .. }
            | Response::StreamRowMalformed { .. }
            | Response::CapabilityMetadata { .. }
            | Response::CapabilityDoctor { .. }
            | Response::CapabilityMetadataMalformed { .. }
            | Response::CapabilityDoctorMalformed { .. }
            | Response::RowsComplete { .. }
            | Response::Terminating
            | Response::Error { .. }) => Err(unexpected_response(OPERATION, &other)),
        }
    }

    fn send_request(&mut self, request: Request) -> Result<(), LeanWorkerError> {
        self.ensure_running()?;
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.dead_error());
        };
        write_frame(stdin, Message::Request(request)).map_err(|err| LeanWorkerError::Protocol {
            message: err.to_string(),
        })
    }

    fn prepare_request(&mut self, import_like: bool) -> Result<(), LeanWorkerError> {
        self.ensure_running()?;

        if let Some(limit) = self.config.restart_policy.max_requests
            && self.requests_since_restart >= limit
        {
            return self.restart_with_reason(LeanWorkerRestartReason::MaxRequests { limit });
        }

        if import_like
            && let Some(limit) = self.config.restart_policy.max_imports
            && self.imports_since_restart >= limit
        {
            return self.restart_with_reason(LeanWorkerRestartReason::MaxImports { limit });
        }

        if let Some(limit_kib) = self.config.restart_policy.max_rss_kib {
            match self.child_rss_kib() {
                Some(current_kib) if current_kib >= limit_kib => {
                    self.stats.last_rss_kib = Some(current_kib);
                    return self.restart_with_reason(LeanWorkerRestartReason::RssCeiling { current_kib, limit_kib });
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
                return self.restart_with_reason(LeanWorkerRestartReason::Idle { idle_for, limit });
            }
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
        let config = self.config.clone();
        self.stop_existing_child()?;
        self.stats.record_restart(reason);
        self.requests_since_restart = 0;
        self.imports_since_restart = 0;
        let mut next = Self::spawn(&config)?;
        next.stats = self.stats.clone();
        next.last_activity = Instant::now();
        *self = next;
        Ok(())
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
        let stdout = self.stdout.take().ok_or_else(|| self.dead_error())?;
        let (sender, receiver) = mpsc::channel();
        let _reader = thread::spawn(move || read_request_messages(stdout, sender));

        loop {
            let event = match deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now())) {
                Some(remaining) if remaining.is_zero() => {
                    self.restart_with_reason(LeanWorkerRestartReason::RequestTimeout {
                        operation,
                        duration: timeout,
                    })?;
                    return Err(LeanWorkerError::Timeout {
                        operation,
                        duration: timeout,
                    });
                }
                Some(remaining) => match receiver.recv_timeout(remaining) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        self.restart_with_reason(LeanWorkerRestartReason::RequestTimeout {
                            operation,
                            duration: timeout,
                        })?;
                        return Err(LeanWorkerError::Timeout {
                            operation,
                            duration: timeout,
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(LeanWorkerError::Protocol {
                            message: "worker response reader exited without a terminal response".to_owned(),
                        });
                    }
                },
                None => match receiver.recv() {
                    Ok(event) => event,
                    Err(_err) => {
                        return Err(LeanWorkerError::Protocol {
                            message: "worker response reader exited without a terminal response".to_owned(),
                        });
                    }
                },
            };

            let message = match event {
                RequestReaderEvent::Message(message) => message,
                RequestReaderEvent::Terminal(message, stdout) => {
                    self.stdout = Some(stdout);
                    match message {
                        Message::Response(Response::Error { code, message }) => {
                            return Err(LeanWorkerError::Worker { code, message });
                        }
                        Message::Response(response) => return Ok(response),
                        other @ (Message::Handshake { .. }
                        | Message::Request(_)
                        | Message::Diagnostic(_)
                        | Message::ProgressTick(_)
                        | Message::DataRow(_)
                        | Message::FatalExit(_)) => {
                            return Err(LeanWorkerError::Protocol {
                                message: format!("worker sent unexpected {operation} message: {other:?}"),
                            });
                        }
                    }
                }
                RequestReaderEvent::ReadError { message, eof } => {
                    return if eof {
                        Err(self.record_exit_error())
                    } else {
                        Err(LeanWorkerError::Protocol { message })
                    };
                }
            };

            match message {
                Message::ProgressTick(tick) => {
                    report_parent_progress(progress, elapsed_event(tick.phase, tick.current, tick.total, started))?;
                    if cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
                        self.restart_with_reason(LeanWorkerRestartReason::Cancelled { operation })?;
                        return Err(LeanWorkerError::Cancelled { operation });
                    }
                }
                Message::DataRow(row) => {
                    report_parent_data_row(data, row)?;
                    if cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
                        self.restart_with_reason(LeanWorkerRestartReason::Cancelled { operation })?;
                        return Err(LeanWorkerError::Cancelled { operation });
                    }
                }
                Message::Diagnostic(diagnostic) => {
                    report_parent_diagnostic(diagnostics, diagnostic.into())?;
                }
                Message::Response(response) => return Err(unexpected_response(operation, &response)),
                other @ (Message::Handshake { .. } | Message::Request(_) | Message::FatalExit(_)) => {
                    return Err(LeanWorkerError::Protocol {
                        message: format!("worker sent unexpected {operation} message: {other:?}"),
                    });
                }
            }
        }
    }

    fn ensure_running(&mut self) -> Result<(), LeanWorkerError> {
        match self.status()? {
            LeanWorkerStatus::Running => Ok(()),
            LeanWorkerStatus::Exited(exit) if exit.success => Err(LeanWorkerError::ChildExited { exit }),
            LeanWorkerStatus::Exited(exit) => Err(LeanWorkerError::ChildPanicOrAbort { exit }),
        }
    }

    fn wait_for_exit(&mut self) -> Result<LeanWorkerExit, LeanWorkerError> {
        let Some(child) = self.child.as_mut() else {
            return Err(self.dead_error());
        };
        let status = child.wait().map_err(|source| LeanWorkerError::Wait { source })?;
        let diagnostics = self.read_stderr();
        let exit = LeanWorkerExit::from_status(status, diagnostics);
        self.last_exit = Some(exit.clone());
        self.child = None;
        self.stdin = None;
        self.stdout = None;
        self.stats.exits = self.stats.exits.saturating_add(1);
        Ok(exit)
    }

    fn try_record_exit(&mut self) -> Option<LeanWorkerExit> {
        let child = self.child.as_mut()?;
        let status = child.try_wait().ok().flatten()?;
        let diagnostics = self.read_stderr();
        let exit = LeanWorkerExit::from_status(status, diagnostics);
        self.last_exit = Some(exit.clone());
        self.child = None;
        self.stdin = None;
        self.stdout = None;
        self.stats.exits = self.stats.exits.saturating_add(1);
        Some(exit)
    }

    fn record_exit_error(&mut self) -> LeanWorkerError {
        match self.wait_for_exit() {
            Ok(exit) if exit.success => LeanWorkerError::ChildExited { exit },
            Ok(exit) => LeanWorkerError::ChildPanicOrAbort { exit },
            Err(err) => err,
        }
    }

    fn stop_existing_child(&mut self) -> Result<(), LeanWorkerError> {
        if let Some(child) = self.child.as_mut() {
            drop(child.kill());
            let status = child.wait().map_err(|source| LeanWorkerError::Wait { source })?;
            let diagnostics = self.read_stderr();
            self.last_exit = Some(LeanWorkerExit::from_status(status, diagnostics));
            self.stats.exits = self.stats.exits.saturating_add(1);
        }
        self.child = None;
        self.stdin = None;
        self.stdout = None;
        Ok(())
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
    Message(Message),
    Terminal(Message, BufReader<ChildStdout>),
    ReadError { message: String, eof: bool },
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the request reader thread must own the sender"
)]
fn read_request_messages(mut stdout: BufReader<ChildStdout>, sender: mpsc::Sender<RequestReaderEvent>) {
    loop {
        match read_frame(&mut stdout) {
            Ok(frame) if matches!(frame.message, Message::Response(_)) => {
                drop(sender.send(RequestReaderEvent::Terminal(frame.message, stdout)));
                return;
            }
            Ok(frame) => {
                if sender.send(RequestReaderEvent::Message(frame.message)).is_err() {
                    return;
                }
            }
            Err(err) => {
                drop(sender.send(RequestReaderEvent::ReadError {
                    message: err.to_string(),
                    eof: err.is_eof(),
                }));
                return;
            }
        }
    }
}

impl Drop for LeanWorker {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            drop(child.kill());
            drop(child.wait());
        }
    }
}

fn expect_handshake(stdout: &mut BufReader<ChildStdout>) -> Result<LeanWorkerRuntimeMetadata, LeanWorkerError> {
    let frame = read_frame(stdout).map_err(|err| {
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
        } if protocol_version == crate::protocol::PROTOCOL_VERSION => Ok(LeanWorkerRuntimeMetadata {
            worker_version,
            protocol_version,
            lean_version: None,
        }),
        other @ (Message::Handshake { .. }
        | Message::Request(_)
        | Message::Response(_)
        | Message::Diagnostic(_)
        | Message::ProgressTick(_)
        | Message::DataRow(_)
        | Message::FatalExit(_)) => Err(LeanWorkerError::Handshake {
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

fn unexpected_response(operation: &'static str, response: &Response) -> LeanWorkerError {
    LeanWorkerError::Protocol {
        message: format!("worker sent unexpected {operation} response: {response:?}"),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
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
