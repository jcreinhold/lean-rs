//! Mathlib-scale worker fixture probe.
//!
//! This example runs the normal large-workload path:
//!
//! ```text
//! capability builder -> LeanWorkerPool -> session lease -> typed command
//! ```
//!
//! It sends a fixed list of mathlib-style module names as the workload.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
//! cargo run -p lean-rs-worker-child --example mathlib_scale_probe
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lean_rs_worker_parent::{
    LeanWorkerCancellationToken, LeanWorkerCapabilityBuilder, LeanWorkerDiagnosticEvent, LeanWorkerDiagnosticSink,
    LeanWorkerError, LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerProgressEvent, LeanWorkerProgressSink,
    LeanWorkerRestartPolicy, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_TOTAL_CHILD_RSS_KIB: u64 = 4_194_304;
const DEFAULT_PER_WORKER_RSS_KIB: u64 = 1_572_864;
const DEFAULT_MAX_IMPORTS: u64 = 8;

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

    println!("workload=mathlib_scale_worker_fixture");
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("module_count={}", scale_modules().len());
    println!(
        "parent_rss_start_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );

    let single = run_index_workload("single_worker", 1, &worker_binary)?;
    let pooled = run_index_workload("pool_max_2", 2, &worker_binary)?;
    let cancellation = run_cancelled_workload(&worker_binary)?;
    let fatal = run_fatal_workload(&worker_binary)?;
    let cycle = run_cycle_workload(&worker_binary)?;
    let slow = run_slow_sink_workload(&worker_binary)?;

    println!(
        "summary single_rows={} pool_rows={} cancellation={} fatal_exit={} post_cycle_rows={} slow_sink_rows={}",
        single.rows, pooled.rows, cancellation, fatal, cycle.rows, slow.rows
    );
    println!(
        "parent_rss_end_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );
    println!("status=ok");
    Ok(())
}

#[derive(Clone, Copy)]
struct WorkloadResult {
    rows: u64,
}

fn scale_modules() -> Vec<String> {
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

fn builder(worker_binary: &Path) -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .validate_metadata(
        "lean_rs_interop_consumer_worker_shape_metadata",
        json!({"source": "mathlib_scale_probe"}),
    )
    .metadata_export("lean_rs_interop_consumer_worker_shape_metadata")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_index")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_panic_after_row")
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_many")
    .worker_executable(worker_binary)
    .restart_policy(memory_restart_policy())
}

fn run_index_workload(
    label: &str,
    max_workers: usize,
    worker_binary: &Path,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(memory_pool_config(max_workers));
    let command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_index",
    );
    let sink = CountingRows::default();
    let diagnostic_sink = CountingDiagnostics::default();
    let progress_sink = CountingProgress::default();
    let mut lease = pool.acquire_lease(builder(worker_binary))?;
    let summary = lease.run_streaming_command(
        &command,
        &ScaleRequest::probe(),
        &sink,
        Some(&diagnostic_sink),
        None,
        Some(&progress_sink),
    )?;
    drop(lease);
    let rows = summary.total_rows;
    let diagnostics = diagnostic_sink.count();
    let progress = progress_sink.count();

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

fn run_cancelled_workload(worker_binary: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(memory_pool_config(1));
    let token = LeanWorkerCancellationToken::new();
    let sink = CancelAfterFirstRow {
        token: &token,
        rows: Mutex::new(0),
    };
    let command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_index",
    );
    let mut lease = pool.acquire_lease(builder(worker_binary))?;
    let result = lease.run_streaming_command(&command, &ScaleRequest::probe(), &sink, None, Some(&token), None);
    println!(
        "cancellation elapsed_ms={:.3} lease_valid_after={}",
        started.elapsed().as_secs_f64() * 1000.0,
        lease.is_valid()
    );
    Ok(matches!(result, Err(LeanWorkerError::Cancelled { .. })))
}

fn run_fatal_workload(worker_binary: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(memory_pool_config(1));
    let sink = CountingRows::default();
    let command = LeanWorkerStreamingCommand::<ScaleRequest, ScaleRow, ScaleSummary>::new(
        "lean_rs_interop_consumer_worker_shape_mathlib_scale_panic_after_row",
    );
    let mut lease = pool.acquire_lease(builder(worker_binary))?;
    let result = lease.run_streaming_command(&command, &ScaleRequest::probe(), &sink, None, None, None);
    println!(
        "fatal_exit elapsed_ms={:.3} rows_before_failure={} lease_valid_after={}",
        started.elapsed().as_secs_f64() * 1000.0,
        sink.count(),
        lease.is_valid()
    );
    Ok(matches!(result, Err(LeanWorkerError::ChildPanicOrAbort { .. })))
}

fn run_cycle_workload(worker_binary: &Path) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let mut pool = LeanWorkerPool::new(memory_pool_config(1));
    let mut lease = pool.acquire_lease(builder(worker_binary))?;
    lease.cycle()?;
    drop(lease);
    run_index_workload("post_cycle", 1, worker_binary)
}

fn run_slow_sink_workload(worker_binary: &Path) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let parent_rss_before = current_process_rss_kib();
    let started = Instant::now();
    let mut pool = LeanWorkerPool::new(memory_pool_config(1));
    let sink = SlowManyRows::new(Duration::from_millis(2));
    let command = LeanWorkerStreamingCommand::<serde_json::Value, ManyRow, serde_json::Value>::new(
        "lean_rs_interop_consumer_worker_data_stream_many",
    );
    let mut lease = pool.acquire_lease(builder(worker_binary))?;
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

fn memory_pool_config(max_workers: usize) -> LeanWorkerPoolConfig {
    LeanWorkerPoolConfig::new(max_workers)
        .max_total_child_rss_kib(env_u64(
            "LEAN_RS_MATHLIB_SCALE_TOTAL_RSS_KIB",
            DEFAULT_TOTAL_CHILD_RSS_KIB,
        ))
        .per_worker_rss_ceiling_kib(env_u64(
            "LEAN_RS_MATHLIB_SCALE_PER_WORKER_RSS_KIB",
            DEFAULT_PER_WORKER_RSS_KIB,
        ))
}

fn memory_restart_policy() -> LeanWorkerRestartPolicy {
    LeanWorkerRestartPolicy::memory_bounded(
        env_u64("LEAN_RS_MATHLIB_SCALE_MAX_IMPORTS", DEFAULT_MAX_IMPORTS),
        env_u64("LEAN_RS_MATHLIB_SCALE_PER_WORKER_RSS_KIB", DEFAULT_PER_WORKER_RSS_KIB),
    )
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

#[derive(Clone, Debug, Serialize)]
struct ScaleRequest {
    workspace: String,
    modules: Vec<String>,
    limit: u64,
}

impl ScaleRequest {
    fn probe() -> Self {
        Self {
            workspace: "mathlib-scale-probe".to_owned(),
            modules: scale_modules(),
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
            "worker child binary not found at {}; run \
             `cargo build -p lean-rs-worker-child --bin lean-rs-worker-child` first",
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
