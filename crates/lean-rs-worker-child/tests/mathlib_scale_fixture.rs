#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use lean_rs_worker_parent::{
    LeanWorkerCancellationToken, LeanWorkerDiagnosticEvent, LeanWorkerDiagnosticSink, LeanWorkerError,
    LeanWorkerImportPlanConfig, LeanWorkerImportPlanner, LeanWorkerModuleWork, LeanWorkerPool, LeanWorkerPoolConfig,
    LeanWorkerProgressEvent, LeanWorkerProgressSink, LeanWorkerRestartReason, LeanWorkerStreamingCommand,
    LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use lean_toolchain::{LeanModuleSetFingerprint, ToolchainFingerprint};
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

fn fallback_modules() -> Vec<String> {
    [
        "Mathlib.Algebra.Group.Basic",
        "Mathlib.Algebra.Ring.Basic",
        "Mathlib.Algebra.Module.Basic",
        "Mathlib.Order.Basic",
        "Mathlib.Data.Nat.Basic",
        "Mathlib.Data.Int.Basic",
        "Mathlib.Data.List.Basic",
        "Mathlib.Data.Set.Basic",
        "Mathlib.Topology.Basic",
        "Mathlib.Topology.Algebra.Group.Basic",
        "Mathlib.CategoryTheory.Category.Basic",
        "Mathlib.CategoryTheory.Functor.Basic",
        "Mathlib.CategoryTheory.NaturalTransformation",
        "Mathlib.LinearAlgebra.Basic",
        "Mathlib.RingTheory.Ideal.Basic",
        "Mathlib.FieldTheory.Basic",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn source_fingerprint(module_count: usize) -> LeanModuleSetFingerprint {
    LeanModuleSetFingerprint {
        toolchain: ToolchainFingerprint::current(),
        lakefile_sha256: "mathlib-scale-fixture-lakefile".to_owned(),
        manifest_sha256: Some("mathlib-scale-fixture-manifest".to_owned()),
        source_count: module_count as u64,
        source_max_mtime_ns: 0,
    }
}

fn planned_builder() -> lean_rs_worker_parent::LeanWorkerCapabilityBuilder {
    let modules = fallback_modules();
    let config = LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .base_imports(["LeanRsInteropConsumer.Callback"])
        .validate_metadata(
            "lean_rs_interop_consumer_worker_shape_metadata",
            json!({"source": "mathlib-scale-fixture-test"}),
        );
    let planner = LeanWorkerImportPlanner::new(config);
    let work = modules.iter().map(|module| {
        LeanWorkerModuleWork::new(
            module,
            PathBuf::from(format!("{}.lean", module.replace('.', "/"))),
            "Mathlib",
            ["LeanRsInteropConsumer.Callback"],
        )
    });
    let batches = planner
        .plan_work_items(work, &source_fingerprint(modules.len()))
        .expect("mathlib-shaped modules plan");
    assert_eq!(batches.len(), 1, "fallback modules share one worker session key");
    batches
        .into_iter()
        .next()
        .expect("one planned batch")
        .capability_builder()
        .metadata_export("lean_rs_interop_consumer_worker_shape_metadata")
        .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_index")
        .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_timeout_after_row")
        .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_panic_after_row")
        .worker_executable(worker_binary())
}

#[derive(Clone, Debug, Serialize)]
struct ScaleRequest {
    workspace: String,
    modules: Vec<String>,
    limit: u64,
}

impl ScaleRequest {
    fn fallback(limit: u64) -> Self {
        Self {
            workspace: "mathlib-scale-fixture".to_owned(),
            modules: fallback_modules(),
            limit,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind")]
enum ScaleRow {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct ScaleSummary {
    fixture: String,
    command: String,
    ok: bool,
    rows: u64,
    modules: u64,
}

#[derive(Default)]
struct RecordingRows {
    rows: Mutex<Vec<LeanWorkerTypedDataRow<ScaleRow>>>,
}

impl RecordingRows {
    fn rows(&self) -> Vec<LeanWorkerTypedDataRow<ScaleRow>> {
        self.rows.lock().expect("row lock is not poisoned").clone()
    }
}

impl LeanWorkerTypedDataSink<ScaleRow> for RecordingRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ScaleRow>) {
        self.rows.lock().expect("row lock is not poisoned").push(row);
    }
}

struct CancelAfterFirstRow<'a> {
    token: &'a LeanWorkerCancellationToken,
    rows: Mutex<u64>,
}

impl LeanWorkerTypedDataSink<ScaleRow> for CancelAfterFirstRow<'_> {
    fn report(&self, _row: LeanWorkerTypedDataRow<ScaleRow>) {
        let mut rows = self.rows.lock().expect("row lock is not poisoned");
        *rows = rows.saturating_add(1);
        drop(rows);
        self.token.cancel();
    }
}

#[derive(Default)]
struct RecordingDiagnostics {
    diagnostics: Mutex<Vec<LeanWorkerDiagnosticEvent>>,
}

impl RecordingDiagnostics {
    fn codes(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .expect("diagnostic lock is not poisoned")
            .iter()
            .map(|event| event.code.clone())
            .collect()
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
fn mathlib_scale_fixture_runs_through_planner_pool_lease_and_typed_command() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(2));
    let mut lease = pool.acquire_lease(planned_builder()).expect("planned lease opens");
    let command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_index",
    );
    let rows = RecordingRows::default();
    let diagnostics = RecordingDiagnostics::default();
    let progress = RecordingProgress::default();

    let summary = lease
        .run_streaming_command(
            &command,
            &ScaleRequest::fallback(128),
            &rows,
            Some(&diagnostics),
            None,
            Some(&progress),
        )
        .expect("mathlib-scale index stream succeeds");

    assert_eq!(summary.total_rows, 47);
    assert_eq!(
        summary.per_stream_counts,
        BTreeMap::from([
            ("declarations".to_owned(), 16),
            ("features".to_owned(), 16),
            ("probes".to_owned(), 15),
        ]),
    );
    assert_eq!(
        summary.metadata,
        Some(ScaleSummary {
            fixture: "mathlib-scale-shaped".to_owned(),
            command: "index".to_owned(),
            ok: true,
            rows: 47,
            modules: 16,
        }),
    );
    assert_eq!(rows.rows().len(), 47);
    assert_eq!(
        diagnostics.codes(),
        vec!["scale.index.started".to_owned(), "scale.index.finished".to_owned()],
    );
    assert_eq!(
        progress
            .events()
            .into_iter()
            .filter(|event| event.phase == "scale.index")
            .map(|event| (event.current, event.total))
            .collect::<Vec<_>>(),
        vec![(1, Some(4)), (2, Some(4)), (3, Some(4)), (4, Some(4))],
    );
    assert!(lease.is_valid(), "successful command keeps the lease valid");
}

#[test]
fn mathlib_scale_fixture_reports_timeout_cancellation_and_fatal_exit_distinctly() {
    let mut timeout_pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut timeout_lease = timeout_pool
        .acquire_lease(planned_builder())
        .expect("planned lease opens");
    timeout_lease
        .set_request_timeout(Duration::from_millis(50))
        .expect("request timeout is set");
    let timeout_command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_timeout_after_row",
    );
    let timeout_rows = RecordingRows::default();
    let err = timeout_lease
        .run_streaming_command(
            &timeout_command,
            &ScaleRequest::fallback(8),
            &timeout_rows,
            None,
            None,
            None,
        )
        .expect_err("slow mathlib-scale stream times out");
    match err {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "worker_run_data_stream"),
        other => panic!("expected timeout, got {other:?}"),
    }
    assert_eq!(timeout_rows.rows().len(), 1);
    assert!(!timeout_lease.is_valid());

    let mut cancel_pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut cancel_lease = cancel_pool
        .acquire_lease(planned_builder())
        .expect("planned lease opens");
    let token = LeanWorkerCancellationToken::new();
    let cancel_rows = CancelAfterFirstRow {
        token: &token,
        rows: Mutex::new(0),
    };
    let index_command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_index",
    );
    let err = cancel_lease
        .run_streaming_command(
            &index_command,
            &ScaleRequest::fallback(64),
            &cancel_rows,
            None,
            Some(&token),
            None,
        )
        .expect_err("sink cancellation stops mathlib-scale stream");
    match err {
        LeanWorkerError::Cancelled { operation } => assert_eq!(operation, "worker_run_data_stream"),
        other => panic!("expected cancellation, got {other:?}"),
    }
    assert!(!cancel_lease.is_valid());

    let mut panic_pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut panic_lease = panic_pool
        .acquire_lease(planned_builder())
        .expect("planned lease opens");
    let panic_command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_panic_after_row",
    );
    let panic_rows = RecordingRows::default();
    let err = panic_lease
        .run_streaming_command(
            &panic_command,
            &ScaleRequest::fallback(8),
            &panic_rows,
            None,
            None,
            None,
        )
        .expect_err("panic stream kills only the child");
    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => {
            assert!(!exit.success, "fatal stream should terminate the child");
        }
        other => panic!("expected child panic/abort, got {other:?}"),
    }
    assert_eq!(panic_rows.rows().len(), 1);
    assert!(!panic_lease.is_valid());
    drop(panic_lease);

    let snapshot = panic_pool.snapshot();
    assert!(
        snapshot.last_restart_reason.is_none()
            || matches!(
                snapshot.last_restart_reason,
                Some(LeanWorkerRestartReason::Explicit | LeanWorkerRestartReason::Cancelled { .. })
            ),
        "fatal exit is a worker failure, not a pool policy restart"
    );
}
