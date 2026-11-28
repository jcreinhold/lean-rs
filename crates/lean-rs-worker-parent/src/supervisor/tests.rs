use super::error::{DISPLAY_DIAGNOSTICS_MAX_BYTES, truncate_for_display};
use super::request::reader::{stale_worker_output_error, stale_worker_request_output_error};
use super::restart::exit_reason;
use super::{
    GenerationBudget, LeanWorkerConfig, LeanWorkerError, LeanWorkerExit, LeanWorkerImportStats,
    LeanWorkerLifecycleSnapshot, LeanWorkerRestartPolicy, LeanWorkerRestartReason, LeanWorkerStats, WorkerGeneration,
    WorkerRequestId,
};
use lean_rs_worker_protocol::protocol::{MAX_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP, MIN_FRAME_BYTES};
use lean_rs_worker_protocol::types::{
    LeanWorkerDeclarationVerificationBatchItem, LeanWorkerDeclarationVerificationBatchRequest,
    LeanWorkerDeclarationVerificationBatchResult, LeanWorkerDeclarationVerificationFacts,
    LeanWorkerDeclarationVerificationStatus, LeanWorkerDeclarationVerificationTarget, LeanWorkerModuleQueryBatchItem,
    LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQuerySelector, LeanWorkerOutputBudgets, LeanWorkerSorryPolicy,
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
    let capture = super::StderrCapture::start(child.stderr.take().expect("stderr pipe")).expect("nonblocking stderr");
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
    assert_eq!(tail.len(), super::startup::STDERR_TAIL_BYTES);
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
    let capture = super::StderrCapture::start(child.stderr.take().expect("stderr pipe")).expect("nonblocking stderr");
    let started = std::time::Instant::now();
    assert!(capture.finish().is_empty());
    assert!(started.elapsed() < Duration::from_millis(500));
    child.kill().expect("kill pipe holder");
    child.wait().expect("reap pipe holder");
}

#[cfg(unix)]
fn blocked_stdin_worker() -> super::LeanWorker {
    piped_worker(std::process::Command::new("sleep").arg("60"))
}

