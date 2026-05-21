//! Lean-dup-class worker readiness proof.
//!
//! This example exercises the generic worker capability path that a downstream
//! subprocess worker can migrate to:
//!
//! ```text
//! import planner -> LeanWorkerPool -> session lease -> typed commands
//! ```
//!
//! The fixture uses command-like names (`version`, `doctor`, `index`,
//! `extract`, `features`, and `probe`) to prove worker coverage, but it does
//! not import `lean-dup` schemas or implement `lean-dup` policy.

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lean_rs_worker::{
    LeanWorkerCancellationToken, LeanWorkerDiagnosticEvent, LeanWorkerDiagnosticSink, LeanWorkerError,
    LeanWorkerImportPlanConfig, LeanWorkerImportPlanner, LeanWorkerJsonCommand, LeanWorkerModuleWork, LeanWorkerPool,
    LeanWorkerPoolConfig, LeanWorkerProgressEvent, LeanWorkerProgressSink, LeanWorkerStreamingCommand,
    LeanWorkerTypedDataRow, LeanWorkerTypedDataSink, LeanWorkerTypedStreamSummary,
};
use lean_toolchain::{LeanModuleSetFingerprint, ToolchainFingerprint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = interop_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsInteropConsumer")?;
    let parent_rss_before = current_process_rss_kib();
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(2).max_total_child_rss_kib(u64::MAX));

    let mut lease = pool.acquire_lease(planned_builder()?)?;
    let version = run_version(&mut lease)?;
    let doctor = run_doctor(&mut lease)?;
    let extract = run_shape_stream(&mut lease, "lean_rs_interop_consumer_worker_shape_extract")?;
    let features = run_shape_stream(&mut lease, "lean_rs_interop_consumer_worker_shape_features")?;
    let index = run_shape_stream(&mut lease, "lean_rs_interop_consumer_worker_shape_index")?;
    let probe = run_shape_stream(&mut lease, "lean_rs_interop_consumer_worker_shape_probe")?;
    let command_snapshot = lease.snapshot();
    drop(lease);

    let timeout = run_timeout(&mut pool)?;
    let cancellation = run_cancellation(&mut pool)?;
    let fatal = run_fatal_recovery(&mut pool)?;
    let backpressure = run_backpressure(&mut pool)?;
    let cycled = run_cycle(&mut pool)?;

    let snapshot = pool.snapshot();
    let elapsed = started.elapsed();
    let parent_rss_after = current_process_rss_kib();

    println!("workload=lean_dup_readiness");
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("commands={}", version.commands.join(","));
    println!("version_protocol={}", version.protocol);
    println!("doctor_diagnostics={}", doctor.diagnostics.len());
    println!(
        "command_rows extract={} features={} index={} probe={}",
        extract.total_rows, features.total_rows, index.total_rows, probe.total_rows
    );
    println!(
        "command_diagnostics extract={} features={} index={} probe={} progress_extract={} progress_features={} progress_index={} progress_probe={}",
        extract.diagnostics,
        features.diagnostics,
        index.diagnostics,
        probe.diagnostics,
        extract.progress,
        features.progress,
        index.progress,
        probe.progress
    );
    println!(
        "command_summary extract={} features={} index={} probe={}",
        extract.summary_command, features.summary_command, index.summary_command, probe.summary_command
    );
    println!(
        "command_snapshot workers={} active_workers={} warm_leases={} rows={} payload_bytes={} stream_successes={}",
        command_snapshot.workers,
        command_snapshot.active_workers,
        command_snapshot.warm_leases,
        command_snapshot.data_rows_delivered,
        command_snapshot.data_row_payload_bytes,
        command_snapshot.stream_successes
    );
    println!("timeout_result={timeout}");
    println!("cancellation_result={cancellation}");
    println!("fatal_recovery_result={fatal}");
    println!("explicit_cycle_result={cycled}");
    println!(
        "backpressure rows={} waits={} failures={}",
        backpressure.rows, backpressure.backpressure_waits, backpressure.backpressure_failures
    );
    println!(
        "pool_snapshot workers={} active_workers={} warm_leases={} queue_depth={} requests={} imports={} stream_requests={} stream_successes={} stream_failures={} data_rows={} payload_bytes={} backpressure_waits={} backpressure_failures={} child_rss_kib={} restarts={} timeout_restarts={} cancelled_restarts={} policy_restarts={} last_reason={:?}",
        snapshot.workers,
        snapshot.active_workers,
        snapshot.warm_leases,
        snapshot.queue_depth,
        snapshot.requests,
        snapshot.imports,
        snapshot.stream_requests,
        snapshot.stream_successes,
        snapshot.stream_failures,
        snapshot.data_rows_delivered,
        snapshot.data_row_payload_bytes,
        snapshot.backpressure_waits,
        snapshot.backpressure_failures,
        snapshot
            .total_child_rss_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot.worker_restarts,
        snapshot.timeout_restarts,
        snapshot.cancelled_restarts,
        snapshot.policy_restarts,
        snapshot.last_restart_reason,
    );
    println!(
        "rss parent_before_kib={} parent_after_kib={}",
        parent_rss_before.map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        parent_rss_after.map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );
    println!("elapsed_ms={:.3}", elapsed.as_secs_f64() * 1000.0);
    print_optional_comparison()?;
    println!(
        "downstream_owned=schemas,semantic_algorithms,cache_validity,ranking,reporting,source_provenance,cli_policy"
    );
    println!("status=ok");
    Ok(())
}

