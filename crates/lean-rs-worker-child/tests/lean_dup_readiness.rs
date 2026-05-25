#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use lean_rs_worker_parent::{
    LeanWorkerCancellationToken, LeanWorkerError, LeanWorkerImportPlanConfig, LeanWorkerImportPlanner,
    LeanWorkerJsonCommand, LeanWorkerModuleWork, LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerStreamingCommand,
    LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use lean_toolchain::{LeanModuleSetFingerprint, ToolchainFingerprint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

fn planned_builder() -> lean_rs_worker_parent::LeanWorkerCapabilityBuilder {
    let modules = ["Fixture.Basic", "Fixture.Advanced"]
        .into_iter()
        .map(|module| {
            LeanWorkerModuleWork::new(
                module,
                PathBuf::from(format!("{}.lean", module.replace('.', "/"))),
                "Fixture",
                ["LeanRsInteropConsumer.Callback"],
            )
        })
        .collect::<Vec<_>>();
    let fingerprint = LeanModuleSetFingerprint {
        toolchain: ToolchainFingerprint::current(),
        lakefile_sha256: "lean-dup-readiness-test-lakefile".to_owned(),
        manifest_sha256: Some("lean-dup-readiness-test-manifest".to_owned()),
        source_count: modules.len() as u64,
        source_max_mtime_ns: 0,
    };
    let config = LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .base_imports(["LeanRsInteropConsumer.Callback"])
        .validate_metadata(
            "lean_rs_interop_consumer_worker_shape_metadata",
            json!({"source": "lean-dup-readiness-test"}),
        );
    LeanWorkerImportPlanner::new(config)
        .plan_work_items(modules, &fingerprint)
        .expect("readiness work items plan")
        .into_iter()
        .next()
        .expect("readiness planner produced a batch")
        .capability_builder()
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
            workspace: "lean-dup-readiness-test".to_owned(),
            modules: vec!["Fixture.Basic".to_owned(), "Fixture.Advanced".to_owned()],
            limit: 512,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShapeVersion {
    worker: String,
    protocol: String,
    commands: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ShapeDoctor {
    diagnostics: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind")]
enum ShapeRow {
    #[serde(rename = "declaration")]
    Declaration { ordinal: u64 },
    #[serde(rename = "feature")]
    Feature { score: u64, ordinal: u64 },
    #[serde(rename = "probe")]
    Probe { ordinal: u64 },
}

impl ShapeRow {
    fn checksum(&self) -> u64 {
        match self {
            Self::Declaration { ordinal } | Self::Probe { ordinal } => *ordinal,
            Self::Feature { score, ordinal } => score.saturating_add(*ordinal),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShapeSummary {
    command: String,
    ok: bool,
    rows: u64,
}

#[derive(Default)]
struct CountingRows {
    metrics: Mutex<RowMetrics>,
}

#[derive(Default)]
struct RowMetrics {
    count: u64,
    checksum: u64,
}

impl CountingRows {
    fn count(&self) -> u64 {
        self.metrics.lock().expect("row metrics lock is not poisoned").count
    }
}

impl LeanWorkerTypedDataSink<ShapeRow> for CountingRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ShapeRow>) {
        let mut metrics = self.metrics.lock().expect("row metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics
            .checksum
            .saturating_add(row.payload.checksum())
            .saturating_add(row.sequence);
    }
}

struct CancelAfterFirst<'a> {
    token: &'a LeanWorkerCancellationToken,
}

impl LeanWorkerTypedDataSink<ShapeRow> for CancelAfterFirst<'_> {
    fn report(&self, _row: LeanWorkerTypedDataRow<ShapeRow>) {
        self.token.cancel();
    }
}

#[test]
fn readiness_commands_run_through_planner_pool_lease_and_typed_facade() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(2).max_total_child_rss_kib(u64::MAX));
    let mut lease = pool
        .acquire_lease(planned_builder())
        .expect("pool opens readiness lease");

    let version_command =
        LeanWorkerJsonCommand::<ShapeRequest, ShapeVersion>::new("lean_rs_interop_consumer_worker_shape_version");
    let version = lease
        .run_json_command(&version_command, &ShapeRequest::default(), None, None)
        .expect("version command succeeds");
    assert_eq!(version.worker, "lean-rs-worker-fixture");
    assert_eq!(version.protocol, "shape-1");
    assert_eq!(
        version.commands,
        vec!["version", "doctor", "extract", "features", "index", "probe"]
    );

    let doctor_command =
        LeanWorkerJsonCommand::<ShapeRequest, ShapeDoctor>::new("lean_rs_interop_consumer_worker_shape_doctor");
    let doctor = lease
        .run_json_command(&doctor_command, &ShapeRequest::default(), None, None)
        .expect("doctor command succeeds");
    assert_eq!(doctor.diagnostics.len(), 3);

    for (export, command_name, expected_rows) in [
        ("lean_rs_interop_consumer_worker_shape_extract", "extract", 2),
        ("lean_rs_interop_consumer_worker_shape_features", "features", 2),
        ("lean_rs_interop_consumer_worker_shape_index", "index", 4),
        ("lean_rs_interop_consumer_worker_shape_probe", "probe", 1),
    ] {
        let rows = CountingRows::default();
        let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(export);
        let summary = lease
            .run_streaming_command(&command, &ShapeRequest::default(), &rows, None, None, None)
            .expect("streaming command succeeds");
        assert_eq!(summary.total_rows, expected_rows);
        assert_eq!(summary.total_rows, rows.count());
        assert_eq!(
            summary.metadata.as_ref().map(|metadata| metadata.command.as_str()),
            Some(command_name)
        );
        assert!(summary.metadata.as_ref().is_some_and(|metadata| metadata.ok));
        assert_eq!(
            summary.metadata.as_ref().map(|metadata| metadata.rows),
            Some(expected_rows)
        );
    }

    let snapshot = lease.snapshot();
    assert_eq!(snapshot.active_workers, 1);
    assert_eq!(snapshot.stream_successes, 4);
    assert_eq!(snapshot.data_rows_delivered, 9);
    assert_eq!(snapshot.queue_depth, 0);
}

#[test]
fn readiness_operational_failures_and_recovery_are_typed() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).max_total_child_rss_kib(u64::MAX));

    let mut timeout_lease = pool.acquire_lease(planned_builder()).expect("pool opens timeout lease");
    timeout_lease
        .set_request_timeout(Duration::from_millis(50))
        .expect("request timeout can be set");
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_timeout_after_row",
    );
    let err = timeout_lease
        .run_streaming_command(
            &command,
            &ShapeRequest::default(),
            &CountingRows::default(),
            None,
            None,
            None,
        )
        .expect_err("timeout command should time out");
    assert!(matches!(err, LeanWorkerError::Timeout { .. }));
    assert!(!timeout_lease.is_valid());
    drop(timeout_lease);

    let mut cancel_lease = pool.acquire_lease(planned_builder()).expect("pool opens cancel lease");
    let token = LeanWorkerCancellationToken::new();
    let cancel_rows = CancelAfterFirst { token: &token };
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_extract",
    );
    let err = cancel_lease
        .run_streaming_command(
            &command,
            &ShapeRequest::default(),
            &cancel_rows,
            None,
            Some(&token),
            None,
        )
        .expect_err("cancel command should cancel");
    assert!(matches!(err, LeanWorkerError::Cancelled { .. }));
    assert!(!cancel_lease.is_valid());
    drop(cancel_lease);

    let mut fatal_lease = pool.acquire_lease(planned_builder()).expect("pool opens fatal lease");
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_panic_after_row",
    );
    let err = fatal_lease
        .run_streaming_command(
            &command,
            &ShapeRequest::default(),
            &CountingRows::default(),
            None,
            None,
            None,
        )
        .expect_err("panic command should kill the child");
    assert!(matches!(err, LeanWorkerError::ChildPanicOrAbort { .. }));
    assert!(!fatal_lease.is_valid());
    drop(fatal_lease);

    let mut cycle_lease = pool.acquire_lease(planned_builder()).expect("pool opens cycle lease");
    cycle_lease.cycle().expect("explicit cycle succeeds");
    assert!(!cycle_lease.is_valid());
    drop(cycle_lease);

    let mut recovered = pool
        .acquire_lease(planned_builder())
        .expect("pool opens recovered lease");
    let version_command =
        LeanWorkerJsonCommand::<ShapeRequest, ShapeVersion>::new("lean_rs_interop_consumer_worker_shape_version");
    let version = recovered
        .run_json_command(&version_command, &ShapeRequest::default(), None, None)
        .expect("version command succeeds after failures");
    assert_eq!(version.protocol, "shape-1");
}
