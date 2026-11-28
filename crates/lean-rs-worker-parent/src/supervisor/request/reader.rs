//! Reading the child's output for one request.
//!
//! A reader thread decodes frames from the child's stdout and passes them on.
//! The supervisor waits for the terminal reply under the request's timeout,
//! cancellation token, and hard RSS limit, and forwards progress and data rows
//! to the caller's sinks on the way.

use std::io::BufReader;
use std::process::ChildStdout;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::protocol::{Message, Response, read_frame};

use super::{frame_timeout, unexpected_response};
use crate::pipe::Pipe;
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerProgressSink, LeanWorkerRawDataSink, elapsed_event, report_parent_data_row,
    report_parent_progress,
};
use crate::supervisor::error::{LeanWorkerError, worker_resource_facts};
use crate::supervisor::restart::LeanWorkerRestartReason;
use crate::supervisor::{LeanWorker, WorkerGeneration, WorkerRequestId, WorkerSupervisorState};

pub(in crate::supervisor) const WORKER_EVENT_BUFFER_CAPACITY: usize = 64;

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(in crate::supervisor) struct ReaderGuard {
    pub(in crate::supervisor) stopped: Arc<AtomicBool>,
    pub(in crate::supervisor) thread: Option<thread::JoinHandle<()>>,
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

impl LeanWorker {
    pub(super) fn read_response(&mut self, operation: &'static str) -> Result<Response, LeanWorkerError> {
        self.read_response_with_events(operation, None, None, None)
    }

    pub(super) fn read_response_with_progress(
        &mut self,
        operation: &'static str,
        progress: Option<&dyn LeanWorkerProgressSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<Response, LeanWorkerError> {
        self.read_response_with_events(operation, progress, cancellation, None)
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Message is #[non_exhaustive] in lean-rs-worker-protocol; any other message is a protocol error"
    )]
    pub(super) fn read_response_with_events(
        &mut self,
        operation: &'static str,
        progress: Option<&dyn LeanWorkerProgressSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        data: Option<&dyn LeanWorkerRawDataSink>,
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
                            drop(reader);
                            return Err(self.reader_lost(operation));
                        }
                    }
                }
                None if cancellation.is_some() => match receiver.recv_timeout(CANCELLATION_POLL_INTERVAL) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        drop(reader);
                        return Err(self.reader_lost(operation));
                    }
                },
                None => match receiver.recv() {
                    Ok(event) => event,
                    Err(_err) => {
                        drop(reader);
                        return Err(self.reader_lost(operation));
                    }
                },
            };
            #[cfg(all(test, unix))]
            let event = crate::supervisor::terminal_frame_tests::observe_fixture_terminal(event, &mut deadline);
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
                    return Err(if eof {
                        let error = self.record_exit_error();
                        self.replace_exited_child(operation, error)
                    } else {
                        self.replace_failed_child(operation, LeanWorkerError::Protocol { message })
                    });
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
                        return Err(self.replace_failed_child(operation, err));
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
                        return Err(self.replace_failed_child(operation, err));
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
                Message::Response(response) => {
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    let error = unexpected_response(operation, &response);
                    return Err(self.replace_failed_child(operation, error));
                }
                other => {
                    self.terminalize_request_failure(streaming, started, request_backpressure_waits);
                    let error = LeanWorkerError::Protocol {
                        message: format!("worker sent unexpected {operation} message: {other:?}"),
                    };
                    return Err(self.replace_failed_child(operation, error));
                }
            }
        }
    }

    /// The response reader stopped without a terminal response, so the child's
    /// output is lost: kill and replace the child.
    fn reader_lost(&mut self, operation: &'static str) -> LeanWorkerError {
        self.finish_in_flight();
        let error = LeanWorkerError::Protocol {
            message: "worker response reader exited without a terminal response".to_owned(),
        };
        self.replace_failed_child(operation, error)
    }

    fn terminalize_request_response(
        &mut self,
        response: Response,
        streaming: bool,
        started: Instant,
        backpressure_waits: u64,
    ) -> Response {
        if streaming {
            if matches!(response, Response::RowsComplete { .. }) {
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
}

pub(in crate::supervisor) enum RequestReaderEvent {
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
pub(in crate::supervisor) fn read_request_messages(
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

pub(in crate::supervisor) fn send_reader_event(
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

pub(in crate::supervisor) fn stale_worker_output_error(
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

pub(in crate::supervisor) fn stale_worker_request_output_error(
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
