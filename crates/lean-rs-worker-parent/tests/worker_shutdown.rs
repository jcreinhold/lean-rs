#![allow(
    clippy::exit,
    clippy::expect_used,
    clippy::panic,
    clippy::unnecessary_wraps,
    clippy::wildcard_enum_match_arm
)]

use std::env;
use std::io::{self, Write as _};
use std::thread;
use std::time::Duration;

#[cfg(all(unix, not(target_os = "linux")))]
use std::process::Command;

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerRestartPolicy, LeanWorkerShutdownOutcome,
};
use lean_rs_worker_protocol::protocol::{
    MAX_FRAME_BYTES, Message, PROTOCOL_VERSION, Request, Response, read_frame, write_frame,
};

const FAKE_CHILD_ENV: &str = "LEAN_RS_WORKER_PARENT_FAKE_CHILD";

fn main() {
    if let Ok(mode) = env::var(FAKE_CHILD_ENV) {
        run_fake_child(&mode);
        return;
    }

    let tests: &[(&str, fn() -> Result<(), String>)] = &[
        ("explicit_shutdown_reaps_child", explicit_shutdown_reaps_child),
        ("dropped_idle_worker_reaps_child", dropped_idle_worker_reaps_child),
        (
            "dropped_worker_escalates_when_terminate_hangs",
            dropped_worker_escalates_when_terminate_hangs,
        ),
        (
            "request_timeout_kills_reaps_and_replaces_wedged_child",
            request_timeout_kills_reaps_and_replaces_wedged_child,
        ),
        (
            "child_crash_reaps_and_returns_terminal_error",
            child_crash_reaps_and_returns_terminal_error,
        ),
        (
            "restart_limit_exhaustion_stops_accepting_work",
            restart_limit_exhaustion_stops_accepting_work,
        ),
    ];

    for (name, test) in tests {
        test().unwrap_or_else(|err| panic!("{name}: {err}"));
    }
}

fn run_fake_child(mode: &str) {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    write_frame(
        &mut stdout,
        Message::Handshake {
            worker_version: "fake-worker-shutdown-test".to_owned(),
            protocol_version: PROTOCOL_VERSION,
        },
        MAX_FRAME_BYTES,
    )
    .expect("fake child writes handshake");

    let frame_limit = {
        let mut stdin = io::stdin().lock();
        read_frame(&mut stdin, MAX_FRAME_BYTES).expect("fake child reads frame-limit")
    };
    match frame_limit.message {
        Message::ConfigureFrameLimit { .. } => {}
        other => panic!("expected frame-limit configuration, got {other:?}"),
    }

    loop {
        let frame = {
            let mut stdin = io::stdin().lock();
            let Ok(frame) = read_frame(&mut stdin, MAX_FRAME_BYTES) else {
                return;
            };
            frame
        };
        let Message::Request(request) = frame.message else {
            continue;
        };
        match request {
            Request::Health if mode == "request_hang" => sleep_forever(),
            Request::Health if mode == "crash_on_health" => std::process::exit(42),
            Request::Health => {
                write_frame(&mut stdout, Message::Response(Response::HealthOk), MAX_FRAME_BYTES)
                    .expect("fake child writes health response");
            }
            Request::Terminate if mode == "terminate_hang" => sleep_forever(),
            Request::Terminate => {
                write_frame(&mut stdout, Message::Response(Response::Terminating), MAX_FRAME_BYTES)
                    .expect("fake child writes terminating response");
                stdout.flush().expect("fake child flushes terminating response");
                return;
            }
            other => {
                write_frame(
                    &mut stdout,
                    Message::Response(Response::Error {
                        code: "fake.unsupported".to_owned(),
                        message: format!("unsupported fake request: {other:?}"),
                    }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes unsupported response");
            }
        }
    }
}

fn sleep_forever() -> ! {
    loop {
        thread::sleep(Duration::from_mins(1));
    }
}

fn fake_worker(mode: &str) -> Result<LeanWorker, LeanWorkerError> {
    fake_worker_with_config(mode, |config| config)
}

fn fake_worker_with_config(
    mode: &str,
    configure: impl FnOnce(LeanWorkerConfig) -> LeanWorkerConfig,
) -> Result<LeanWorker, LeanWorkerError> {
    let executable = env::current_exe().map_err(|source| LeanWorkerError::Spawn {
        executable: "<current test executable>".into(),
        source,
    })?;
    let config = LeanWorkerConfig::new(executable)
        .env(FAKE_CHILD_ENV, mode)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_millis(80))
        .shutdown_timeout(Duration::from_millis(80));
    LeanWorker::spawn(&configure(config))
}

fn restart_limited_fake_worker(mode: &str, max_restarts: u64) -> Result<LeanWorker, LeanWorkerError> {
    fake_worker_with_config(mode, |config| {
        config.restart_policy(
            LeanWorkerRestartPolicy::default().max_restarts_per_window(max_restarts, Duration::from_mins(1)),
        )
    })
}

fn explicit_shutdown_reaps_child() -> Result<(), String> {
    let worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    let report = worker.shutdown().map_err(|err| err.to_string())?;
    assert_eq!(report.outcome, LeanWorkerShutdownOutcome::Graceful);
    assert!(report.exit.success, "fake child should exit cleanly");
    assert_reaped(pid)
}

fn dropped_idle_worker_reaps_child() -> Result<(), String> {
    let worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    drop(worker);
    assert_reaped(pid)
}

fn dropped_worker_escalates_when_terminate_hangs() -> Result<(), String> {
    let worker = fake_worker("terminate_hang").map_err(|err| err.to_string())?;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    drop(worker);
    assert_reaped(pid)
}

fn request_timeout_kills_reaps_and_replaces_wedged_child() -> Result<(), String> {
    let mut worker = fake_worker("request_hang").map_err(|err| err.to_string())?;
    let old_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose an initial child pid".to_owned())?;
    let err = worker.health().expect_err("wedged health request should time out");
    match err {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "health"),
        other => return Err(format!("expected timeout error, got {other:?}")),
    }
    assert_reaped(old_pid)?;
    let new_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not spawn a replacement child".to_owned())?;
    assert_ne!(old_pid, new_pid, "timeout should replace the wedged child");
    let stats = worker.stats();
    assert_eq!(stats.requests, 1, "accepted request should be recorded once");
    assert_eq!(stats.timeout_restarts, 1, "timeout should be terminalized once");
    drop(worker);
    assert_reaped(new_pid)
}