#[cfg(unix)]
fn piped_worker(command: &mut std::process::Command) -> super::LeanWorker {
    let mut worker = sleeping_worker();
    worker
        .shutdown_child(super::shutdown::ShutdownIntent::KillOnly)
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
    worker.child = Some(crate::process::LeanWorkerProcessTracker::new().own(child));
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
    let scope = crate::session::LeanWorkerCancellationToken::new().with_deadline(super::Instant::now());
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
    let scope = crate::session::LeanWorkerCancellationToken::new().with_deadline(deadline);
    // The policy recycles before dispatch. The replacement shell never sends READY.
    let error = worker
        .worker_capability_metadata("fixture", &serde_json::json!({}), Some(&scope), None)
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
    let metadata = lean_rs_worker_protocol::types::LeanWorkerCapabilityMetadata {
        commands: Vec::new(),
        capabilities: Vec::new(),
        lean_version: None,
        extra: None,
    };
    let response = lean_rs_worker_protocol::protocol::Frame::new(lean_rs_worker_protocol::protocol::Message::Response(
        lean_rs_worker_protocol::protocol::Response::CapabilityMetadata {
            metadata: metadata.clone(),
        },
    ));
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
        worker.worker_capability_metadata("fixture", &serde_json::json!({}), Some(scope), None)
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
    assert_eq!(request(&mut worker, &scope).expect("first frame answered"), metadata);
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
    assert_eq!(receipt.operation(), "worker_capability_metadata");
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
        lean_rs_worker_protocol::protocol::Request::CapabilityMetadata {
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
        lean_rs_worker_protocol::protocol::Request::CapabilityMetadata {
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
        lean_rs_worker_protocol::protocol::Request::CapabilityMetadata {
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
        .shutdown_child(super::shutdown::ShutdownIntent::Graceful)
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
    let event = || super::request::reader::RequestReaderEvent::ReadError {
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
        let _sent = super::request::reader::send_reader_event(&sender, &thread_stop, event());
    });
    let guard = super::request::reader::ReaderGuard {
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
        super::request::reader::read_request_messages(
            std::io::BufReader::new(pipe),
            sender,
            MAX_FRAME_BYTES,
            WorkerGeneration::default(),
            None,
        );
    });
    let guard = super::request::reader::ReaderGuard {
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
    let tracker = crate::process::LeanWorkerProcessTracker::new();
    let child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn retained child");
    let mut worker = sleeping_worker();
    worker
        .shutdown_child(super::shutdown::ShutdownIntent::KillOnly)
        .expect("stop initial child");
    worker.pending_startup = Some(Box::new(super::StartupChild(Some(tracker.own(child)))));
    let failure = worker.shutdown_child_with_cleanup(super::shutdown::ShutdownIntent::KillOnly, |_| {
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
        .shutdown_child(super::shutdown::ShutdownIntent::KillOnly)
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
fn sleeping_worker() -> super::LeanWorker {
    let child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("owned old test child");
    super::LeanWorker {
        config: dummy_config(),
        child: Some(crate::process::LeanWorkerProcessTracker::new().own(child)),
        stdin: None,
        request_started: None,
        stdout: None,
        stderr: None,
        last_exit: None,
        request_scope: std::sync::Arc::new(()),
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
        recovery: None,
        stopped: false,
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
                Err(super::startup::SpawnFailure::after_spawn(
                    super::StartupChild(Some(crate::process::LeanWorkerProcessTracker::new().own(replacement))),
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
    let failed_shutdown = worker.shutdown_child_with_cleanup(super::shutdown::ShutdownIntent::Graceful, |_child| {
        Err(LeanWorkerError::Wait {
            source: std::io::Error::other("injected persistent cleanup uncertainty"),
        })
    });
    assert!(matches!(failed_shutdown, Err(LeanWorkerError::Wait { .. })));
    assert!(worker.pending_startup.is_some());
    assert!(worker.status().is_err());
    assert!(child_exists(replacement_pid.get()));
    assert_eq!(worker.confirmed_request_stop_since(&checkpoint), Some(&receipt));
    // No request uses an unconfirmed replacement. The next request first
    // kills and reaps it, then starts a new child, which fails here because
    // the test configuration names no real executable.
    assert!(matches!(worker.health(), Err(LeanWorkerError::Spawn { .. })));
    assert!(!child_exists(replacement_pid.get()));
    assert!(worker.pending_startup.is_none());
    assert_eq!(
        worker.confirmed_request_stop_since(&checkpoint),
        Some(&receipt),
        "startup cleanup must not replace the original request's process identity"
    );
    assert_eq!(worker.stats.exits, 2);
    assert_eq!(worker.stats.abnormal_restarts, 2);
    assert!(matches!(
        worker.stats.last_restart_reason,
        Some(LeanWorkerRestartReason::StartFailed { .. })
    ));
    let repeated = worker
        .shutdown_child(super::shutdown::ShutdownIntent::Graceful)
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
                    return Err(super::startup::SpawnFailure::no_child(LeanWorkerError::Spawn {
                        executable: PathBuf::from("missing"),
                        source: std::io::Error::from(std::io::ErrorKind::NotFound),
                    }));
                }
                let replacement = std::process::Command::new("sleep")
                    .arg("60")
                    .spawn()
                    .expect("replacement test child");
                replacement_pid.set(replacement.id());
                Err(super::startup::SpawnFailure::after_spawn(
                    super::StartupChild(Some(crate::process::LeanWorkerProcessTracker::new().own(replacement))),
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
                .shutdown_child(super::shutdown::ShutdownIntent::Graceful)
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
    let mut other = super::StartupChild(Some(crate::process::LeanWorkerProcessTracker::new().own(other)));
    let failed_setup = (|| -> Result<(), LeanWorkerError> {
        let mut owned = super::StartupChild(Some(crate::process::LeanWorkerProcessTracker::new().own(child)));
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
    use super::resources::child_cpu_millis;
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
    use super::resources::child_cpu_millis;
    let sample = child_cpu_millis(std::process::id());
    assert!(sample.is_some(), "proc_pid_rusage must sample the current process");
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn ps_cputime_parses_every_shape() {
    use super::resources::parse_ps_cputime;
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
        abnormal_restarts: 3,
        last_rss_kib: Some(42),
        rss_samples_unavailable: 1,
        ..LeanWorkerStats::default()
    };
    let exit = exit_with("bye", false);
    let wait = Duration::from_secs(30);
    let snapshot = LeanWorkerLifecycleSnapshot::from_worker(&stats, Some(exit.clone()), 0, None, 2, Some(wait));
    assert_eq!(snapshot.worker_generation, 3);
    assert_eq!(snapshot.restarts, 3);
    assert_eq!(snapshot.abnormal_restarts, 3);
    assert_eq!(snapshot.abnormal_restarts_in_window, 2);
    assert_eq!(snapshot.restart_retry_after, Some(wait));
    assert_eq!(snapshot.exits, 2);
    assert_eq!(snapshot.last_exit, Some(exit));
    assert_eq!(snapshot.last_rss_kib, Some(42));
    assert_eq!(snapshot.rss_samples_unavailable, 1);
}

/// Every reason names its cause, says whether the policy planned it, and
/// renders on one line. Only the planned ones may skip the restart window.
#[test]
fn restart_reasons_name_their_cause_and_kind() {
    let import_stats = None;
    let cases = [
        (LeanWorkerRestartReason::Explicit, "explicit", true, "explicit"),
        (
            LeanWorkerRestartReason::MaxRequests { limit: 1 },
            "max_requests",
            true,
            "max_requests limit=1",
        ),
        (
            LeanWorkerRestartReason::MaxImports { limit: 2 },
            "max_imports",
            true,
            "max_imports limit=2",
        ),
        (
            LeanWorkerRestartReason::ImportResidue {
                residue_bytes: 3 * 1024 * 1024,
                limit_bytes: 2 * 1024 * 1024,
            },
            "import_residue",
            true,
            "import_residue residue_mib=3 limit_mib=2",
        ),
        (
            LeanWorkerRestartReason::ImportAccountingUnknown,
            "import_accounting_unknown",
            true,
            "import_accounting_unknown: retained memory could not be measured after an import error",
        ),
        (
            LeanWorkerRestartReason::RssCeiling {
                current_kib: 2,
                limit_kib: 1,
                last_import_stats: import_stats.clone(),
            },
            "rss_ceiling",
            true,
            "rss_ceiling current_kib=2 limit_kib=1",
        ),
        (
            LeanWorkerRestartReason::Idle {
                idle_for: Duration::from_millis(5),
                limit: Duration::from_millis(4),
            },
            "idle",
            true,
            "idle idle_for_millis=5 limit_millis=4",
        ),
        (
            LeanWorkerRestartReason::RssHardLimit {
                operation: "test",
                current_kib: 2,
                limit_kib: 1,
                last_import_stats: import_stats,
            },
            "rss_hard_limit",
            false,
            "rss_hard_limit operation=test current_kib=2 limit_kib=1",
        ),
        (
            LeanWorkerRestartReason::Cancelled { operation: "test" },
            "cancelled",
            false,
            "cancelled operation=test",
        ),
        (
            LeanWorkerRestartReason::RequestTimeout {
                operation: "test",
                duration: Duration::from_millis(7),
            },
            "timeout",
            false,
            "timeout operation=test duration_millis=7",
        ),
        (
            LeanWorkerRestartReason::ChildAbort {
                operation: "test",
                status: String::from("signal: 6 (SIGABRT)"),
                diagnostics: String::from(" out of memory \n"),
            },
            "child_abort",
            false,
            "child_abort operation=test status=signal: 6 (SIGABRT): out of memory",
        ),
        (
            LeanWorkerRestartReason::ChildExit {
                operation: "test",
                status: String::from("exit status: 0"),
                diagnostics: String::new(),
            },
            "child_exit",
            false,
            "child_exit operation=test status=exit status: 0",
        ),
        (
            LeanWorkerRestartReason::SessionMissing { operation: "test" },
            "session_missing",
            false,
            "session_missing operation=test",
        ),
        (
            LeanWorkerRestartReason::RequestFailed {
                operation: "test",
                error: String::from("bad frame"),
            },
            "request_failed",
            false,
            "request_failed operation=test: bad frame",
        ),
        (
            LeanWorkerRestartReason::StartFailed {
                error: String::from("no handshake"),
            },
            "start_failed",
            false,
            "start_failed: no handshake",
        ),
    ];
    for (reason, cause, planned, display) in cases {
        assert_eq!(reason.stable_cause(), cause);
        assert_eq!(reason.is_planned(), planned, "{cause}");
        assert_eq!(reason.to_string(), display);
    }
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
    } = exit_reason("verify_declaration", &exit)
    else {
        panic!("a failed exit must produce ChildAbort");
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
    let LeanWorkerRestartReason::ChildAbort { diagnostics, .. } = exit_reason("health", &exit) else {
        panic!("a failed exit must produce ChildAbort");
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
    let resource = super::error::resource_facts(
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
        super::request::operations::degraded_query_batch_outcome(&selectors, resource.clone())
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
    let resource = super::error::resource_facts(
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
        super::request::operations::degraded_query_batch_outcome(&selectors, resource.clone())
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
    let resource = super::error::resource_facts(
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
        super::request::operations::degraded_query_batch_outcome(&selectors, resource.clone())
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
        super::request::operations::degraded_declaration_verification_batch_result(&request)
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
