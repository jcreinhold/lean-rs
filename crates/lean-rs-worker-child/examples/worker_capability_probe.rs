//! Lean-dup-shaped worker capability probe.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo run --release -p lean-rs-worker --example worker_capability_probe
//! ```

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use lean_rs_worker_parent::{
    LeanWorkerCancellationToken, LeanWorkerCapabilityBuilder, LeanWorkerError, LeanWorkerStreamingCommand,
    LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let parent_rss_before = parent_rss_kib();
    let cold_started = Instant::now();
    let mut capability = capability_builder()
        .validate_metadata(
            "lean_rs_interop_consumer_worker_shape_metadata",
            json!({"source": "worker_capability_probe"}),
        )
        .open()?;
    let cold_start = cold_started.elapsed();
    let child_rss_after_start = capability.worker_mut().rss_kib();

    let import_started = Instant::now();
    {
        let session = capability.open_session(None, None)?;
        std::hint::black_box(session.request_timeout());
    }
    let first_import = import_started.elapsed();

    let stream_started = Instant::now();
    let index_rows = run_stream(&mut capability, "lean_rs_interop_consumer_worker_shape_index")?;
    let stream_elapsed = stream_started.elapsed();

    let cancel_started = Instant::now();
    let cancellation = run_cancelled_stream(&mut capability)?;
    let cancel_elapsed = cancel_started.elapsed();

    let fatal_started = Instant::now();
    let fatal_exit = run_fatal_stream(&mut capability)?;
    let fatal_elapsed = fatal_started.elapsed();

    let cycle_started = Instant::now();
    capability.worker_mut().cycle()?;
    let cycle_elapsed = cycle_started.elapsed();

    let post_cycle_rows = run_stream(&mut capability, "lean_rs_interop_consumer_worker_shape_extract")?;
    let child_rss_after = capability.worker_mut().rss_kib();
    let parent_rss_after = parent_rss_kib();
    let stats = capability.worker().stats();

    println!("workload=worker_capability_probe");
    println!("cold_start_ms={:.3}", cold_start.as_secs_f64() * 1000.0);
    println!("first_import_ms={:.3}", first_import.as_secs_f64() * 1000.0);
    println!("index_rows={index_rows}");
    println!("stream_ms={:.3}", stream_elapsed.as_secs_f64() * 1000.0);
    println!(
        "stream_rows_per_second={:.1}",
        index_rows as f64 / stream_elapsed.as_secs_f64().max(0.001)
    );
    println!("cancellation_result={cancellation}");
    println!("cancellation_latency_ms={:.3}", cancel_elapsed.as_secs_f64() * 1000.0);
    println!("fatal_exit_result={fatal_exit}");
    println!("fatal_exit_recovery_ms={:.3}", fatal_elapsed.as_secs_f64() * 1000.0);
    println!("cycle_ms={:.3}", cycle_elapsed.as_secs_f64() * 1000.0);
    println!("post_cycle_rows={post_cycle_rows}");
    println!("parent_rss_before_kib={parent_rss_before:?}");
    println!("parent_rss_after_kib={parent_rss_after:?}");
    println!("child_rss_after_start_kib={child_rss_after_start:?}");
    println!("child_rss_after_kib={child_rss_after:?}");
    println!(
        "stats requests={} imports={} restarts={} exits={} cancelled_restarts={} explicit_cycles={} last_reason={:?}",
        stats.requests,
        stats.imports,
        stats.restarts,
        stats.exits,
        stats.cancelled_restarts,
        stats.explicit_cycles,
        stats.last_restart_reason
    );
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

#[derive(Clone, Debug, Serialize)]
struct ShapeRequest {
    workspace: String,
    modules: Vec<String>,
    limit: u64,
}

impl Default for ShapeRequest {
    fn default() -> Self {
        Self {
            workspace: "probe-workspace".to_owned(),
            modules: vec!["Fixture.Basic".to_owned()],
            limit: 8,
        }
    }
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

#[derive(Clone, Debug, Deserialize)]
struct ShapeSummary {
    #[allow(dead_code)]
    fixture: String,
    #[allow(dead_code)]
    command: String,
    ok: bool,
    rows: u64,
}

#[derive(Default)]
struct CountingSink {
    metrics: Mutex<SinkMetrics>,
}

#[derive(Default)]
struct SinkMetrics {
    count: u64,
    checksum: u64,
}

impl CountingSink {
    fn count(&self) -> u64 {
        self.metrics.lock().expect("metrics lock is not poisoned").count
    }
}

impl LeanWorkerTypedDataSink<ShapeRow> for CountingSink {
    fn report(&self, row: LeanWorkerTypedDataRow<ShapeRow>) {
        let mut metrics = self.metrics.lock().expect("metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics.checksum.saturating_add(row.payload.checksum());
    }
}

struct CancelAfterFirst<'a> {
    token: &'a LeanWorkerCancellationToken,
    rows: Mutex<u64>,
}

impl LeanWorkerTypedDataSink<ShapeRow> for CancelAfterFirst<'_> {
    fn report(&self, _row: LeanWorkerTypedDataRow<ShapeRow>) {
        let mut rows = self.rows.lock().expect("row lock is not poisoned");
        *rows = rows.saturating_add(1);
        drop(rows);
        self.token.cancel();
    }
}

fn run_stream(
    capability: &mut lean_rs_worker_parent::LeanWorkerCapability,
    export: &'static str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(export);
    let sink = CountingSink::default();
    let mut session = capability.open_session(None, None)?;
    let summary = session.run_streaming_command(&command, &ShapeRequest::default(), &sink, None, None, None)?;
    if !summary.metadata.as_ref().is_some_and(|metadata| metadata.ok) {
        return Err("stream did not return successful terminal metadata".into());
    }
    if summary.metadata.as_ref().map(|metadata| metadata.rows) != Some(summary.total_rows) {
        return Err("summary row count did not match terminal metadata".into());
    }
    if summary.total_rows != sink.count() {
        return Err("summary row count did not match observed rows".into());
    }
    Ok(summary.total_rows)
}

fn run_cancelled_stream(
    capability: &mut lean_rs_worker_parent::LeanWorkerCapability,
) -> Result<bool, Box<dyn std::error::Error>> {
    let token = LeanWorkerCancellationToken::new();
    let sink = CancelAfterFirst {
        token: &token,
        rows: Mutex::new(0),
    };
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_extract",
    );
    let mut session = capability.open_session(None, None)?;
    let err = session.run_streaming_command(&command, &ShapeRequest::default(), &sink, None, Some(&token), None);
    Ok(matches!(err, Err(LeanWorkerError::Cancelled { .. })))
}

fn run_fatal_stream(
    capability: &mut lean_rs_worker_parent::LeanWorkerCapability,
) -> Result<bool, Box<dyn std::error::Error>> {
    let sink = CountingSink::default();
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_panic_after_row",
    );
    let mut session = capability.open_session(None, None)?;
    let err = session.run_streaming_command(&command, &ShapeRequest::default(), &sink, None, None, None);
    Ok(matches!(err, Err(LeanWorkerError::ChildPanicOrAbort { .. })))
}

fn capability_builder() -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .metadata_export("lean_rs_interop_consumer_worker_shape_metadata")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_index")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_extract")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_panic_after_row")
    .worker_executable(worker_binary())
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn worker_binary() -> PathBuf {
    let workspace = workspace_root();
    let release = workspace.join("target").join("release").join("lean-rs-worker-child");
    if release.is_file() {
        release
    } else {
        workspace.join("target").join("debug").join("lean-rs-worker-child")
    }
}

fn interop_root() -> PathBuf {
    workspace_root().join("fixtures").join("interop-shims")
}

fn parent_rss_kib() -> Option<u64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps").args(["-o", "rss=", "-p", &pid]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}