fn child_crash_reaps_and_returns_terminal_error() -> Result<(), String> {
    let mut worker = fake_worker("crash_on_health").map_err(|err| err.to_string())?;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    let err = worker.health().expect_err("fake child crash should fail request");
    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => assert!(!exit.success),
        other => return Err(format!("expected child fatal exit, got {other:?}")),
    }
    assert_reaped(pid)
}

fn restart_limit_exhaustion_stops_accepting_work() -> Result<(), String> {
    let mut worker = restart_limited_fake_worker("request_hang", 1).map_err(|err| err.to_string())?;
    let first_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose an initial child pid".to_owned())?;
    let err = worker
        .health()
        .expect_err("first wedged health request should time out");
    match err {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "health"),
        other => return Err(format!("expected first timeout error, got {other:?}")),
    }
    assert_reaped(first_pid)?;

    let second_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose replacement child pid".to_owned())?;
    let err = worker
        .health()
        .expect_err("second wedged health request should exhaust restart limit");
    match err {
        LeanWorkerError::RestartLimitExceeded { restarts, window } => {
            assert_eq!(restarts, 1);
            assert_eq!(window, Duration::from_mins(1));
        }
        other => return Err(format!("expected restart-limit error, got {other:?}")),
    }
    assert_reaped(second_pid)?;

    let err = worker
        .health()
        .expect_err("restart-exhausted worker should stop accepting work");
    match err {
        LeanWorkerError::ShutdownInProgress { operation } => assert_eq!(operation, "worker_request"),
        other => return Err(format!("expected shutdown-in-progress after exhaustion, got {other:?}")),
    }

    let stats = worker.stats();
    assert_eq!(
        stats.restarts, 1,
        "only the admitted replacement should bump generation"
    );
    assert_eq!(
        stats.timeout_restarts, 1,
        "only the admitted timeout replacement is counted"
    );
    assert_eq!(
        stats.replacement_attempts, 2,
        "both timeout replacements were attempted"
    );
    assert_eq!(stats.replacement_successes, 1);
    assert_eq!(stats.replacement_failures, 1);
    assert_eq!(stats.replacement_budget_admitted, 1);
    assert_eq!(stats.replacement_budget_skipped, 1);
    assert_eq!(
        stats.last_replacement_skipped_reason.as_deref(),
        Some("restart_limit_exceeded")
    );
    Ok(())
}

fn assert_reaped(pid: u32) -> Result<(), String> {
    for _ in 0..100 {
        match process_state(pid)? {
            ProcessState::Missing => return Ok(()),
            ProcessState::Zombie => return Err(format!("child process {pid} is a zombie")),
            ProcessState::Alive => thread::sleep(Duration::from_millis(10)),
            #[cfg(not(unix))]
            ProcessState::Unknown => return Ok(()),
        }
    }
    Err(format!("child process {pid} still exists after cleanup"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessState {
    Alive,
    Missing,
    Zombie,
    #[cfg(not(unix))]
    Unknown,
}

#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Result<ProcessState, String> {
    match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(status) => {
            if status
                .lines()
                .any(|line| line.starts_with("State:") && line.contains('Z'))
            {
                Ok(ProcessState::Zombie)
            } else {
                Ok(ProcessState::Alive)
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(ProcessState::Missing),
        Err(err) => Err(format!("failed to inspect /proc/{pid}/status: {err}")),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_state(pid: u32) -> Result<ProcessState, String> {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map_err(|err| format!("failed to run ps for pid {pid}: {err}"))?;
    if !output.status.success() {
        return Ok(ProcessState::Missing);
    }
    let stat = String::from_utf8_lossy(&output.stdout);
    if stat.trim().is_empty() {
        Ok(ProcessState::Missing)
    } else if stat.contains('Z') {
        Ok(ProcessState::Zombie)
    } else {
        Ok(ProcessState::Alive)
    }
}

#[cfg(not(unix))]
fn process_state(_pid: u32) -> Result<ProcessState, String> {
    Ok(ProcessState::Unknown)
}
