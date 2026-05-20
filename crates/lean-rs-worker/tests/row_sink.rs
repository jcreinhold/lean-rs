#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::PathBuf;
use std::sync::Mutex;

use lean_rs_worker::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerConfig, LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerError,
    LeanWorkerRestartReason,
};
use serde_json::json;

fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
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

struct PanicDataSink;

impl LeanWorkerDataSink for PanicDataSink {
    fn report(&self, _row: LeanWorkerDataRow) {
        panic!("row sink boom");
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
fn row_frames_are_delivered_to_sink_in_order() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let sink = RecordingDataSink::default();

    let count = worker
        .__emit_test_rows(
            vec![
                "rows".to_owned(),
                "warnings".to_owned(),
                "rows".to_owned(),
                "warnings".to_owned(),
            ],
            None,
            Some(&sink),
        )
        .expect("row request succeeds");

    assert_eq!(count, 4);
    assert_eq!(
        sink.rows(),
        vec![
            LeanWorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 0,
                payload: json!({"stream": "rows", "index": 0}),
            },
            LeanWorkerDataRow {
                stream: "warnings".to_owned(),
                sequence: 0,
                payload: json!({"stream": "warnings", "index": 1}),
            },
            LeanWorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 1,
                payload: json!({"stream": "rows", "index": 2}),
            },
            LeanWorkerDataRow {
                stream: "warnings".to_owned(),
                sequence: 1,
                payload: json!({"stream": "warnings", "index": 3}),
            },
        ],
    );
}

#[test]
fn sink_panic_maps_to_data_sink_panic() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    let err = worker
        .__emit_test_rows(vec!["rows".to_owned()], None, Some(&PanicDataSink))
        .expect_err("sink panic should be reported as a worker error");

    match err {
        LeanWorkerError::DataSinkPanic { message } => {
            assert!(
                message.contains("row sink boom"),
                "panic message should be preserved, got {message}",
            );
        }
        other => panic!("expected data sink panic, got {other:?}"),
    }
}

#[test]
fn sink_requested_cancellation_cycles_child() {
    let token = LeanWorkerCancellationToken::new();
    let sink = CancelOnFirstRow {
        token: &token,
        rows: Mutex::new(Vec::new()),
    };
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    let err = worker
        .__emit_test_rows(vec!["rows".to_owned(), "rows".to_owned()], Some(&token), Some(&sink))
        .expect_err("sink cancellation should stop the worker request");

    match err {
        LeanWorkerError::Cancelled { operation } => assert_eq!(operation, "emit_test_rows"),
        other => panic!("expected cancellation, got {other:?}"),
    }
    assert_eq!(
        sink.rows(),
        vec![LeanWorkerDataRow {
            stream: "rows".to_owned(),
            sequence: 0,
            payload: json!({"stream": "rows", "index": 0}),
        }],
    );
    let stats = worker.stats();
    assert_eq!(stats.cancelled_restarts, 1);
    assert_eq!(
        stats.last_restart_reason,
        Some(LeanWorkerRestartReason::Cancelled {
            operation: "emit_test_rows",
        }),
    );
    worker
        .health()
        .expect("worker remains usable after cancellation restart");
}

#[test]
fn data_row_without_sink_is_protocol_error() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    let err = worker
        .__emit_test_rows(vec!["rows".to_owned()], None, None)
        .expect_err("row frames without a sink should be rejected");

    match err {
        LeanWorkerError::Protocol { message } => {
            assert!(
                message.contains("without a row sink"),
                "protocol message should explain missing row sink, got {message}",
            );
        }
        other => panic!("expected protocol error, got {other:?}"),
    }
}
