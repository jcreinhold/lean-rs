//! Mathlib-scale worker fixture probe.
//!
//! This example exercises the normal large-workload path:
//!
//! ```text
//! import planner -> LeanWorkerPool -> session lease -> typed command
//! ```
//!
//! It does not import `lean-dup` schemas. If `LEAN_RS_MATHLIB_ROOT` points at a
//! mathlib checkout, the planner uses that module list as the workload shape;
//! otherwise it uses a deterministic mathlib-shaped fallback.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo build -p lean-rs-worker --bin lean-rs-worker-child
//! cargo run -p lean-rs-worker --example mathlib_scale_probe
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lean_rs_worker_parent::{
    LeanWorkerCancellationToken, LeanWorkerDiagnosticEvent, LeanWorkerDiagnosticSink, LeanWorkerError,
    LeanWorkerImportPlanConfig, LeanWorkerImportPlanner, LeanWorkerModuleWork, LeanWorkerPool, LeanWorkerPoolConfig,
    LeanWorkerProgressEvent, LeanWorkerProgressSink, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow,
    LeanWorkerTypedDataSink,
};
use lean_toolchain::{
    LeanModuleDiscoveryOptions, LeanModuleSetFingerprint, ToolchainFingerprint, discover_lake_modules,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

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
    let worker_binary = worker_binary()?;
    let workload = workload()?;

    println!("workload=mathlib_scale_worker_fixture");
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("module_source={}", workload.source);
    println!("module_count={}", workload.modules.len());
    println!("mathlib_available={}", workload.mathlib_available);
    println!(
        "parent_rss_start_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );

    let single = run_index_workload("single_worker", 1, &worker_binary, &workload)?;
    let pooled = run_index_workload("pool_max_2", 2, &worker_binary, &workload)?;
    let cancellation = run_cancelled_workload(&worker_binary, &workload)?;
    let fatal = run_fatal_workload(&worker_binary, &workload)?;
    let cycle = run_cycle_workload(&worker_binary, &workload)?;
    let slow = run_slow_sink_workload(&worker_binary, &workload)?;

    println!(
        "summary single_rows={} pool_rows={} cancellation={} fatal_exit={} post_cycle_rows={} slow_sink_rows={}",
        single.rows, pooled.rows, cancellation, fatal, cycle.rows, slow.rows
    );
    println!(
        "parent_rss_end_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );
    println!("comparison_command=skipped");
    println!("status=ok");
    Ok(())
}

#[derive(Clone)]
struct Workload {
    source: String,
    mathlib_available: bool,
    modules: Vec<LeanWorkerModuleWork>,
    fingerprint: LeanModuleSetFingerprint,
}

#[derive(Clone, Copy)]
struct WorkloadResult {
    rows: u64,
}

fn workload() -> Result<Workload, Box<dyn std::error::Error>> {
    if let Some(root) = std::env::var_os("LEAN_RS_MATHLIB_ROOT") {
        let root = PathBuf::from(root);
        let limit = std::env::var("LEAN_RS_MATHLIB_SCALE_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(128);
        let discovered =
            discover_lake_modules(LeanModuleDiscoveryOptions::new(&root).selected_roots(["Mathlib".to_owned()]))?;
        let modules = discovered
            .modules
            .iter()
            .take(limit)
            .map(|module| {
                LeanWorkerModuleWork::new(
                    module.module.clone(),
                    module.path.clone(),
                    module.source_root.clone(),
                    ["LeanRsInteropConsumer.Callback"],
                )
            })
            .collect::<Vec<_>>();
        return Ok(Workload {
            source: root.display().to_string(),
            mathlib_available: true,
            modules,
            fingerprint: discovered.fingerprint,
        });
    }

    let modules = fallback_modules()
        .into_iter()
        .map(|module| {
            LeanWorkerModuleWork::new(
                module.clone(),
                PathBuf::from(format!("{}.lean", module.replace('.', "/"))),
                "Mathlib",
                ["LeanRsInteropConsumer.Callback"],
            )
        })
        .collect::<Vec<_>>();
    Ok(Workload {
        source: "fallback".to_owned(),
        mathlib_available: false,
        fingerprint: LeanModuleSetFingerprint {
            toolchain: ToolchainFingerprint::current(),
            lakefile_sha256: "mathlib-scale-fixture-lakefile".to_owned(),
            manifest_sha256: Some("mathlib-scale-fixture-manifest".to_owned()),
            source_count: modules.len() as u64,
            source_max_mtime_ns: 0,
        },
        modules,
    })
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

fn planned_builders(
    worker_binary: &Path,
    workload: &Workload,
) -> Result<Vec<lean_rs_worker_parent::LeanWorkerCapabilityBuilder>, Box<dyn std::error::Error>> {
    let config = LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .base_imports(["LeanRsInteropConsumer.Callback"])
        .validate_metadata(
            "lean_rs_interop_consumer_worker_shape_metadata",
            json!({"source": "mathlib_scale_probe"}),
        );
    let batches =
        LeanWorkerImportPlanner::new(config).plan_work_items(workload.modules.clone(), &workload.fingerprint)?;
    Ok(batches
        .into_iter()
        .map(|batch| {
            batch
                .capability_builder()
                .metadata_export("lean_rs_interop_consumer_worker_shape_metadata")
                .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_index")
                .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_panic_after_row")
                .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_many")
                .worker_executable(worker_binary)
        })
        .collect())
}

fn run_index_workload(
    label: &str,
    max_workers: usize,
    worker_binary: &Path,
    workload: &Workload,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(max_workers).max_total_child_rss_kib(u64::MAX));
    let command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_index",
    );
    let mut rows = 0_u64;
    let mut diagnostics = 0_usize;
    let mut progress = 0_usize;

    for builder in planned_builders(worker_binary, workload)? {
        let sink = CountingRows::default();
        let diagnostic_sink = CountingDiagnostics::default();
        let progress_sink = CountingProgress::default();
        let mut lease = pool.acquire_lease(builder)?;
        let summary = lease.run_streaming_command(
            &command,
            &ScaleRequest::from_workload(workload),
            &sink,
            Some(&diagnostic_sink),
            None,
            Some(&progress_sink),
        )?;
        rows = rows.saturating_add(summary.total_rows);
        diagnostics = diagnostics.saturating_add(diagnostic_sink.count());
        progress = progress.saturating_add(progress_sink.count());
    }

    let elapsed = started.elapsed();
    let snapshot = pool.snapshot();
    println!(
        "{label} elapsed_ms={:.3} rows={} rows_per_second={:.1} diagnostics={} progress={} workers={} active_workers={} warm_leases={} queue_depth={} child_rss_kib={} stream_requests={} stream_successes={} stream_failures={} data_rows_delivered={} data_row_payload_bytes={} stream_elapsed_ms={:.3} backpressure_waits={} backpressure_failures={} restarts={} policy_restarts={} timeout_restarts={} cancelled_restarts={} last_reason={:?}",
        elapsed.as_secs_f64() * 1000.0,
        rows,
        rows as f64 / elapsed.as_secs_f64().max(0.001),
        diagnostics,
        progress,
        snapshot.workers,
        snapshot.active_workers,
        snapshot.warm_leases,
        snapshot.queue_depth,
        snapshot
            .total_child_rss_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot.stream_requests,
        snapshot.stream_successes,
        snapshot.stream_failures,
        snapshot.data_rows_delivered,
        snapshot.data_row_payload_bytes,
        snapshot.stream_elapsed.as_secs_f64() * 1000.0,
        snapshot.backpressure_waits,
        snapshot.backpressure_failures,
        snapshot.worker_restarts,
        snapshot.policy_restarts,
        snapshot.timeout_restarts,
        snapshot.cancelled_restarts,
        snapshot.last_restart_reason,
    );
    Ok(WorkloadResult { rows })
}

fn run_cancelled_workload(worker_binary: &Path, workload: &Workload) -> Result<bool, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).max_total_child_rss_kib(u64::MAX));
    let token = LeanWorkerCancellationToken::new();
    let sink = CancelAfterFirstRow {
        token: &token,
        rows: Mutex::new(0),
    };
    let command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_index",
    );
    let mut lease = pool.acquire_lease(first_builder(worker_binary, workload)?)?;
    let result = lease.run_streaming_command(
        &command,
        &ScaleRequest::from_workload(workload),
        &sink,
        None,
        Some(&token),
        None,
    );
    println!(
        "cancellation elapsed_ms={:.3} lease_valid_after={}",
        started.elapsed().as_secs_f64() * 1000.0,
        lease.is_valid()
    );
    Ok(matches!(result, Err(LeanWorkerError::Cancelled { .. })))
}

