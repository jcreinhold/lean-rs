//! Allocation and RSS probe for large worker row streams.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p lean-rs-worker --example row_perf_probe
//! ```
//!
//! This probe measures the parent process. Child Lean allocations use the
//! child process allocator and are represented here by RSS samples, not dhat
//! heap counters.

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use lean_rs_worker_parent::{
    LeanWorkerCapabilityBuilder, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

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

#[derive(Debug, Serialize)]
struct FixtureRequest {
    source: String,
}

#[derive(Debug, Deserialize)]
struct FixtureRow {
    i: u64,
}

#[derive(Debug, Deserialize)]
struct LargeFixtureRow {
    blob: String,
}

#[derive(Debug, Deserialize)]
struct FixtureSummary {
    fixture: String,
    ok: bool,
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

impl LeanWorkerTypedDataSink<FixtureRow> for CountingSink {
    fn report(&self, row: LeanWorkerTypedDataRow<FixtureRow>) {
        let mut metrics = self.metrics.lock().expect("metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics.checksum.saturating_add(row.payload.i);
    }
}

#[derive(Default)]
struct LargeCountingSink {
    metrics: Mutex<SinkMetrics>,
}

impl LeanWorkerTypedDataSink<LargeFixtureRow> for LargeCountingSink {
    fn report(&self, row: LeanWorkerTypedDataRow<LargeFixtureRow>) {
        let mut metrics = self.metrics.lock().expect("metrics lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics
            .checksum
            .saturating_add(u64::try_from(row.payload.blob.len()).expect("blob length fits in u64"));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _profiler = dhat::Profiler::builder().testing().build();

    let mut capability = LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .worker_executable(worker_binary())
    .open()?;
    let parent_rss_before = parent_rss_kib();
    let child_rss_before = capability.worker_mut().rss_kib();
    let alloc_before = dhat::HeapStats::get();
    let started = Instant::now();

    let sink = CountingSink::default();
    let command = LeanWorkerStreamingCommand::<FixtureRequest, FixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream_many",
    );
    let summary = {
        let mut session = capability.open_session(None, None)?;
        session.run_streaming_command(
            &command,
            &FixtureRequest {
                source: "row-perf-probe".to_owned(),
            },
            &sink,
            None,
            None,
            None,
        )?
    };

    let elapsed = started.elapsed();
    let alloc_after = dhat::HeapStats::get();
    let child_rss_after = capability.worker_mut().rss_kib();
    let parent_rss_after = parent_rss_kib();
    let rows = summary.total_rows;
    let rows_per_second = if elapsed.as_secs_f64() == 0.0 {
        f64::INFINITY
    } else {
        rows as f64 / elapsed.as_secs_f64()
    };

    println!("workload=worker_data_stream_many rows={rows}");
    println!("elapsed_ms={:.3}", elapsed.as_secs_f64() * 1000.0);
    println!("rows_per_second={rows_per_second:.1}");
    println!(
        "parent_alloc_blocks={}",
        alloc_after.total_blocks.saturating_sub(alloc_before.total_blocks)
    );
    println!(
        "parent_alloc_bytes={}",
        alloc_after.total_bytes.saturating_sub(alloc_before.total_bytes)
    );
    println!("parent_rss_before_kib={parent_rss_before:?}");
    println!("parent_rss_after_kib={parent_rss_after:?}");
    println!("child_rss_before_kib={child_rss_before:?}");
    println!("child_rss_after_kib={child_rss_after:?}");
    println!(
        "summary_fixture={:?}",
        summary.metadata.as_ref().map(|metadata| &metadata.fixture)
    );
    println!("summary_ok={:?}", summary.metadata.as_ref().map(|metadata| metadata.ok));

    let parent_rss_before = parent_rss_kib();
    let child_rss_before = capability.worker_mut().rss_kib();
    let alloc_before = dhat::HeapStats::get();
    let started = Instant::now();
    let sink = LargeCountingSink::default();
    let command = LeanWorkerStreamingCommand::<FixtureRequest, LargeFixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream_large_payload",
    );
    let summary = {
        let mut session = capability.open_session(None, None)?;
        session.run_streaming_command(
            &command,
            &FixtureRequest {
                source: "row-perf-probe-large".to_owned(),
            },
            &sink,
            None,
            None,
            None,
        )?
    };

    let elapsed = started.elapsed();
    let alloc_after = dhat::HeapStats::get();
    let child_rss_after = capability.worker_mut().rss_kib();
    let parent_rss_after = parent_rss_kib();
    let rows = summary.total_rows;
    let rows_per_second = if elapsed.as_secs_f64() == 0.0 {
        f64::INFINITY
    } else {
        rows as f64 / elapsed.as_secs_f64()
    };

    println!("workload=worker_data_stream_large_payload rows={rows}");
    println!("elapsed_ms={:.3}", elapsed.as_secs_f64() * 1000.0);
    println!("rows_per_second={rows_per_second:.1}");
    println!(
        "parent_alloc_blocks={}",
        alloc_after.total_blocks.saturating_sub(alloc_before.total_blocks)
    );
    println!(
        "parent_alloc_bytes={}",
        alloc_after.total_bytes.saturating_sub(alloc_before.total_bytes)
    );
    println!("parent_rss_before_kib={parent_rss_before:?}");
    println!("parent_rss_after_kib={parent_rss_after:?}");
    println!("child_rss_before_kib={child_rss_before:?}");
    println!("child_rss_after_kib={child_rss_after:?}");
    println!(
        "summary_fixture={:?}",
        summary.metadata.as_ref().map(|metadata| &metadata.fixture)
    );
    println!("summary_ok={:?}", summary.metadata.as_ref().map(|metadata| metadata.ok));
    Ok(())
}
