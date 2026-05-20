//! Worker data-row streaming example.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo run -p lean-rs-worker --example worker_streaming
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;

use lean_rs_worker::{LeanWorker, LeanWorkerConfig, LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerSessionConfig};
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
    let workspace = workspace_root();
    let interop_fixture = workspace.join("fixtures").join("interop-shims");
    lean_toolchain::build_lake_target_quiet(&interop_fixture, "LeanRsInteropConsumer")?;
    let worker_binary = ensure_worker_child_built(&workspace)?;

    let mut worker = LeanWorker::spawn(&LeanWorkerConfig::new(worker_binary))?;
    worker.health()?;
    println!("worker_status=started");

    let request = json!({
        "source": "worker_streaming_example",
        "limit": 3
    });

    let first = run_stream_once(&mut worker, &interop_fixture, &request, "initial")?;
    println!("initial_stream_rows={first}");

    worker.cycle()?;
    println!("worker_cycle_restarts={}", worker.stats().restarts);

    let second = run_stream_once(&mut worker, &interop_fixture, &request, "after_cycle")?;
    println!("post_cycle_stream_rows={second}");

    let exit = worker.terminate()?;
    println!("worker_exit_success={}", exit.success);
    println!("status=ok");
    Ok(())
}

fn run_stream_once(
    worker: &mut LeanWorker,
    interop_fixture: &Path,
    request: &Value,
    label: &'static str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let sink = JsonlSink::new(label);
    let config = LeanWorkerSessionConfig::new(
        interop_fixture,
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    );
    let mut session = worker.open_session(&config, None, None)?;
    let summary = session.run_data_stream(
        "lean_rs_interop_consumer_worker_data_stream",
        request,
        &sink,
        None,
        None,
        None,
    )?;

    let rows = sink.rows()?;
    if summary.total_rows != 2 || rows.len() != 2 {
        return Err(format!(
            "{label} stream expected 2 rows, got summary={} observed={}",
            summary.total_rows,
            rows.len()
        )
        .into());
    }
    if rows.first().map(|row| row.stream.as_str()) != Some("rows") {
        return Err(format!("{label} stream did not start with the rows stream").into());
    }
    Ok(summary.total_rows)
}

struct JsonlSink {
    label: &'static str,
    rows: Mutex<Vec<LeanWorkerDataRow>>,
}

impl JsonlSink {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            rows: Mutex::new(Vec::new()),
        }
    }

    fn rows(&self) -> Result<Vec<LeanWorkerDataRow>, Box<dyn std::error::Error>> {
        self.rows
            .lock()
            .map(|guard| guard.clone())
            .map_err(|_| "row sink mutex was poisoned".into())
    }
}

impl LeanWorkerDataSink for JsonlSink {
    fn report(&self, row: LeanWorkerDataRow) {
        let line = json!({
            "example_phase": self.label,
            "stream": row.stream,
            "sequence": row.sequence,
            "payload": row.payload,
        });
        println!("{line}");
        if let Ok(mut guard) = self.rows.lock() {
            guard.push(row);
        }
    }
}

fn ensure_worker_child_built(workspace: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cargo = option_env!("CARGO").unwrap_or("cargo");
    let status = Command::new(cargo)
        .current_dir(workspace)
        .args(["build", "-p", "lean-rs-worker", "--bin", "lean-rs-worker-child"])
        .status()?;
    if !status.success() {
        return Err("failed to build lean-rs-worker-child".into());
    }
    worker_binary_path(workspace)
}

fn worker_binary_path(workspace: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let executable = format!("lean-rs-worker-child{}", std::env::consts::EXE_SUFFIX);
    let current = std::env::current_exe()?;
    let profile_candidate = current
        .parent()
        .and_then(Path::parent)
        .map(|profile| profile.join(&executable));
    for candidate in [
        profile_candidate,
        Some(workspace.join("target").join("debug").join(&executable)),
        Some(workspace.join("target").join("release").join(&executable)),
    ]
    .into_iter()
    .flatten()
    {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("worker child binary was built but could not be found".into())
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
}
