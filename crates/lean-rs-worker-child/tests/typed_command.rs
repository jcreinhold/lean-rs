#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use lean_rs_worker_parent::{
    LeanWorkerCapabilityBuilder, LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerDiagnosticEvent,
    LeanWorkerDiagnosticSink, LeanWorkerError, LeanWorkerJsonCommand, LeanWorkerProgressEvent, LeanWorkerProgressSink,
    LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};
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

fn builder() -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .json_command_export("lean_rs_interop_consumer_worker_json_command")
    .json_command_export("lean_rs_interop_consumer_worker_json_command_malformed")
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream")
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_chunked")
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_chunked_completion")
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_chunk_error")
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_large_payload")
    .worker_executable(worker_binary())
}

#[derive(Debug, Serialize)]
struct FixtureRequest {
    source: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct FixtureResponse {
    accepted: bool,
    kind: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct FixtureRow {
    kind: String,
    ordinal: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct LargeFixtureRow {
    kind: String,
    blob: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct FixtureSummary {
    fixture: String,
    ok: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
struct WrongRow {
    missing: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WrongSummary {
    missing: String,
}

struct RecordingTypedSink<Row> {
    rows: Mutex<Vec<LeanWorkerTypedDataRow<Row>>>,
}

impl<Row> Default for RecordingTypedSink<Row> {
    fn default() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
        }
    }
}

impl<Row> RecordingTypedSink<Row>
where
    Row: Clone,
{
    fn rows(&self) -> Vec<LeanWorkerTypedDataRow<Row>> {
        self.rows.lock().expect("row lock is not poisoned").clone()
    }
}

impl<Row> LeanWorkerTypedDataSink<Row> for RecordingTypedSink<Row>
where
    Row: Clone + Send + Sync,
{
    fn report(&self, row: LeanWorkerTypedDataRow<Row>) {
        self.rows.lock().expect("row lock is not poisoned").push(row);
    }
}

#[derive(Default)]
struct RecordingRawSink {
    rows: Mutex<Vec<LeanWorkerDataRow>>,
}

impl RecordingRawSink {
    fn rows(&self) -> Vec<LeanWorkerDataRow> {
        self.rows.lock().expect("row lock is not poisoned").clone()
    }
}

impl LeanWorkerDataSink for RecordingRawSink {
    fn report(&self, row: LeanWorkerDataRow) {
        self.rows.lock().expect("row lock is not poisoned").push(row);
    }
}

#[derive(Default)]
struct RecordingDiagnostics {
    diagnostics: Mutex<Vec<LeanWorkerDiagnosticEvent>>,
}

impl RecordingDiagnostics {
    fn diagnostics(&self) -> Vec<LeanWorkerDiagnosticEvent> {
        self.diagnostics
            .lock()
            .expect("diagnostic lock is not poisoned")
            .clone()
    }
}

impl LeanWorkerDiagnosticSink for RecordingDiagnostics {
    fn report(&self, diagnostic: LeanWorkerDiagnosticEvent) {
        self.diagnostics
            .lock()
            .expect("diagnostic lock is not poisoned")
            .push(diagnostic);
    }
}

#[derive(Default)]
struct RecordingProgress {
    events: Mutex<Vec<LeanWorkerProgressEvent>>,
}

impl RecordingProgress {
    fn events(&self) -> Vec<LeanWorkerProgressEvent> {
        self.events.lock().expect("progress lock is not poisoned").clone()
    }
}

impl LeanWorkerProgressSink for RecordingProgress {
    fn report(&self, event: LeanWorkerProgressEvent) {
        self.events.lock().expect("progress lock is not poisoned").push(event);
    }
}

#[test]
fn typed_json_command_decodes_response() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command =
        LeanWorkerJsonCommand::<FixtureRequest, FixtureResponse>::new("lean_rs_interop_consumer_worker_json_command");

    let response = session
        .run_json_command(
            &command,
            &FixtureRequest {
                source: "typed-command-test".to_owned(),
            },
            None,
            None,
        )
        .expect("typed JSON command succeeds");

    assert_eq!(
        response,
        FixtureResponse {
            accepted: true,
            kind: "fixture".to_owned(),
        }
    );
}

#[test]
fn typed_json_command_decode_error_is_typed() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerJsonCommand::<FixtureRequest, FixtureResponse>::new(
        "lean_rs_interop_consumer_worker_json_command_malformed",
    );

    let err = session
        .run_json_command(
            &command,
            &FixtureRequest {
                source: "typed-command-test".to_owned(),
            },
            None,
            None,
        )
        .expect_err("malformed response should be typed");

    match err {
        LeanWorkerError::TypedCommandResponseDecode { export, message } => {
            assert_eq!(export, "lean_rs_interop_consumer_worker_json_command_malformed");
            assert!(!message.is_empty(), "serde decode message should be carried");
        }
        other => panic!("expected typed response decode error, got {other:?}"),
    }
}

#[test]
fn typed_streaming_command_decodes_rows_and_summary() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<FixtureRequest, FixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream",
    );
    let sink = RecordingTypedSink::<FixtureRow>::default();

