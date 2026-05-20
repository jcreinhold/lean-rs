#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lean_rs_worker::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerConfig, LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerError,
    LeanWorkerRestartReason, LeanWorkerSessionConfig,
};
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

fn interop_root() -> PathBuf {
    workspace_root().join("fixtures").join("interop-shims")
}

fn ensure_interop_built() {
    let fixture = interop_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsInteropConsumer")
        .expect("interop consumer Lake target builds");
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
}

fn stream_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
}

#[derive(Default)]
struct RecordingDataSink {
    rows: Mutex<Vec<LeanWorkerDataRow>>,
}

impl RecordingDataSink {
    fn rows(&self) -> Vec<LeanWorkerDataRow> {
        self.rows.lock().expect("row lock is not poisoned").clone()
    }
}

impl LeanWorkerDataSink for RecordingDataSink {
    fn report(&self, row: LeanWorkerDataRow) {
        self.rows.lock().expect("row lock is not poisoned").push(row);
    }
}

struct CancelOnFirstRow<'a> {
    token: &'a LeanWorkerCancellationToken,
    rows: Mutex<Vec<LeanWorkerDataRow>>,
}

impl CancelOnFirstRow<'_> {
    fn rows(&self) -> Vec<LeanWorkerDataRow> {
        self.rows.lock().expect("row lock is not poisoned").clone()
    }
}

impl LeanWorkerDataSink for CancelOnFirstRow<'_> {
    fn report(&self, row: LeanWorkerDataRow) {
        self.rows.lock().expect("row lock is not poisoned").push(row);
        self.token.cancel();
    }
}

#[test]
fn successful_stream_delivers_rows_with_per_stream_sequences() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let summary = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream",
            &json!({"request": "demo"}),
            &sink,
            None,
            None,
        )
        .expect("streaming export succeeds");

    assert_eq!(summary.rows, 3);
    assert_eq!(
        sink.rows(),
        vec![
            LeanWorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 0,
                payload: json!({"kind": "request", "ordinal": 0}),
            },
            LeanWorkerDataRow {
                stream: "diagnostics".to_owned(),
                sequence: 0,
                payload: json!({"severity": "info", "message": "started"}),
            },
            LeanWorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 1,
                payload: json!({"kind": "done", "ordinal": 1}),
            },
        ],
    );
}

#[test]
fn malformed_row_json_is_typed() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream_malformed_json",
            &json!({}),
            &sink,
            None,
            None,
        )
        .expect_err("malformed row JSON should be typed");

    match err {
        LeanWorkerError::StreamRowMalformed { message } => {
            assert!(message.contains("not valid JSON"), "unexpected message: {message}");
        }
        other => panic!("expected malformed row error, got {other:?}"),
    }
    assert!(sink.rows().is_empty(), "malformed row should not be delivered");
}

#[test]
fn missing_stream_or_payload_is_typed() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream_missing_stream",
            &json!({}),
            &sink,
            None,
            None,
        )
        .expect_err("missing stream should be typed");
    match err {
        LeanWorkerError::StreamRowMalformed { message } => {
            assert!(message.contains("`stream`"), "unexpected message: {message}");
        }
        other => panic!("expected malformed row error, got {other:?}"),
    }

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream_missing_payload",
            &json!({}),
            &sink,
            None,
            None,
        )
        .expect_err("missing payload should be typed");
    match err {
        LeanWorkerError::StreamRowMalformed { message } => {
            assert!(message.contains("`payload`"), "unexpected message: {message}");
        }
        other => panic!("expected malformed row error, got {other:?}"),
    }
    assert!(sink.rows().is_empty(), "invalid rows should not be delivered");
}

#[test]
fn nonzero_export_status_is_typed() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream_status",
            &json!({}),
            &sink,
            None,
            None,
        )
        .expect_err("nonzero export status should be typed");

    match err {
        LeanWorkerError::StreamExportFailed { status } => assert_eq!(status, 7),
        other => panic!("expected stream export status error, got {other:?}"),
    }
}

#[test]
fn callback_status_error_is_typed() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream_wrong_callback",
            &json!({}),
            &sink,
            None,
            None,
        )
        .expect_err("wrong callback payload should be typed");

    match err {
        LeanWorkerError::StreamCallbackFailed { status, description } => {
            assert_eq!(status, 3);
            assert!(
                description.contains("wrong payload"),
                "unexpected description: {description}"
            );
        }
        other => panic!("expected callback status error, got {other:?}"),
    }
}

#[test]
fn child_fatal_exit_is_reported_to_parent() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream_panic",
            &json!({}),
            &sink,
            None,
            None,
        )
        .expect_err("Lean panic should kill only the child");

    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => {
            assert!(!exit.success, "panic export should terminate the child");
        }
        other => panic!("expected fatal child exit, got {other:?}"),
    }
}

#[test]
fn row_sink_cancellation_cycles_child_and_invalidates_session() {
    ensure_interop_built();
    let token = LeanWorkerCancellationToken::new();
    let sink = CancelOnFirstRow {
        token: &token,
        rows: Mutex::new(Vec::new()),
    };
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    {
        let mut session = worker
            .open_session(&stream_session_config(), None, None)
            .expect("worker session opens");

        let err = session
            .run_data_stream(
                "lean_rs_interop_consumer_worker_data_stream",
                &json!({}),
                &sink,
                Some(&token),
                None,
            )
            .expect_err("sink cancellation should stop the stream request");

        match err {
            LeanWorkerError::Cancelled { operation } => assert_eq!(operation, "worker_run_data_stream"),
            other => panic!("expected cancellation, got {other:?}"),
        }
        assert_eq!(
            sink.rows(),
            vec![LeanWorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 0,
                payload: json!({"kind": "request", "ordinal": 0}),
            }],
        );

        let err = session
            .declaration_names(&["LeanRsInteropConsumer.Callback.add"], None, None)
            .expect_err("cancelled worker session should be invalidated");
        match err {
            LeanWorkerError::UnsupportedRequest { operation } => {
                assert_eq!(operation, "worker_session_after_cancel");
            }
            other => panic!("expected invalidated session error, got {other:?}"),
        }
    }

    let stats = worker.stats();
    assert_eq!(stats.cancelled_restarts, 1);
    assert_eq!(
        stats.last_restart_reason,
        Some(LeanWorkerRestartReason::Cancelled {
            operation: "worker_run_data_stream",
        }),
    );
    worker
        .health()
        .expect("worker remains usable after cancellation restart");
}
