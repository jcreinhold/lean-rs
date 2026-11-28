//! Starting a worker child: spawn, handshake, the stderr tail, and cleanup of
//! a child whose startup failed.

use std::collections::VecDeque;
use std::io::{self, BufReader, BufWriter, Read as _};
use std::process::{ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::protocol::{Message, read_frame, write_frame};

use super::config::{LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT, LeanWorkerConfig};
use super::error::{LeanWorkerError, resource_facts};
use super::request::frame_timeout;
use super::restart::GenerationBudget;
use super::{LeanWorker, LeanWorkerExit, LeanWorkerStats, WorkerGeneration, WorkerRequestId, WorkerSupervisorState};
use crate::pipe::Pipe;
use crate::process::{CheckedSpawnError, OwnedChild, inherit_checked_native_loader_environment};
use crate::session::{LeanWorkerCancellationToken, LeanWorkerRuntimeMetadata, check_cancelled};

// Drain stderr from startup onward: pipe capacity must never stop Lean. Keep
// only the newest bytes, even when a worker writes without newlines.
pub(super) const STDERR_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(super) struct StderrCapture {
    tail: Arc<Mutex<VecDeque<u8>>>,
    stopped: Arc<AtomicBool>,
    // The private reader returns (), invokes no user code, and shares only
    // mutex-protected bytes and an atomic stop flag. Parent unwinding cannot
    // expose an inconsistent value through its handle. Keep this assertion
    // narrow; revisit it if the reader ever carries user callbacks or data.
    reader: Option<std::panic::AssertUnwindSafe<thread::JoinHandle<()>>>,
}

impl StderrCapture {
    pub(super) fn start(pipe: ChildStderr) -> std::io::Result<Self> {
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

    pub(super) fn finish(mut self) -> String {
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

/// Own a newly spawned process until all pipe/handshake/config setup succeeds.
/// `Child` itself does not terminate or reap its process when dropped.
#[derive(Debug)]
pub(super) struct StartupChild(pub(super) Option<OwnedChild>);

impl StartupChild {
    pub(super) fn child_mut(&mut self) -> Result<&mut OwnedChild, LeanWorkerError> {
        self.0.as_mut().ok_or_else(|| LeanWorkerError::Setup {
            message: "startup child was already transferred".to_owned(),
        })
    }

    fn take_child(&mut self) -> Result<OwnedChild, LeanWorkerError> {
        self.0.take().ok_or_else(|| LeanWorkerError::Setup {
            message: "startup child was already transferred".to_owned(),
        })
    }

    pub(super) fn cleanup(&mut self) -> Result<ExitStatus, LeanWorkerError> {
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
pub(super) struct SpawnFailure {
    pub(super) error: LeanWorkerError,
    pub(super) pending: Option<Box<StartupChild>>,
    pub(super) exit: Option<Box<LeanWorkerExit>>,
}

impl SpawnFailure {
    pub(super) fn no_child(error: LeanWorkerError) -> Self {
        Self {
            error,
            pending: None,
            exit: None,
        }
    }

    pub(super) fn after_spawn(
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

pub(super) type StartupScope<'a> = (&'static str, &'a LeanWorkerCancellationToken);

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

impl LeanWorker {
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

    pub(super) fn spawn_tracked(config: &LeanWorkerConfig) -> Result<Self, SpawnFailure> {
        Self::spawn_tracked_scoped(config, None)
    }

    pub(super) fn spawn_tracked_scoped(
        config: &LeanWorkerConfig,
        scope: Option<StartupScope<'_>>,
    ) -> Result<Self, SpawnFailure> {
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
        // This child is a runtime process. A parent running inside Cargo may pass
        // on a build script's OUT_DIR, which would move the child's capability
        // manifests.
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
        // A prepared descriptor reaches this child only through its own configuration.
        command.env_remove(lean_rs_worker_protocol::PREPARED_HOST_ARTIFACTS_ENV);
        for (key, value) in &config.env {
            command.env(key, value);
        }
        if config.check_loader_environment {
            inherit_checked_native_loader_environment(&mut command).map_err(|error| {
                SpawnFailure::no_child(LeanWorkerError::Setup {
                    message: error.to_string(),
                })
            })?;
        }
        // Install this reserved value after inherited and explicit generic environment
        // choices, on the common path for both initial and replacement generations.
        if let Some(input) = &config.application_launch_input {
            command.env(lean_rs_worker_protocol::WORKER_APPLICATION_INPUT_ENV, input);
        } else {
            command.env_remove(lean_rs_worker_protocol::WORKER_APPLICATION_INPUT_ENV);
        }
        // Its own process group keeps the worker and the processes it starts
        // apart from this process's terminal and its other children.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);

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

        let spawned = if scope.is_some() {
            config.process_tracker.spawn_checked(command, admit)
        } else {
            config.process_tracker.spawn(command).map_err(CheckedSpawnError::Io)
        };
        let mut child = StartupChild(Some(spawned.map_err(|failure| {
            SpawnFailure::no_child(match failure {
                CheckedSpawnError::Io(source) => LeanWorkerError::Spawn {
                    executable: config.executable.clone(),
                    source,
                },
                CheckedSpawnError::Refused(reason) => reason,
            })
        })?));
        Self::initialize_child(config, spawn_started, startup_timeout, scope.is_some(), &mut child)
            .map_err(|error| SpawnFailure::after_spawn(child, error, StartupChild::cleanup))
    }

    fn initialize_child(
        config: &LeanWorkerConfig,
        spawn_started: Instant,
        startup_timeout: Duration,
        scoped: bool,
        child: &mut StartupChild,
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

        // Spawn and pipe setup already consumed startup time. READY shares that
        // budget, including the existing zero-wait cleanup path.
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
            last_request: None,
            last_request_stop: None,
            last_request_exit: None,
            pending_startup: None,
            runtime_metadata,
            host_runtime_metadata: None,
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
            recovery: None,
            stopped: false,
            state: WorkerSupervisorState::Idle {
                generation: WorkerGeneration::default(),
            },
        })
    }
}

#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "Message is #[non_exhaustive] across the lean-rs-worker-protocol crate boundary; the wildcard arm \
              uniformly rejects any non-handshake frame"
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