    let summary = session
        .run_streaming_command(
            &command,
            &FixtureRequest {
                source: "typed-stream-test".to_owned(),
            },
            &sink,
            None,
            None,
            None,
        )
        .expect("typed streaming command succeeds");

    assert_eq!(summary.total_rows, 2);
    assert_eq!(summary.per_stream_counts.get("rows"), Some(&2));
    assert_eq!(
        summary.metadata,
        Some(FixtureSummary {
            fixture: "worker_data_stream".to_owned(),
            ok: true,
        }),
    );
    assert_eq!(
        sink.rows(),
        vec![
            LeanWorkerTypedDataRow {
                stream: "rows".to_owned(),
                sequence: 0,
                payload: FixtureRow {
                    kind: "request".to_owned(),
                    ordinal: 0,
                },
            },
            LeanWorkerTypedDataRow {
                stream: "rows".to_owned(),
                sequence: 1,
                payload: FixtureRow {
                    kind: "done".to_owned(),
                    ordinal: 1,
                },
            },
        ],
    );
}

#[test]
fn helper_chunked_stream_delivers_rows_diagnostics_progress_and_metadata() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<FixtureRequest, FixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream_chunked",
    );
    let sink = RecordingTypedSink::<FixtureRow>::default();
    let diagnostics = RecordingDiagnostics::default();
    let progress = RecordingProgress::default();
    let started = Instant::now();

    let summary = session
        .run_streaming_command(
            &command,
            &FixtureRequest {
                source: "chunked-helper-test".to_owned(),
            },
            &sink,
            Some(&diagnostics),
            None,
            Some(&progress),
        )
        .expect("helper chunked stream succeeds");
    let elapsed = started.elapsed();
    eprintln!(
        "helper_chunked_stream rows={} chunks=3 chunk_size=2 parallelism=1 elapsed_ms={:.2}",
        summary.total_rows,
        elapsed.as_secs_f64() * 1000.0
    );

    assert_eq!(summary.total_rows, 6);
    assert_eq!(summary.per_stream_counts.get("chunks"), Some(&6));
    assert_eq!(
        summary.metadata,
        Some(FixtureSummary {
            fixture: "worker_data_stream_chunks".to_owned(),
            ok: true,
        }),
    );
    assert_eq!(
        sink.rows()
            .iter()
            .map(|row| (row.sequence, row.payload.ordinal))
            .collect::<Vec<_>>(),
        vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5)],
    );
    assert_eq!(
        diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>(),
        vec![
            "lean_rs.worker.fixture.chunk.started",
            "lean_rs.worker.fixture.chunk.finished",
        ],
    );
    let chunk_progress = progress
        .events()
        .into_iter()
        .filter(|event| event.phase == "fixture.chunk")
        .map(|event| (event.current, event.total))
        .collect::<Vec<_>>();
    assert_eq!(chunk_progress, vec![(1, Some(3)), (2, Some(3)), (3, Some(3))]);
}

