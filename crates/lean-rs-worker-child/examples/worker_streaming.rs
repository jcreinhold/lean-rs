//! Worker data-row streaming example.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo run -p lean-rs-worker --example worker_streaming
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use lean_rs_worker_parent::{
    LeanWorkerCapability, LeanWorkerCapabilityBuilder, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow,
    LeanWorkerTypedDataSink,
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
    let workspace = workspace_root();
    let interop_fixture = workspace.join("fixtures").join("interop-shims");

    let mut capability = LeanWorkerCapabilityBuilder::new(
        &interop_fixture,
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .validate_metadata(
        "lean_rs_interop_consumer_worker_metadata",
        json!({"source": "worker_streaming_example"}),
    )
    .open()?;
    println!("worker_status=started");
    println!(
        "worker_runtime_version={}",
        capability.runtime_metadata().worker_version
    );
    println!(
        "capability_commands={}",
        capability
            .validated_metadata()
            .map_or(0, |metadata| metadata.commands.len())
    );

    let request = ExampleRequest {
        source: "worker_streaming_example".to_owned(),
        limit: 3,
    };

    let first = run_stream_once(&mut capability, &request, "initial")?;
    println!("initial_stream_rows={first}");

    capability.worker_mut().cycle()?;
    println!("worker_cycle_restarts={}", capability.worker().stats().restarts);

    let second = run_stream_once(&mut capability, &request, "after_cycle")?;
    println!("post_cycle_stream_rows={second}");

    let exit = capability.terminate()?;
    println!("worker_exit_success={}", exit.success);
    println!("status=ok");
    Ok(())
}

fn run_stream_once(
    capability: &mut LeanWorkerCapability,
    request: &ExampleRequest,
    label: &'static str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let sink = JsonlSink::new(label);
    let mut session = capability.open_session(None, None)?;
    let doctor = session.capability_doctor(
        "lean_rs_interop_consumer_worker_doctor",
        &json!({"source": "worker_streaming_example"}),
        None,
        None,
    )?;
    println!("doctor_diagnostics={}", doctor.diagnostics.len());
    let command = LeanWorkerStreamingCommand::<ExampleRequest, ExampleRow, ExampleSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream",
    );
    let summary = session.run_streaming_command(&command, request, &sink, None, None, None)?;

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
    if !summary.metadata.as_ref().is_some_and(|metadata| metadata.ok) {
        return Err(format!("{label} stream did not return successful terminal metadata").into());
    }
    Ok(summary.total_rows)
}

#[derive(Debug, Serialize)]
struct ExampleRequest {
    source: String,
    limit: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct ExampleRow {
    kind: String,
    ordinal: u64,
}

#[derive(Debug, Deserialize)]
struct ExampleSummary {
    #[allow(dead_code)]
    fixture: String,
    ok: bool,
}

struct JsonlSink {
    label: &'static str,
    rows: Mutex<Vec<LeanWorkerTypedDataRow<ExampleRow>>>,
}

impl JsonlSink {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            rows: Mutex::new(Vec::new()),
        }
    }

    fn rows(&self) -> Result<Vec<LeanWorkerTypedDataRow<ExampleRow>>, Box<dyn std::error::Error>> {
        self.rows
            .lock()
            .map(|guard| guard.clone())
            .map_err(|_| "row sink mutex was poisoned".into())
    }
}

impl LeanWorkerTypedDataSink<ExampleRow> for JsonlSink {
    fn report(&self, row: LeanWorkerTypedDataRow<ExampleRow>) {
        let line = json!({
            "example_phase": self.label,
            "stream": &row.stream,
            "sequence": row.sequence,
            "payload": {
                "kind": &row.payload.kind,
                "ordinal": row.payload.ordinal,
            },
        });
        println!("{line}");
        if let Ok(mut guard) = self.rows.lock() {
            guard.push(row);
        }
    }
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
}