fn planned_builder() -> Result<lean_rs_worker::LeanWorkerCapabilityBuilder, Box<dyn std::error::Error>> {
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
        lakefile_sha256: "lean-dup-readiness-lakefile".to_owned(),
        manifest_sha256: Some("lean-dup-readiness-manifest".to_owned()),
        source_count: modules.len() as u64,
        source_max_mtime_ns: 0,
    };
    let config = LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .base_imports(["LeanRsInteropConsumer.Callback"])
        .validate_metadata(
            "lean_rs_interop_consumer_worker_shape_metadata",
            json!({"source": "lean_dup_readiness"}),
        );
    let batch = LeanWorkerImportPlanner::new(config)
        .plan_work_items(modules, &fingerprint)?
        .into_iter()
        .next()
        .ok_or("readiness planner produced no batches")?;
    Ok(batch.capability_builder())
}

fn run_version(
    lease: &mut lean_rs_worker::LeanWorkerSessionLease<'_>,
) -> Result<ShapeVersion, Box<dyn std::error::Error>> {
    let command =
        LeanWorkerJsonCommand::<ShapeRequest, ShapeVersion>::new("lean_rs_interop_consumer_worker_shape_version");
    Ok(lease.run_json_command(&command, &ShapeRequest::default(), None, None)?)
}

fn run_doctor(
    lease: &mut lean_rs_worker::LeanWorkerSessionLease<'_>,
) -> Result<ShapeDoctor, Box<dyn std::error::Error>> {
    let command =
        LeanWorkerJsonCommand::<ShapeRequest, ShapeDoctor>::new("lean_rs_interop_consumer_worker_shape_doctor");
    Ok(lease.run_json_command(&command, &ShapeRequest::default(), None, None)?)
}

fn run_shape_stream(
    lease: &mut lean_rs_worker::LeanWorkerSessionLease<'_>,
    export: &'static str,
) -> Result<StreamCapture, Box<dyn std::error::Error>> {
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(export);
    let rows = ShapeRows::default();
    let diagnostics = CountingDiagnostics::default();
    let progress = CountingProgress::default();
    let summary = lease.run_streaming_command(
        &command,
        &ShapeRequest::default(),
        &rows,
        Some(&diagnostics),
        None,
        Some(&progress),
    )?;
    capture_stream(&summary, &rows, &diagnostics, &progress)
}

fn run_timeout(pool: &mut LeanWorkerPool) -> Result<bool, Box<dyn std::error::Error>> {
    let mut lease = pool.acquire_lease(planned_builder()?)?;
    lease.set_request_timeout(Duration::from_millis(50))?;
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_timeout_after_row",
    );
    let rows = ShapeRows::default();
    let err = lease
        .run_streaming_command(&command, &ShapeRequest::default(), &rows, None, None, None)
        .expect_err("timeout fixture should time out");
    Ok(matches!(err, LeanWorkerError::Timeout { .. }) && !lease.is_valid())
}