fn run_fatal_workload(worker_binary: &Path, workload: &Workload) -> Result<bool, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).max_total_child_rss_kib(u64::MAX));
    let sink = CountingRows::default();
    let command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_panic_after_row",
    );
    let mut lease = pool.acquire_lease(first_builder(worker_binary, workload)?)?;
    let result = lease.run_streaming_command(
        &command,
        &ScaleRequest::from_workload(workload),
        &sink,
        None,
        None,
        None,
    );
    println!(
        "fatal_exit elapsed_ms={:.3} rows_before_failure={} lease_valid_after={}",
        started.elapsed().as_secs_f64() * 1000.0,
        sink.count(),
        lease.is_valid()
    );
    Ok(matches!(result, Err(LeanWorkerError::ChildPanicOrAbort { .. })))
}

fn run_cycle_workload(worker_binary: &Path, workload: &Workload) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).max_total_child_rss_kib(u64::MAX));
    let mut lease = pool.acquire_lease(first_builder(worker_binary, workload)?)?;
    lease.cycle()?;
    drop(lease);
    run_index_workload("post_cycle", 1, worker_binary, workload)
}

fn run_slow_sink_workload(
    worker_binary: &Path,
    workload: &Workload,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let parent_rss_before = current_process_rss_kib();
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).max_total_child_rss_kib(u64::MAX));
    let sink = SlowManyRows::new(Duration::from_millis(2));
    let command = LeanWorkerStreamingCommand::<serde_json::Value, ManyRow, serde_json::Value>::new(
        "lean_rs_interop_consumer_worker_data_stream_many",
    );
    let mut lease = pool.acquire_lease(first_builder(worker_binary, workload)?)?;
    let summary = lease.run_streaming_command(&command, &json!({"rows": 512}), &sink, None, None, None)?;
    drop(lease);
    let elapsed = started.elapsed();
    let snapshot = pool.snapshot();
    let parent_rss_after = current_process_rss_kib();
    println!(
        "slow_sink elapsed_ms={:.3} rows={} summary_rows={} parent_rss_before_kib={} parent_rss_after_kib={} child_rss_kib={} stream_requests={} stream_successes={} stream_failures={} data_rows_delivered={} data_row_payload_bytes={} backpressure_waits={} backpressure_failures={}",
        elapsed.as_secs_f64() * 1000.0,
        sink.count(),
        summary.total_rows,
        parent_rss_before.map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        parent_rss_after.map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot
            .total_child_rss_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot.stream_requests,
        snapshot.stream_successes,
        snapshot.stream_failures,
        snapshot.data_rows_delivered,
        snapshot.data_row_payload_bytes,
        snapshot.backpressure_waits,
        snapshot.backpressure_failures,
    );
    Ok(WorkloadResult {
        rows: summary.total_rows,
    })
}

