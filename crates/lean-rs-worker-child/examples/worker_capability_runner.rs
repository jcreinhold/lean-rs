//! Worker capability runner example.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo run -p lean-rs-worker --example worker_capability_runner
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Duration;

use lean_rs_worker_parent::{
    LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT, LeanWorkerCapability, LeanWorkerCapabilityBuilder, LeanWorkerDiagnosticEvent,
    LeanWorkerDiagnosticSink, LeanWorkerError, LeanWorkerProgressEvent, LeanWorkerProgressSink,
    LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
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
    let mut capability = capability_builder()
        .validate_metadata(
            "lean_rs_interop_consumer_worker_shape_metadata",
            json!({"source": "worker_capability_runner"}),
        )
        .open()?;

    let metadata = capability
        .validated_metadata()
        .ok_or("builder did not validate metadata")?;
    println!(
        "metadata commands={} capabilities={}",
        metadata.commands.len(),
        metadata.capabilities.len()
    );

    let summary = run_index(&mut capability, "initial")?;
    println!(
        "terminal total_rows={} streams={}",
        summary.total_rows,
        summary.per_stream_counts.len()
    );

    demonstrate_timeout(&mut capability)?;

    capability.worker_mut().cycle()?;
    println!("cycle restarts={}", capability.worker().stats().restarts);

    let post_cycle = run_index(&mut capability, "post_cycle")?;
    println!("post_cycle total_rows={}", post_cycle.total_rows);

    println!("status=ok");
    Ok(())
}

fn run_index(
    capability: &mut LeanWorkerCapability,
    label: &'static str,
) -> Result<lean_rs_worker_parent::LeanWorkerTypedStreamSummary<ShapeSummary>, Box<dyn std::error::Error>> {
    let rows = RecordingRows::new(label);
    let diagnostics = RecordingDiagnostics::new(label);
    let progress = RecordingProgress::new(label);
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_index",
    );

    let mut session = capability.open_session(None, Some(&progress))?;
    let summary = session.run_streaming_command(
        &command,
        &ShapeRequest::default(),
        &rows,
        Some(&diagnostics),
        None,
        Some(&progress),
    )?;

    let observed_rows = rows.rows()?;
    if observed_rows.len() != 4 {
        return Err(format!("{label} expected 4 typed rows, got {}", observed_rows.len()).into());
    }
    if diagnostics.diagnostics()? < 2 {
        return Err(format!("{label} expected at least two diagnostics").into());
    }
    if progress.events()? < 2 {
        return Err(format!("{label} expected progress events").into());
    }
    if summary.total_rows != observed_rows.len() as u64 {
        return Err(format!("{label} terminal summary did not match observed rows").into());
    }
    let Some(metadata) = summary.metadata.as_ref() else {
        return Err(format!("{label} terminal metadata was missing").into());
    };
    if !metadata.ok || metadata.command != "index" || metadata.fixture != "lean-dup-shaped" {
        return Err(format!("{label} terminal metadata was not successful").into());
    }
    if metadata.rows != summary.total_rows {
        return Err(format!("{label} terminal metadata row count did not match").into());
    }
    Ok(summary)
}

fn demonstrate_timeout(capability: &mut LeanWorkerCapability) -> Result<(), Box<dyn std::error::Error>> {
    let rows = RecordingRows::new("timeout");
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
        "lean_rs_interop_consumer_worker_shape_timeout_after_row",
    );
    let mut session = capability.open_session(None, None)?;
    session.set_request_timeout(Duration::from_millis(50));
    let err = session.run_streaming_command(&command, &ShapeRequest::default(), &rows, None, None, None);
    match err {
        Err(LeanWorkerError::Timeout { operation, duration }) => {
            println!("timeout operation={operation} duration_ms={}", duration.as_millis());
            capability
                .worker_mut()
                .set_request_timeout(LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT);
            Ok(())
        }
        Err(other) => Err(format!("expected timeout, got {other:?}").into()),
        Ok(summary) => Err(format!("expected timeout, got success with {} rows", summary.total_rows).into()),
    }
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
            workspace: "recipe-workspace".to_owned(),
            modules: vec!["Fixture.Basic".to_owned()],
            limit: 8,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
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

impl ShapeRow {
    fn describe(&self) -> String {
        match self {
            Self::Declaration { module, name, ordinal } => {
                format!("declaration:{module}.{name}:{ordinal}")
            }
            Self::Feature {
                module,
                name,
                feature,
                score,
                ordinal,
            } => format!("feature:{module}.{name}:{feature}:{score}:{ordinal}"),
            Self::Probe {
                left,
                right,
                relation,
                ordinal,
            } => format!("probe:{left}:{right}:{relation}:{ordinal}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ShapeSummary {
    fixture: String,
    command: String,
    ok: bool,
    rows: u64,
}

struct RecordingRows {
    label: &'static str,
    rows: Mutex<Vec<LeanWorkerTypedDataRow<ShapeRow>>>,
}

impl RecordingRows {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            rows: Mutex::new(Vec::new()),
        }
    }

    fn rows(&self) -> Result<Vec<LeanWorkerTypedDataRow<ShapeRow>>, Box<dyn std::error::Error>> {
        self.rows
            .lock()
            .map(|guard| guard.clone())
            .map_err(|_| "row sink mutex was poisoned".into())
    }
}

impl LeanWorkerTypedDataSink<ShapeRow> for RecordingRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ShapeRow>) {
        println!(
            "row label={} stream={} sequence={} payload={}",
            self.label,
            row.stream,
            row.sequence,
            row.payload.describe()
        );
        if let Ok(mut guard) = self.rows.lock() {
            guard.push(row);
        }
    }
}

struct RecordingDiagnostics {
    label: &'static str,
    count: Mutex<u64>,
}

impl RecordingDiagnostics {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            count: Mutex::new(0),
        }
    }

    fn diagnostics(&self) -> Result<u64, Box<dyn std::error::Error>> {
        self.count
            .lock()
            .map(|guard| *guard)
            .map_err(|_| "diagnostic sink mutex was poisoned".into())
    }
}

impl LeanWorkerDiagnosticSink for RecordingDiagnostics {
    fn report(&self, diagnostic: LeanWorkerDiagnosticEvent) {
        println!(
            "diagnostic label={} code={} message={}",
            self.label, diagnostic.code, diagnostic.message
        );
        if let Ok(mut guard) = self.count.lock() {
            *guard = guard.saturating_add(1);
        }
    }
}

struct RecordingProgress {
    label: &'static str,
    count: Mutex<u64>,
}

impl RecordingProgress {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            count: Mutex::new(0),
        }
    }

    fn events(&self) -> Result<u64, Box<dyn std::error::Error>> {
        self.count
            .lock()
            .map(|guard| *guard)
            .map_err(|_| "progress sink mutex was poisoned".into())
    }
}

impl LeanWorkerProgressSink for RecordingProgress {
    fn report(&self, event: LeanWorkerProgressEvent) {
        println!(
            "progress label={} phase={} current={} total={:?}",
            self.label, event.phase, event.current, event.total
        );
        if let Ok(mut guard) = self.count.lock() {
            *guard = guard.saturating_add(1);
        }
    }
}

fn capability_builder() -> LeanWorkerCapabilityBuilder {
    let workspace = workspace_root();
    LeanWorkerCapabilityBuilder::new(
        workspace.join("fixtures").join("interop-shims"),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .metadata_export("lean_rs_interop_consumer_worker_shape_metadata")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_index")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_timeout_after_row")
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
}
