//! Stopping the worker child: the terminate request, kill after the grace
//! period, reaping, and the exit record.

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::protocol::{Message, Request, Response};

use super::config::LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT;
use super::error::LeanWorkerError;
use super::request::reader::{ReaderGuard, RequestReaderEvent, WORKER_EVENT_BUFFER_CAPACITY, read_request_messages};
use super::startup::{StartupChild, StderrCapture};
use super::{LeanWorker, LeanWorkerExit, LeanWorkerRequestStopReceipt, LeanWorkerStatus, WorkerSupervisorState};

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

impl LeanWorker {
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
    /// The worker then refuses requests with
    /// [`LeanWorkerError::ShutdownInProgress`] until [`Self::cycle`] starts a
    /// new child.
    ///
    /// # Errors
    /// Returns the cleanup error while retaining the child for another attempt.
    pub fn try_shutdown(&mut self) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        self.stopped = true;
        self.shutdown_child(ShutdownIntent::Graceful)
    }

    pub(super) fn wait_for_exit(&mut self) -> Result<LeanWorkerExit, LeanWorkerError> {
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

    pub(super) fn finalize_child_exit(&mut self, status: ExitStatus) -> LeanWorkerExit {
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
        self.stdin = None;
        self.stdout = None;
        self.finish_in_flight();
        self.state = WorkerSupervisorState::Exited {
            generation: self.generation,
        };
        self.stats.exits = self.stats.exits.saturating_add(1);
        exit
    }

    pub(super) fn record_exit_error(&mut self) -> LeanWorkerError {
        self.state = WorkerSupervisorState::Crashed {
            generation: self.generation,
        };
        match self.wait_for_exit() {
            Ok(exit) if exit.success => LeanWorkerError::ChildExited { exit },
            Ok(exit) => LeanWorkerError::ChildPanicOrAbort { exit },
            Err(err) => err,
        }
    }

    pub(super) fn shutdown_child(
        &mut self,
        intent: ShutdownIntent,
    ) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        self.shutdown_child_with_cleanup(intent, StartupChild::cleanup)
    }

    pub(super) fn shutdown_child_with_cleanup(
        &mut self,
        intent: ShutdownIntent,
        cleanup: impl FnOnce(&mut StartupChild) -> Result<ExitStatus, LeanWorkerError>,
    ) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
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

    pub(super) fn dead_error(&self) -> LeanWorkerError {
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ShutdownIntent {
    Graceful,
    KillOnly,
}

impl Drop for LeanWorker {
    fn drop(&mut self) {
        drop(self.shutdown_child(ShutdownIntent::Graceful));
    }
}
