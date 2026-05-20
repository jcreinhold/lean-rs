#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use lean_rs_worker::{LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerStatus};

fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    workspace_root().join("fixtures").join("lean")
}

fn ensure_fixture_built() {
    let fixture = fixture_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsFixture").expect("fixture Lake target builds");
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
}

#[test]
fn public_health_check_succeeds() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker.health().expect("health check succeeds");
    assert_eq!(worker.status().expect("status succeeds"), LeanWorkerStatus::Running);
    let exit = worker.terminate().expect("worker terminates");
    assert!(exit.success, "worker should exit cleanly");
}

#[test]
fn public_fixture_export_call_succeeds() {
    ensure_fixture_built();
    let fixture = fixture_root();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker
        .load_fixture_capability(&fixture)
        .expect("fixture capability loads in worker");
    let value = worker
        .call_fixture_mul(&fixture, 9, 8)
        .expect("worker calls fixture export");
    assert_eq!(value, 72);
    let exit = worker.terminate().expect("worker terminates");
    assert!(exit.success, "worker should exit cleanly");
}

#[test]
fn explicit_restart_replaces_live_child() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker.health().expect("health check succeeds before restart");
    worker.restart().expect("worker restarts");
    worker.health().expect("health check succeeds after restart");
    let exit = worker.terminate().expect("worker terminates");
    assert!(exit.success, "worker should exit cleanly after restart");
}

#[test]
fn dead_worker_use_is_typed() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker.health().expect("health check succeeds");
    worker.__kill_for_test().expect("worker kill succeeds");
    let err = worker.health().expect_err("dead worker use should fail");
    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => {
            assert!(!exit.success, "killed child should be a fatal worker exit");
        }
        other => panic!("expected child panic/abort error, got {other:?}"),
    }
}

#[test]
fn child_crash_is_typed_and_parent_survives() {
    ensure_fixture_built();
    let fixture = fixture_root();
    let worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let exit = worker
        .__trigger_lean_panic_fixture(&fixture)
        .expect("parent observes child fatal exit");
    assert!(!exit.success, "panic fixture should terminate the child");
    if !exit.diagnostics.is_empty() {
        assert!(
            exit.diagnostics.contains("lean_rs_fixture: deliberate Lean panic"),
            "child diagnostics should contain Lean panic message, got:\n{}",
            exit.diagnostics,
        );
    }
}

#[test]
fn missing_fixture_path_maps_to_worker_error() {
    let missing = workspace_root()
        .join("fixtures")
        .join("definitely-missing-worker-fixture");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let err = worker
        .load_fixture_capability(&missing)
        .expect_err("missing fixture path should be a typed worker error");
    match err {
        LeanWorkerError::Worker { code, message } => {
            assert_eq!(code, "lean_rs.module_init");
            assert!(
                message.contains("definitely-missing-worker-fixture"),
                "message should identify missing fixture path, got {message}",
            );
        }
        other => panic!("expected worker error, got {other:?}"),
    }
    let exit = worker.terminate().expect("worker terminates after typed error");
    assert!(exit.success, "worker should stay alive after typed load error");
}

#[test]
fn startup_failure_is_typed() {
    let missing = workspace_root().join("target").join("definitely-missing-worker-child");
    let err = LeanWorker::spawn(&LeanWorkerConfig::new(&missing)).expect_err("spawn should fail");
    match err {
        LeanWorkerError::Spawn { executable, .. } => assert_eq!(executable, missing),
        other => panic!("expected spawn error, got {other:?}"),
    }
}

#[test]
fn startup_timeout_is_typed() {
    let err = LeanWorker::spawn(
        &LeanWorkerConfig::new("/bin/cat")
            .env("unused", "unused")
            .startup_timeout(Duration::from_millis(20)),
    )
    .expect_err("non-worker child should not handshake");
    match err {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "startup"),
        other => panic!("expected startup timeout, got {other:?}"),
    }
}
