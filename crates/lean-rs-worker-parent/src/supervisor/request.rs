//! One request at a time over the child's pipes.
//!
//! This module writes request frames and tracks the request in flight.
//! `reader` reads the child's replies, progress, and data rows until the
//! terminal frame. `operations` holds one typed method per request kind.

pub(super) mod operations;
pub(super) mod reader;

use std::io::{self, Write};
use std::process::ChildStdin;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::protocol::{Message, Request, Response, write_frame};

use super::error::{LeanWorkerError, worker_resource_facts};
use super::restart::LeanWorkerRestartReason;
use super::shutdown::ShutdownIntent;
use super::{
    InFlightRequest, LeanWorker, LeanWorkerPendingRequestStop, LeanWorkerStatus, WorkerRequestId, WorkerSupervisorState,
};
use crate::pipe::Pipe;
use crate::session::{LeanWorkerCancellationToken, check_cancelled};

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

pub(super) fn frame_timeout(started: Instant, configured: Duration, deadline: Option<Instant>) -> Duration {
    deadline.map_or(configured, |end| configured.min(end.saturating_duration_since(started)))
}

impl LeanWorker {
    fn send_request(&mut self, operation: &'static str, request: Request) -> Result<(), LeanWorkerError> {
        self.send_request_cancellable(request, operation, None)
    }

    pub(super) fn send_request_cancellable(
        &mut self,
        request: Request,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        check_cancelled(operation, cancellation)?;
        self.ensure_running()?;
        let deadline = cancellation.and_then(LeanWorkerCancellationToken::deadline);
        if deadline.is_none() {
            self.begin_in_flight(operation)?;
            // A later dispatched request supersedes a prior pre-body refusal.
            self.last_host_session_refusal = None;
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
                LeanWorkerError::Spawn { .. }
                | LeanWorkerError::WorkerChildUnresolved { .. }
                | LeanWorkerError::WorkerChildNotExecutable { .. }
                | LeanWorkerError::Bootstrap { .. }
                | LeanWorkerError::CapabilityBuild { .. }
                | LeanWorkerError::Setup { .. }
                | LeanWorkerError::Handshake { .. }
                | LeanWorkerError::Protocol { .. }
                | LeanWorkerError::Worker { .. }
                | LeanWorkerError::ChildExited { .. }
                | LeanWorkerError::ChildPanicOrAbort { .. }
                | LeanWorkerError::RssHardLimitExceeded { .. }
                | LeanWorkerError::ProgressPanic { .. }
                | LeanWorkerError::DataSinkPanic { .. }
                | LeanWorkerError::CapabilityMetadataMalformed { .. }
                | LeanWorkerError::CapabilityDoctorMalformed { .. }
                | LeanWorkerError::RestartLimitExceeded { .. }
                | LeanWorkerError::UnsupportedRequest { .. }
                | LeanWorkerError::Wait { .. }
                | LeanWorkerError::Kill { .. }
                | LeanWorkerError::WaitTimeout { .. }
                | LeanWorkerError::ShutdownInProgress { .. } => {
                    self.finish_in_flight();
                    // A failed pipe write kills and reaps the child.
                    if self.child.is_none() && self.pending_startup.is_none() {
                        return Err(self.replace_failed_child(operation, error));
                    }
                    return Err(error);
                }
            };
            self.restart_with_reason(reason)?;
            return Err(error);
        }
        self.request_started = Some(started);
        Ok(())
    }

    pub(super) fn write_request(&mut self, request: Request) -> Result<(), LeanWorkerError> {
        self.write_request_bounded(request, "shutdown_send", None, self.config.shutdown_timeout)
    }

    pub(super) fn write_request_bounded(
        &mut self,
        request: Request,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        timeout: Duration,
    ) -> Result<(), LeanWorkerError> {
        let deadline = cancellation.and_then(LeanWorkerCancellationToken::deadline);
        let planned = if deadline.is_some() {
            // ensure_running may have blocked. Check again before admitting a frame.
            check_cancelled(operation, cancellation)?;
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
        let (result, entered) = if let Some((in_flight, next)) = planned {
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
                self.last_host_session_refusal = None;
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
        let cancelled = cancellation.is_some_and(LeanWorkerCancellationToken::is_cancelled);
        match result {
            Ok(()) => {
                self.stdin = Some(stdin);
                Ok(())
            }
            Err(lean_rs_worker_protocol::protocol::ProtocolError::Io(error)) => {
                if deadline.is_some() && !entered && error.kind() == io::ErrorKind::TimedOut {
                    self.stdin = Some(stdin);
                } else {
                    // Prevent BufWriter::drop from retrying a partial buffered write.
                    stdin.get_ref().stopped.store(true, Ordering::Release);
                    drop(stdin);
                    if deadline.is_some_and(|end| {
                        error.kind() == io::ErrorKind::TimedOut && (!cancelled || Instant::now() >= end)
                    }) {
                        self.stop_for_scoped_expiry(if cancelled {
                            LeanWorkerRestartReason::Cancelled { operation }
                        } else {
                            LeanWorkerRestartReason::RequestTimeout {
                                operation,
                                duration: timeout,
                            }
                        });
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
            self.stop_for_scoped_expiry(reason);
            release_reader();
            Ok(())
        } else {
            self.restart_with_reason_before_spawn(reason, release_reader)
        }
    }

    /// Kill the child of a request whose deadline has passed, and leave its
    /// replacement to the next request or cycle.
    ///
    /// A successor cannot start inside a deadline that has already passed.
    /// `reason` waits in `recovery` so the replacement still records it.
    fn stop_for_scoped_expiry(&mut self, reason: LeanWorkerRestartReason) {
        // The operation has timed out even if cleanup cannot confirm exit.
        // Keep that case fenced and observable via cleanup_pending and the request's stop receipt.
        self.recovery = Some(reason);
        self.state = WorkerSupervisorState::Killing {
            generation: self.generation,
        };
        drop(self.shutdown_child(ShutdownIntent::KillOnly));
    }

    pub(super) fn begin_in_flight(&mut self, operation: &'static str) -> Result<InFlightRequest, LeanWorkerError> {
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
            WorkerSupervisorState::Idle { .. }
            | WorkerSupervisorState::Streaming { .. }
            | WorkerSupervisorState::Stopping { .. }
            | WorkerSupervisorState::Killing { .. }
            | WorkerSupervisorState::Reaping { .. }
            | WorkerSupervisorState::Crashed { .. }
            | WorkerSupervisorState::RestartExhausted { .. }
            | WorkerSupervisorState::Exited { .. } => {
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
            WorkerSupervisorState::Idle { .. }
            | WorkerSupervisorState::Stopping { .. }
            | WorkerSupervisorState::Killing { .. }
            | WorkerSupervisorState::Reaping { .. }
            | WorkerSupervisorState::Crashed { .. }
            | WorkerSupervisorState::RestartExhausted { .. }
            | WorkerSupervisorState::Exited { .. } => {}
        }
    }

    pub(super) fn finish_in_flight(&mut self) {
        if matches!(
            self.state,
            WorkerSupervisorState::Busy { .. } | WorkerSupervisorState::Streaming { .. }
        ) {
            self.state = WorkerSupervisorState::Idle {
                generation: self.generation,
            };
        }
    }

    fn ensure_running(&mut self) -> Result<(), LeanWorkerError> {
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
}

fn unexpected_response(operation: &'static str, response: &Response) -> LeanWorkerError {
    LeanWorkerError::Protocol {
        message: format!("worker sent unexpected {operation} response: {response:?}"),
    }
}
