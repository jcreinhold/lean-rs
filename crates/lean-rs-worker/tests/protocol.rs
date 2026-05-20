#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};

use lean_rs_worker::__test_support::{WorkerDataRow, WorkerHarnessError, WorkerProcess};
use serde_json::json;

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

#[test]
fn health_check_succeeds() {
    let mut worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    worker.health().expect("health check succeeds");
    let status = worker.terminate().expect("worker terminates");
    assert!(status.success(), "worker should exit cleanly");
}

#[test]
fn fixture_capability_loads_and_exported_call_succeeds() {
    ensure_fixture_built();
    let fixture = fixture_root();
    let mut worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    worker
        .load_fixture_capability(&fixture)
        .expect("fixture capability loads in worker");
    let value = worker
        .call_fixture_mul(&fixture, 6, 7)
        .expect("worker calls fixture export");
    assert_eq!(value, 42);
    let status = worker.terminate().expect("worker terminates");
    assert!(status.success(), "worker should exit cleanly");
}

#[test]
fn terminate_request_exits_cleanly() {
    let worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let status = worker.terminate().expect("worker terminates");
    assert!(status.success(), "worker should exit cleanly");
}

#[test]
fn lean_internal_panic_kills_only_child() {
    ensure_fixture_built();
    let fixture = fixture_root();
    let worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let fatal = worker
        .trigger_lean_panic(&fixture)
        .expect("parent observes child fatal exit");
    assert!(
        !fatal.status.is_empty(),
        "fatal exit should include rendered child status"
    );
    if !fatal.stderr.is_empty() {
        assert!(
            fatal.stderr.contains("lean_rs_fixture: deliberate Lean panic"),
            "child stderr should contain Lean panic message, got:\n{}",
            fatal.stderr,
        );
    }
}

#[test]
fn missing_fixture_path_reports_worker_error_without_crashing_child() {
    let missing = workspace_root()
        .join("fixtures")
        .join("definitely-missing-worker-fixture");
    let mut worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let err = worker
        .load_fixture_capability(&missing)
        .expect_err("missing fixture path should be a typed worker error");
    match err {
        WorkerHarnessError::WorkerError { code, message } => {
            assert_eq!(code, "lean_rs.module_init");
            assert!(
                message.contains("definitely-missing-worker-fixture"),
                "message should identify missing fixture path, got {message}",
            );
        }
        other => panic!("expected WorkerError, got {other:?}"),
    }
    let status = worker.terminate().expect("worker terminates after typed error");
    assert!(status.success(), "worker should stay alive after typed load error");
}

#[test]
fn data_rows_are_delivered_in_pipe_order_with_per_stream_sequences() {
    let mut worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let rows = worker
        .emit_test_rows(vec![
            "rows".to_owned(),
            "warnings".to_owned(),
            "rows".to_owned(),
            "warnings".to_owned(),
        ])
        .expect("worker emits data rows");

    assert_eq!(
        rows,
        vec![
            WorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 0,
                payload: json!({ "stream": "rows", "index": 0 }),
            },
            WorkerDataRow {
                stream: "warnings".to_owned(),
                sequence: 0,
                payload: json!({ "stream": "warnings", "index": 1 }),
            },
            WorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 1,
                payload: json!({ "stream": "rows", "index": 2 }),
            },
            WorkerDataRow {
                stream: "warnings".to_owned(),
                sequence: 1,
                payload: json!({ "stream": "warnings", "index": 3 }),
            },
        ],
    );

    let status = worker.terminate().expect("worker terminates after row stream");
    assert!(status.success(), "worker should exit cleanly");
}

#[test]
fn eof_before_rows_complete_is_reported_as_protocol_failure() {
    let worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let err = worker
        .emit_rows_then_exit()
        .expect_err("child exit before terminal response should fail");
    match err {
        WorkerHarnessError::Protocol(message) => {
            assert!(
                message.contains("before terminal row response"),
                "failure should name missing terminal response, got {message}",
            );
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }
}

#[test]
fn fatal_exit_after_partial_rows_is_reported_as_worker_failure() {
    let worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let err = worker
        .emit_rows_then_panic()
        .expect_err("fatal exit before terminal response should fail");
    match err {
        WorkerHarnessError::FatalExit(exit) => {
            assert!(
                !exit.status.is_empty(),
                "fatal exit should include rendered child status"
            );
        }
        other => panic!("expected FatalExit, got {other:?}"),
    }
}
