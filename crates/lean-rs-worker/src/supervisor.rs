use std::ffi::OsString;
use std::fmt;
use std::io::{BufReader, BufWriter, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::protocol::{Message, Request, Response, read_frame, write_frame};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

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
}

impl LeanWorkerConfig {
    /// Create a worker configuration for a child executable.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            current_dir: None,
            env: Vec::new(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
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
            Self::Setup { .. }
            | Self::Handshake { .. }
            | Self::Protocol { .. }
            | Self::Worker { .. }
            | Self::ChildExited { .. }
            | Self::ChildPanicOrAbort { .. }
            | Self::Timeout { .. }
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

        let stdout = match receiver.recv_timeout(config.startup_timeout) {
            Ok((stdout, Ok(()))) => stdout,
            Ok((_stdout, Err(err))) => {
                let mut worker = Self {
                    config: config.clone(),
                    child: Some(child),
                    stdin: Some(stdin),
                    stdout: None,
                    stderr,
                    last_exit: None,
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
        })
    }

    /// Check whether the worker responds to requests.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the protocol fails, or
    /// the child returns a typed worker error.
    pub fn health(&mut self) -> Result<(), LeanWorkerError> {
        self.send_request(Request::Health)?;
        match self.read_response("health")? {
            Response::HealthOk => Ok(()),
            other @ (Response::CapabilityLoaded
            | Response::U64 { .. }
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
        self.send_request(Request::LoadFixtureCapability {
            fixture_root: path_string(fixture_root.as_ref()),
        })?;
        match self.read_response("load_fixture_capability")? {
            Response::CapabilityLoaded => Ok(()),
            other @ (Response::HealthOk | Response::U64 { .. } | Response::Terminating | Response::Error { .. }) => {
                Err(unexpected_response("load_fixture_capability", &other))
            }
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
        self.send_request(Request::CallFixtureMul {
            fixture_root: path_string(fixture_root.as_ref()),
            lhs,
            rhs,
        })?;
        match self.read_response("call_fixture_mul")? {
            Response::U64 { value } => Ok(value),
            other @ (Response::HealthOk
            | Response::CapabilityLoaded
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
                Ok(LeanWorkerStatus::Exited(exit))
            }
            None => Ok(LeanWorkerStatus::Running),
        }
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
        let config = self.config.clone();
        self.stop_existing_child()?;
        let next = Self::spawn(&config)?;
        *self = next;
        Ok(())
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
        self.send_request(Request::TriggerLeanPanic {
            fixture_root: path_string(fixture_root.as_ref()),
        })?;
        match self.read_response("trigger_lean_panic") {
            Ok(response) => Err(unexpected_response("trigger_lean_panic", &response)),
            Err(LeanWorkerError::ChildPanicOrAbort { exit }) => Ok(exit),
            Err(err) => Err(err),
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

    fn read_response(&mut self, operation: &'static str) -> Result<Response, LeanWorkerError> {
        let Some(stdout) = self.stdout.as_mut() else {
            return Err(self.dead_error());
        };
        let frame = match read_frame(stdout) {
            Ok(frame) => frame,
            Err(err) if err.is_eof() => return Err(self.record_exit_error()),
            Err(err) => {
                return Err(LeanWorkerError::Protocol {
                    message: err.to_string(),
                });
            }
        };
        match frame.message {
            Message::Response(Response::Error { code, message }) => Err(LeanWorkerError::Worker { code, message }),
            Message::Response(response) => Ok(response),
            other @ (Message::Handshake { .. }
            | Message::Request(_)
            | Message::Diagnostic(_)
            | Message::ProgressTick(_)
            | Message::FatalExit(_)) => Err(LeanWorkerError::Protocol {
                message: format!("worker sent unexpected {operation} message: {other:?}"),
            }),
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
}

impl Drop for LeanWorker {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            drop(child.kill());
            drop(child.wait());
        }
    }
}

fn expect_handshake(stdout: &mut BufReader<ChildStdout>) -> Result<(), LeanWorkerError> {
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
        Message::Handshake { protocol_version, .. } if protocol_version == crate::protocol::PROTOCOL_VERSION => Ok(()),
        other @ (Message::Handshake { .. }
        | Message::Request(_)
        | Message::Response(_)
        | Message::Diagnostic(_)
        | Message::ProgressTick(_)
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
