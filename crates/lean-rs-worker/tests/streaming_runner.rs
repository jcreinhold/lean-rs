#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use lean_rs_worker::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerConfig, LeanWorkerDataRow, LeanWorkerDataSink,
    LeanWorkerDiagnosticEvent, LeanWorkerDiagnosticSink, LeanWorkerError, LeanWorkerRestartReason,
    LeanWorkerSessionConfig,
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

#[derive(Default)]
struct RecordingDiagnosticSink {
    diagnostics: Mutex<Vec<LeanWorkerDiagnosticEvent>>,
}

impl RecordingDiagnosticSink {
    fn diagnostics(&self) -> Vec<LeanWorkerDiagnosticEvent> {
        self.diagnostics
            .lock()
            .expect("diagnostic lock is not poisoned")
            .clone()
    }
}

impl LeanWorkerDiagnosticSink for RecordingDiagnosticSink {
    fn report(&self, diagnostic: LeanWorkerDiagnosticEvent) {
        self.diagnostics
            .lock()
            .expect("diagnostic lock is not poisoned")
            .push(diagnostic);
    }
}

struct PanicDiagnosticSink;

impl LeanWorkerDiagnosticSink for PanicDiagnosticSink {
    fn report(&self, _diagnostic: LeanWorkerDiagnosticEvent) {
        panic!("diagnostic sink boom");
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
    let diagnostics = RecordingDiagnosticSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let summary = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream",
            &json!({"request": "demo"}),
            &sink,
            Some(&diagnostics),
            None,
            None,
        )
        .expect("streaming export succeeds");

    assert_eq!(summary.total_rows, 2);
    assert_eq!(summary.per_stream_counts, BTreeMap::from([("rows".to_owned(), 2)]));
    assert_eq!(
        summary.metadata,
        Some(json!({"fixture": "worker_data_stream", "ok": true}))
    );
    assert_eq!(
        sink.rows(),
        vec![
            LeanWorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 0,
                payload: json!({"kind": "request", "ordinal": 0}),
            },
            LeanWorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 1,
                payload: json!({"kind": "done", "ordinal": 1}),
            },
        ],
    );
    assert_eq!(
        diagnostics.diagnostics(),
        vec![
            LeanWorkerDiagnosticEvent {
                code: "lean_rs.worker.fixture.started".to_owned(),
                message: "started".to_owned(),
            },
            LeanWorkerDiagnosticEvent {
                code: "lean_rs.worker.fixture.finished".to_owned(),
                message: "finished".to_owned(),
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
                None,
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

#[test]
fn row_before_child_panic_is_delivered_before_terminal_failure() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream_row_then_panic",
            &json!({}),
            &sink,
            None,
            None,
            None,
        )
        .expect_err("fatal child exit should fail the stream");

    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => {
            assert!(!exit.success, "panic export should terminate the child");
        }
        other => panic!("expected fatal child exit, got {other:?}"),
    }
    assert_eq!(
        sink.rows(),
        vec![LeanWorkerDataRow {
            stream: "rows".to_owned(),
            sequence: 0,
            payload: json!({"kind": "before-panic"}),
        }],
    );
}

#[test]
fn diagnostic_sink_panic_is_typed() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&stream_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream",
            &json!({}),
            &sink,
            Some(&PanicDiagnosticSink),
            None,
            None,
        )
        .expect_err("diagnostic sink panic should be typed");

    match err {
        LeanWorkerError::DiagnosticSinkPanic { message } => {
            assert!(
                message.contains("diagnostic sink boom"),
                "panic message should be preserved, got {message}",
            );
        }
        other => panic!("expected diagnostic sink panic, got {other:?}"),
    }
}

#[test]
fn large_stream_records_live_forwarding_throughput_and_rss() {
    ensure_interop_built();
    let sink = RecordingDataSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let rss_before = worker.rss_kib();
    let started = Instant::now();
    let summary = {
        let mut session = worker
            .open_session(&stream_session_config(), None, None)
            .expect("worker session opens");
        session
            .run_data_stream(
                "lean_rs_interop_consumer_worker_data_stream_many",
                &json!({}),
                &sink,
                None,
                None,
                None,
            )
            .expect("large stream succeeds")
    };
    let elapsed = started.elapsed();

    assert_eq!(summary.total_rows, 512);
    assert_eq!(sink.rows().len(), 512);
    assert_eq!(summary.per_stream_counts, BTreeMap::from([("rows".to_owned(), 512)]));
    let rss_after = worker.rss_kib();
    println!(
        "large_stream rows=512 elapsed_ms={} rows_per_sec={:.1} rss_before_kib={:?} rss_after_kib={:?}",
        elapsed.as_millis(),
        512.0 / elapsed.as_secs_f64().max(0.001),
        rss_before,
        rss_after,
    );
}