fn first_builder(
    worker_binary: &Path,
    workload: &Workload,
) -> Result<lean_rs_worker_parent::LeanWorkerCapabilityBuilder, Box<dyn std::error::Error>> {
    planned_builders(worker_binary, workload)?
        .into_iter()
        .next()
        .ok_or_else(|| "workload produced no planned worker batches".into())
}

#[derive(Clone, Debug, Serialize)]
struct ScaleRequest {
    workspace: String,
    modules: Vec<String>,
    limit: u64,
}

impl ScaleRequest {
    fn from_workload(workload: &Workload) -> Self {
        Self {
            workspace: workload.source.clone(),
            modules: workload.modules.iter().map(|module| module.module.clone()).collect(),
            limit: 256,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind")]
enum ScaleRow {
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

impl ScaleRow {
    fn checksum(&self) -> u64 {
        match self {
            Self::Declaration { ordinal, .. } | Self::Probe { ordinal, .. } => *ordinal,
            Self::Feature { score, ordinal, .. } => score.saturating_add(*ordinal),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ScaleSummary {
    #[allow(dead_code)]
    fixture: String,
    #[allow(dead_code)]
    command: String,
    #[allow(dead_code)]
    ok: bool,
    #[allow(dead_code)]
    rows: u64,
    #[allow(dead_code)]
    modules: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct ManyRow {
    i: u64,
}

#[derive(Default)]
struct CountingRows {
    metrics: Mutex<SinkMetrics>,
}

#[derive(Default)]
struct SinkMetrics {
    count: u64,
    checksum: u64,
}

impl CountingRows {
    fn count(&self) -> u64 {
        self.metrics.lock().expect("metrics lock is not poisoned").count
    }
}

impl LeanWorkerTypedDataSink<ScaleRow> for CountingRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ScaleRow>) {
        let mut metrics = self.metrics.lock().expect("metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics
            .checksum
            .saturating_add(row.payload.checksum())
            .saturating_add(row.sequence);
    }
}

struct SlowManyRows {
    delay: Duration,
    metrics: Mutex<SinkMetrics>,
}

impl SlowManyRows {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            metrics: Mutex::new(SinkMetrics::default()),
        }
    }

    fn count(&self) -> u64 {
        self.metrics.lock().expect("metrics lock is not poisoned").count
    }
}

impl LeanWorkerTypedDataSink<ManyRow> for SlowManyRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ManyRow>) {
        std::thread::sleep(self.delay);
        let mut metrics = self.metrics.lock().expect("metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics
            .checksum
            .saturating_add(row.payload.i)
            .saturating_add(row.sequence);
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
struct CountingDiagnostics {
    count: Mutex<usize>,
}

impl CountingDiagnostics {
    fn count(&self) -> usize {
        *self.count.lock().expect("diagnostic lock is not poisoned")
    }
}

impl LeanWorkerDiagnosticSink for CountingDiagnostics {
    fn report(&self, _diagnostic: LeanWorkerDiagnosticEvent) {
        let mut count = self.count.lock().expect("diagnostic lock is not poisoned");
        *count = count.saturating_add(1);
    }
}

#[derive(Default)]
struct CountingProgress {
    count: Mutex<usize>,
}

impl CountingProgress {
    fn count(&self) -> usize {
        *self.count.lock().expect("progress lock is not poisoned")
    }
}

impl LeanWorkerProgressSink for CountingProgress {
    fn report(&self, _event: LeanWorkerProgressEvent) {
        let mut count = self.count.lock().expect("progress lock is not poisoned");
        *count = count.saturating_add(1);
    }
}

fn worker_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let current = std::env::current_exe()?;
    let profile_dir = current
        .parent()
        .and_then(Path::parent)
        .ok_or("example binary is not under target/<profile>/examples")?;
    let binary = profile_dir.join(format!("lean-rs-worker-child{}", std::env::consts::EXE_SUFFIX));
    if !binary.exists() {
        return Err(format!(
            "worker child binary not found at {}; run `cargo build -p lean-rs-worker --bin lean-rs-worker-child` first",
            binary.display()
        )
        .into());
    }
    Ok(binary)
}

fn interop_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(Path::parent).map_or_else(
        || PathBuf::from("fixtures/interop-shims"),
        |workspace| workspace.join("fixtures").join("interop-shims"),
    )
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
