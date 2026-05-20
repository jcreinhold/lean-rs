#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use lean_rs_worker::{
    LeanWorkerCancellationToken, LeanWorkerCapabilityBuilder, LeanWorkerDiagnosticEvent, LeanWorkerDiagnosticSink,
    LeanWorkerDoctorSeverity, LeanWorkerError, LeanWorkerJsonCommand, LeanWorkerRestartReason,
    LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink, LeanWorkerTypedStreamSummary,
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
    .worker_executable(worker_binary())
}

#[derive(Clone, Debug, Serialize)]
struct ShapeRequest {
    workspace: String,
    modules: Vec<String>,
    limit: u64,
}

impl Default for ShapeRequest {
    fn default() -> Self {
        Self {
            workspace: "fixture-workspace".to_owned(),
            modules: vec!["Fixture.Basic".to_owned()],
            limit: 8,
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct ShapeVersion {
    worker: String,
    protocol: String,
    commands: Vec<String>,
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind")]
enum ShapeRow {
    #[serde(rename = "declaration")]
    Declaration { module: String, name: String, ordinal: u64 },
    #[serde(rename = "feature")]
    Feature {
        module: String,
        name: String,
        feature: String,
        score: u64,
        ordinal: u64,
    },
    #[serde(rename = "probe")]
    Probe {
        left: String,
        right: String,
        relation: String,
        ordinal: u64,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct ShapeSummary {
    fixture: String,
    command: String,
    ok: bool,
    rows: u64,
}

#[derive(Default)]
struct RecordingRows {
    rows: Mutex<Vec<LeanWorkerTypedDataRow<ShapeRow>>>,
}

impl RecordingRows {
    fn rows(&self) -> Vec<LeanWorkerTypedDataRow<ShapeRow>> {
        self.rows.lock().expect("row lock is not poisoned").clone()
    }
}

impl LeanWorkerTypedDataSink<ShapeRow> for RecordingRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ShapeRow>) {
        self.rows.lock().expect("row lock is not poisoned").push(row);
    }
}

struct CancelAfterFirstRow<'a> {
    token: &'a LeanWorkerCancellationToken,
    rows: Mutex<Vec<LeanWorkerTypedDataRow<ShapeRow>>>,
}

impl CancelAfterFirstRow<'_> {
    fn rows(&self) -> Vec<LeanWorkerTypedDataRow<ShapeRow>> {
        self.rows.lock().expect("row lock is not poisoned").clone()
    }
}

impl LeanWorkerTypedDataSink<ShapeRow> for CancelAfterFirstRow<'_> {
    fn report(&self, row: LeanWorkerTypedDataRow<ShapeRow>) {
        self.rows.lock().expect("row lock is not poisoned").push(row);
        self.token.cancel();
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

fn run_shape_command(export: &str, expected_command: &str) -> LeanWorkerTypedStreamSummary<ShapeSummary> {
    let mut capability = builder().open().expect("builder opens capability");
    let mut session = capability.open_session(None, None).expect("session opens");
    let sink = RecordingRows::default();
    let diagnostics = RecordingDiagnostics::default();
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(export);

    let summary = session
        .run_streaming_command(
            &command,
            &ShapeRequest::default(),
            &sink,
            Some(&diagnostics),
            None,
            None,
        )
        .expect("shape command succeeds");

    assert_eq!(
        summary.metadata.as_ref().map(|metadata| metadata.command.as_str()),
        Some(expected_command)
    );
    assert_eq!(summary.total_rows, sink.rows().len() as u64);
    assert!(
        diagnostics.diagnostics().len() >= 2,
        "shape commands should emit start/finish diagnostics",
    );
    summary
}

#[test]
fn metadata_doctor_and_version_use_generic_capability_shapes() {
    let mut capability = builder()
        .validate_metadata(
            "lean_rs_interop_consumer_worker_shape_metadata",
            json!({"source": "shape-fixture-test"}),
        )
        .open()
        .expect("builder opens capability");
    let metadata = capability
        .validated_metadata()
        .expect("builder validated shape metadata");
    let command_names = metadata
        .commands
        .iter()
        .map(|command| command.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        command_names,
        vec!["version", "doctor", "extract", "features", "index", "probe"]
    );
    assert!(
        metadata
            .capabilities
            .iter()
            .any(|capability| capability.name == "rows.json.raw"),
    );

    let mut session = capability.open_session(None, None).expect("session opens");
    let doctor = session
        .capability_doctor(
            "lean_rs_interop_consumer_worker_shape_doctor",
            &json!({"source": "shape-fixture-test"}),
            None,
            None,
        )
        .expect("doctor export succeeds");
    assert_eq!(doctor.diagnostics.len(), 3);
    assert_eq!(
        doctor.diagnostics.first().map(|diagnostic| diagnostic.severity),
        Some(LeanWorkerDoctorSeverity::Pass)
    );
    assert_eq!(
        doctor.diagnostics.get(1).map(|diagnostic| diagnostic.severity),
        Some(LeanWorkerDoctorSeverity::Warning)
    );

    let version_command =
        LeanWorkerJsonCommand::<ShapeRequest, ShapeVersion>::new("lean_rs_interop_consumer_worker_shape_version");
    let version = session
        .run_json_command(&version_command, &ShapeRequest::default(), None, None)
        .expect("version command succeeds");
    assert_eq!(version.protocol, "shape-1");
    assert!(version.commands.contains(&"index".to_owned()));
    assert!(version.capabilities.contains(&"diagnostics".to_owned()));
}

#[test]
fn streaming_commands_use_builder_and_typed_facade() {
    let extract = run_shape_command("lean_rs_interop_consumer_worker_shape_extract", "extract");
    assert_eq!(extract.total_rows, 2);
    assert_eq!(
        extract.per_stream_counts,
        BTreeMap::from([("declarations".to_owned(), 2)])
    );

    let features = run_shape_command("lean_rs_interop_consumer_worker_shape_features", "features");
    assert_eq!(features.per_stream_counts, BTreeMap::from([("features".to_owned(), 2)]));

    let index = run_shape_command("lean_rs_interop_consumer_worker_shape_index", "index");
    assert_eq!(index.total_rows, 4);
    assert_eq!(
        index.per_stream_counts,
        BTreeMap::from([("declarations".to_owned(), 2), ("features".to_owned(), 2)])
    );

    let probe = run_shape_command("lean_rs_interop_consumer_worker_shape_probe", "probe");
    assert_eq!(probe.total_rows, 1);
    assert_eq!(probe.per_stream_counts, BTreeMap::from([("probes".to_owned(), 1)]));
}

#[test]
fn timeout_cancellation_and_fatal_exit_have_distinct_worker_outcomes() {
    let mut timeout_capability = builder().open().expect("builder opens timeout capability");
    {
        let mut session = timeout_capability.open_session(None, None).expect("session opens");
        session.set_request_timeout(Duration::from_millis(50));
        let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
            "lean_rs_interop_consumer_worker_shape_timeout_after_row",
        );
        let sink = RecordingRows::default();
        let err = session
            .run_streaming_command(&command, &ShapeRequest::default(), &sink, None, None, None)
            .expect_err("slow shape command should time out");
        match err {
            LeanWorkerError::Timeout { operation, duration } => {
                assert_eq!(operation, "worker_run_data_stream");
                assert_eq!(duration, Duration::from_millis(50));
            }
            other => panic!("expected timeout, got {other:?}"),
        }
        assert_eq!(sink.rows().len(), 1, "pre-timeout row is tentative");
    }
    assert_eq!(timeout_capability.worker().stats().timeout_restarts, 1);

    let token = LeanWorkerCancellationToken::new();
    let cancel_sink = CancelAfterFirstRow {
        token: &token,
        rows: Mutex::new(Vec::new()),
    };
    let mut cancel_capability = builder().open().expect("builder opens cancellation capability");
    {
        let mut session = cancel_capability.open_session(None, None).expect("session opens");
        let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
            "lean_rs_interop_consumer_worker_shape_extract",
        );
        let err = session
            .run_streaming_command(
                &command,
                &ShapeRequest::default(),
                &cancel_sink,
                None,
                Some(&token),
                None,
            )
            .expect_err("sink cancellation should stop shape command");
        match err {
            LeanWorkerError::Cancelled { operation } => assert_eq!(operation, "worker_run_data_stream"),
            other => panic!("expected cancellation, got {other:?}"),
        }
    }
    assert_eq!(cancel_sink.rows().len(), 1);
    assert_eq!(
        cancel_capability.worker().stats().last_restart_reason,
        Some(LeanWorkerRestartReason::Cancelled {
            operation: "worker_run_data_stream",
        }),
    );

    let mut panic_capability = builder().open().expect("builder opens fatal-exit capability");
    let panic_sink = RecordingRows::default();
    let mut session = panic_capability.open_session(None, None).expect("session opens");
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_panic_after_row",
    );
    let err = session
        .run_streaming_command(&command, &ShapeRequest::default(), &panic_sink, None, None, None)
        .expect_err("panic shape command should kill only the child");
    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => {
            assert!(!exit.success, "panic command should terminate the child");
        }
        other => panic!("expected child panic/abort, got {other:?}"),
    }
    assert_eq!(panic_sink.rows().len(), 1);
}

#[test]
fn worker_cycle_leaves_shape_fixture_usable() {
    let mut capability = builder().open().expect("builder opens capability");
    let command =
        LeanWorkerJsonCommand::<ShapeRequest, ShapeVersion>::new("lean_rs_interop_consumer_worker_shape_version");
    {
        let mut session = capability.open_session(None, None).expect("session opens");
        let version = session
            .run_json_command(&command, &ShapeRequest::default(), None, None)
            .expect("version command succeeds before cycle");
        assert_eq!(version.worker, "lean-rs-worker-fixture");
    }

    capability.worker_mut().cycle().expect("worker cycle succeeds");
    assert_eq!(
        capability.worker().stats().last_restart_reason,
        Some(LeanWorkerRestartReason::Explicit)
    );

    let mut session = capability.open_session(None, None).expect("session opens after cycle");
    let version = session
        .run_json_command(&command, &ShapeRequest::default(), None, None)
        .expect("version command succeeds after cycle");
    assert_eq!(version.protocol, "shape-1");
}
