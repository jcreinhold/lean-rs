#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lean_rs_worker::{
    LeanWorkerCapabilityBuilder, LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerStreamingCommand,
    LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(2));
    let builder = LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    );

    let command =
        LeanWorkerStreamingCommand::<Request, Row, Summary>::new("lean_rs_interop_consumer_worker_data_stream");
    let rows = RecordingRows::default();

    {
        let mut lease = pool.acquire_lease(builder.clone())?;
        let summary = lease.run_streaming_command(
            &command,
            &Request {
                source: "worker-pool-example".to_owned(),
            },
            &rows,
            None,
            None,
            None,
        )?;
        if let Some(metadata) = summary.metadata {
            println!(
                "first lease rows={} fixture={} ok={}",
                summary.total_rows, metadata.fixture, metadata.ok
            );
        }
    }

    {
        let mut lease = pool.acquire_lease(builder)?;
        lease.cycle()?;
    }

    println!(
        "pool workers={} captured_rows={:?}",
        pool.snapshot().workers,
        rows.rows()
    );
    Ok(())
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

#[derive(Debug, Serialize)]
struct Request {
    source: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Row {
    kind: String,
    ordinal: u64,
}

#[derive(Debug, Deserialize)]
struct Summary {
    fixture: String,
    ok: bool,
}

#[derive(Default)]
struct RecordingRows {
    rows: Mutex<Vec<LeanWorkerTypedDataRow<Row>>>,
}

impl RecordingRows {
    fn rows(&self) -> Vec<String> {
        self.rows
            .lock()
            .expect("rows lock is not poisoned")
            .iter()
            .map(|row| {
                format!(
                    "{}#{}:{}:{}",
                    row.stream, row.sequence, row.payload.kind, row.payload.ordinal
                )
            })
            .collect()
    }
}

impl LeanWorkerTypedDataSink<Row> for RecordingRows {
    fn report(&self, row: LeanWorkerTypedDataRow<Row>) {
        self.rows.lock().expect("rows lock is not poisoned").push(row);
    }
}