#[test]
fn helper_chunked_stream_reports_bounded_chunk_errors() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<FixtureRequest, FixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream_chunk_error",
    );
    let sink = RecordingTypedSink::<FixtureRow>::default();
    let diagnostics = RecordingDiagnostics::default();

    let err = session
        .run_streaming_command(
            &command,
            &FixtureRequest {
                source: "chunked-helper-error-test".to_owned(),
            },
            &sink,
            Some(&diagnostics),
            None,
            None,
        )
        .expect_err("chunk helper error should become a typed export status");

    match err {
        LeanWorkerError::StreamExportFailed { status } => assert_eq!(status, 10),
        other => panic!("expected stream export failure, got {other:?}"),
    }
    assert_eq!(
        sink.rows().len(),
        2,
        "the first chunk is delivered before the chunk error"
    );
    assert_eq!(
        diagnostics
            .diagnostics()
            .last()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("lean_rs.worker.stream.chunk_error"),
    );
}

#[test]
fn helper_completion_order_stream_uses_same_typed_surface() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<FixtureRequest, FixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream_chunked_completion",
    );
    let sink = RecordingTypedSink::<FixtureRow>::default();

    let summary = session
        .run_streaming_command(
            &command,
            &FixtureRequest {
                source: "chunked-helper-completion-test".to_owned(),
            },
            &sink,
            None,
            None,
            None,
        )
        .expect("completion-order helper stream succeeds");

    assert_eq!(summary.total_rows, 6);
    assert_eq!(summary.per_stream_counts.get("chunks"), Some(&6));
    assert_eq!(sink.rows().len(), 6);
}

#[test]
fn typed_streaming_command_row_decode_error_names_stream_and_sequence() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<FixtureRequest, WrongRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream",
    );
    let sink = RecordingTypedSink::<WrongRow>::default();

    let err = session
        .run_streaming_command(
            &command,
            &FixtureRequest {
                source: "typed-stream-test".to_owned(),
            },
            &sink,
            None,
            None,
            None,
        )
        .expect_err("bad row schema should be typed");

    match err {
        LeanWorkerError::TypedCommandRowDecode {
            export,
            stream,
            sequence,
            message,
        } => {
            assert_eq!(export, "lean_rs_interop_consumer_worker_data_stream");
            assert_eq!(stream, "rows");
            assert_eq!(sequence, 0);
            assert!(
                message.contains("missing"),
                "serde decode message should be carried, got {message}",
            );
        }
        other => panic!("expected typed row decode error, got {other:?}"),
    }
}

#[test]
fn typed_streaming_command_summary_decode_error_is_typed() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<FixtureRequest, FixtureRow, WrongSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream",
    );
    let sink = RecordingTypedSink::<FixtureRow>::default();

    let err = session
        .run_streaming_command(
            &command,
            &FixtureRequest {
                source: "typed-stream-test".to_owned(),
            },
            &sink,
            None,
            None,
            None,
        )
        .expect_err("bad summary schema should be typed");

    match err {
        LeanWorkerError::TypedCommandSummaryDecode { export, message } => {
            assert_eq!(export, "lean_rs_interop_consumer_worker_data_stream");
            assert!(
                message.contains("missing"),
                "serde decode message should be carried, got {message}",
            );
        }
        other => panic!("expected typed summary decode error, got {other:?}"),
    }
}

#[test]
fn typed_streaming_command_decodes_large_payload_from_performance_path() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<FixtureRequest, LargeFixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream_large_payload",
    );
    let sink = RecordingTypedSink::<LargeFixtureRow>::default();

    let summary = session
        .run_streaming_command(
            &command,
            &FixtureRequest {
                source: "typed-stream-large-payload-test".to_owned(),
            },
            &sink,
            None,
            None,
            None,
        )
        .expect("typed streaming command succeeds");

    assert_eq!(summary.total_rows, 1);
    let rows = sink.rows();
    assert_eq!(rows.len(), 1);
    let row = rows.first().expect("one row was recorded");
    assert_eq!(row.payload.kind, "large");
    assert_eq!(row.payload.blob.len(), 8192);
}

#[test]
fn raw_row_streaming_remains_available() {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let sink = RecordingRawSink::default();

    let summary = session
        .run_data_stream(
            "lean_rs_interop_consumer_worker_data_stream",
            &json!({"source": "raw-row-test"}),
            &sink,
            None,
            None,
            None,
        )
        .expect("raw streaming command still succeeds");

    assert_eq!(summary.total_rows, 2);
    assert_eq!(sink.rows().len(), 2);
}