fn run_cancellation(pool: &mut LeanWorkerPool) -> Result<bool, Box<dyn std::error::Error>> {
    let mut lease = pool.acquire_lease(planned_builder()?)?;
    let token = LeanWorkerCancellationToken::new();
    let rows = CancelAfterFirst { token: &token };
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_extract",
    );
    let err = lease
        .run_streaming_command(&command, &ShapeRequest::default(), &rows, None, Some(&token), None)
        .expect_err("cancellation fixture should cancel");
    Ok(matches!(err, LeanWorkerError::Cancelled { .. }) && !lease.is_valid())
}

fn run_fatal_recovery(pool: &mut LeanWorkerPool) -> Result<bool, Box<dyn std::error::Error>> {
    let mut lease = pool.acquire_lease(planned_builder()?)?;
    let rows = ShapeRows::default();
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_panic_after_row",
    );
    let err = lease
        .run_streaming_command(&command, &ShapeRequest::default(), &rows, None, None, None)
        .expect_err("panic fixture should kill the child");
    let fatal = matches!(err, LeanWorkerError::ChildPanicOrAbort { .. }) && !lease.is_valid();
    drop(lease);

    let mut fresh = pool.acquire_lease(planned_builder()?)?;
    let recovered = run_version(&mut fresh)?.protocol == "shape-1";
    Ok(fatal && recovered)
}

fn run_cycle(pool: &mut LeanWorkerPool) -> Result<bool, Box<dyn std::error::Error>> {
    let mut lease = pool.acquire_lease(planned_builder()?)?;
    lease.cycle()?;
    let invalidated = !lease.is_valid();
    drop(lease);

    let mut fresh = pool.acquire_lease(planned_builder()?)?;
    let recovered = run_version(&mut fresh)?.worker == "lean-rs-worker-fixture";
    Ok(invalidated && recovered)
}

fn run_backpressure(pool: &mut LeanWorkerPool) -> Result<BackpressureCapture, Box<dyn std::error::Error>> {
    let mut lease = pool.acquire_lease(planned_builder()?)?;
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ManyRow, Value>::new(
        "lean_rs_interop_consumer_worker_data_stream_many",
    );
    let rows = SlowManyRows::new(Duration::from_millis(2));
    let summary = lease.run_streaming_command(&command, &ShapeRequest::default(), &rows, None, None, None)?;
    let snapshot = lease.snapshot();
    Ok(BackpressureCapture {
        rows: summary.total_rows,
        backpressure_waits: snapshot.backpressure_waits,
        backpressure_failures: snapshot.backpressure_failures,
    })
}

fn capture_stream(
    summary: &LeanWorkerTypedStreamSummary<ShapeSummary>,
    rows: &ShapeRows,
    diagnostics: &CountingDiagnostics,
    progress: &CountingProgress,
) -> Result<StreamCapture, Box<dyn std::error::Error>> {
    let metadata = summary
        .metadata
        .as_ref()
        .ok_or("stream did not return terminal metadata")?;
    if !metadata.ok || metadata.rows != summary.total_rows {
        return Err("terminal metadata did not match stream summary".into());
    }
    if rows.count() != summary.total_rows {
        return Err("sink row count did not match stream summary".into());
    }
    Ok(StreamCapture {
        total_rows: summary.total_rows,
        diagnostics: diagnostics.count(),
        progress: progress.count(),
        summary_command: metadata.command.clone(),
    })
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
            workspace: "lean-dup-readiness".to_owned(),
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
    #[allow(dead_code)]
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ShapeDoctor {
    diagnostics: Vec<ShapeDoctorDiagnostic>,
    #[allow(dead_code)]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct ShapeDoctorDiagnostic {
    #[allow(dead_code)]
    severity: String,
    #[allow(dead_code)]
    code: String,
    #[allow(dead_code)]
    message: String,
    #[allow(dead_code)]
    details: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind")]
enum ShapeRow {
    #[serde(rename = "declaration")]
    Declaration {
        #[allow(dead_code)]
        module: String,
        #[allow(dead_code)]
        name: String,
        ordinal: u64,
    },
    #[serde(rename = "feature")]
    Feature {
        #[allow(dead_code)]
        module: String,
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        feature: String,
        score: u64,
        ordinal: u64,
    },
    #[serde(rename = "probe")]
    Probe {
        #[allow(dead_code)]
        left: String,
        #[allow(dead_code)]
        right: String,
        #[allow(dead_code)]
        relation: String,
        ordinal: u64,
    },
}

impl ShapeRow {
    fn checksum(&self) -> u64 {
        match self {
            Self::Declaration { ordinal, .. } | Self::Probe { ordinal, .. } => *ordinal,
            Self::Feature { score, ordinal, .. } => score.saturating_add(*ordinal),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShapeSummary {
    #[allow(dead_code)]
    fixture: String,
    command: String,
    ok: bool,
    rows: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct ManyRow {
    i: u64,
}

#[derive(Default)]
struct ShapeRows {
    metrics: Mutex<RowMetrics>,
}

#[derive(Default)]
struct RowMetrics {
    count: u64,
    checksum: u64,
}

impl ShapeRows {
    fn count(&self) -> u64 {
        self.metrics.lock().expect("row metrics lock is not poisoned").count
    }
}

impl LeanWorkerTypedDataSink<ShapeRow> for ShapeRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ShapeRow>) {
        let mut metrics = self.metrics.lock().expect("row metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics
            .checksum
            .saturating_add(row.payload.checksum())
            .saturating_add(row.sequence);
    }
}

struct SlowManyRows {
    delay: Duration,
    metrics: Mutex<RowMetrics>,
}

impl SlowManyRows {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            metrics: Mutex::new(RowMetrics::default()),
        }
    }
}

impl LeanWorkerTypedDataSink<ManyRow> for SlowManyRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ManyRow>) {
        std::thread::sleep(self.delay);
        let mut metrics = self.metrics.lock().expect("row metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics
            .checksum
            .saturating_add(row.payload.i)
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

#[derive(Default)]
struct CountingDiagnostics {
    count: Mutex<u64>,
}

impl CountingDiagnostics {
    fn count(&self) -> u64 {
        *self.count.lock().expect("diagnostics lock is not poisoned")
    }
}

impl LeanWorkerDiagnosticSink for CountingDiagnostics {
    fn report(&self, _diagnostic: LeanWorkerDiagnosticEvent) {
        let mut count = self.count.lock().expect("diagnostics lock is not poisoned");
        *count = count.saturating_add(1);
    }
}

#[derive(Default)]
struct CountingProgress {
    count: Mutex<u64>,
}

impl CountingProgress {
    fn count(&self) -> u64 {
        *self.count.lock().expect("progress lock is not poisoned")
    }
}

impl LeanWorkerProgressSink for CountingProgress {
    fn report(&self, _event: LeanWorkerProgressEvent) {
        let mut count = self.count.lock().expect("progress lock is not poisoned");
        *count = count.saturating_add(1);
    }
}

struct StreamCapture {
    total_rows: u64,
    diagnostics: u64,
    progress: u64,
    summary_command: String,
}

struct BackpressureCapture {
    rows: u64,
    backpressure_waits: u64,
    backpressure_failures: u64,
}

fn print_optional_comparison() -> Result<(), Box<dyn std::error::Error>> {
    let Some(lean_dup_root) = std::env::var_os("LEAN_RS_LEAN_DUP_ROOT").map(PathBuf::from) else {
        println!("lean_dup_checkout=unset");
        return Ok(());
    };
    if lean_dup_root.is_dir() {
        let revision = Command::new("git")
            .arg("-C")
            .arg(&lean_dup_root)
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|output| {
                output
                    .status
                    .success()
                    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            })
            .unwrap_or_else(|| "unavailable".to_owned());
        println!("lean_dup_checkout={} revision={revision}", lean_dup_root.display());
    } else {
        println!("lean_dup_checkout=missing");
    }

    if let Some(command) = std::env::var_os("LEAN_RS_WORKER_COMPARE_COMMAND") {
        let command = command.to_string_lossy();
        let started = Instant::now();
        let status = Command::new("sh").arg("-c").arg(command.as_ref()).status()?;
        println!("comparison_command={command}");
        println!("comparison_status_success={}", status.success());
        println!("comparison_elapsed_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
    } else {
        println!("comparison_command=skipped");
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), |workspace| workspace.to_path_buf())
}

fn interop_root() -> PathBuf {
    workspace_root().join("fixtures").join("interop-shims")
}

fn current_process_rss_kib() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}
