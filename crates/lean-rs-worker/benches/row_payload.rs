#![allow(clippy::expect_used)]

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lean_rs_worker::{
    LeanWorkerCapabilityBuilder, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

const ROWS: usize = 512;
const LARGE_PAYLOAD_BYTES: usize = 4096;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn worker_binary() -> PathBuf {
    workspace_root()
        .join("target")
        .join("release")
        .join("lean-rs-worker-child")
}

fn interop_root() -> PathBuf {
    workspace_root().join("fixtures").join("interop-shims")
}

#[derive(Clone, Debug, Serialize)]
struct FixtureRequest {
    source: String,
}

#[derive(Clone, Debug, Deserialize)]
struct FixtureRow {
    i: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct FixtureSummary {
    fixture: String,
    ok: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ValueEnvelope {
    stream: String,
    payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawEnvelope {
    stream: String,
    payload: Box<RawValue>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ValueFrame {
    stream: String,
    sequence: u64,
    payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawFrame {
    stream: String,
    sequence: u64,
    payload: Box<RawValue>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawStringFrame {
    stream: String,
    sequence: u64,
    payload: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BytesFrame {
    stream: String,
    sequence: u64,
    payload: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawFrameBatch {
    rows: Vec<RawFrame>,
}

#[derive(Clone, Debug, Deserialize)]
struct LargeRow {
    i: u64,
    blob: String,
}

struct CountingTypedSink {
    rows: Mutex<u64>,
}

impl CountingTypedSink {
    fn new() -> Self {
        Self { rows: Mutex::new(0) }
    }
}

impl LeanWorkerTypedDataSink<FixtureRow> for CountingTypedSink {
    fn report(&self, row: LeanWorkerTypedDataRow<FixtureRow>) {
        let mut rows = self.rows.lock().expect("row counter lock is not poisoned");
        *rows = rows.saturating_add(row.payload.i);
    }
}

fn large_rows(count: usize) -> Vec<String> {
    let blob = "x".repeat(LARGE_PAYLOAD_BYTES);
    (0..count)
        .map(|i| {
            serde_json::to_string(&json!({
                "stream": "rows",
                "payload": {
                    "i": i,
                    "blob": blob,
                },
            }))
            .expect("large row serializes")
        })
        .collect()
}

fn value_roundtrip(rows: &[String]) -> u64 {
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: ValueEnvelope = serde_json::from_str(row).expect("value envelope parses");
        let frame = ValueFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload,
        };
        let bytes = serde_json::to_vec(&frame).expect("value frame serializes");
        let decoded: ValueFrame = serde_json::from_slice(&bytes).expect("value frame decodes");
        let payload: LargeRow = serde_json::from_value(decoded.payload).expect("payload decodes");
        checksum = checksum.saturating_add(payload.i);
        checksum = checksum.saturating_add(u64::try_from(payload.blob.len()).expect("blob length fits"));
    }
    checksum
}

fn raw_value_roundtrip(rows: &[String]) -> u64 {
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: RawEnvelope = serde_json::from_str(row).expect("raw envelope parses");
        let frame = RawFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload,
        };
        let bytes = serde_json::to_vec(&frame).expect("raw frame serializes");
        let decoded: RawFrame = serde_json::from_slice(&bytes).expect("raw frame decodes");
        let payload: LargeRow = serde_json::from_str(decoded.payload.get()).expect("payload decodes");
        checksum = checksum.saturating_add(payload.i);
        checksum = checksum.saturating_add(u64::try_from(payload.blob.len()).expect("blob length fits"));
    }
    checksum
}

fn raw_string_roundtrip(rows: &[String]) -> u64 {
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: RawEnvelope = serde_json::from_str(row).expect("raw envelope parses");
        let frame = RawStringFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload.get().to_owned(),
        };
        let bytes = serde_json::to_vec(&frame).expect("raw string frame serializes");
        let decoded: RawStringFrame = serde_json::from_slice(&bytes).expect("raw string frame decodes");
        let payload: LargeRow = serde_json::from_str(&decoded.payload).expect("payload decodes");
        checksum = checksum.saturating_add(payload.i);
        checksum = checksum.saturating_add(u64::try_from(payload.blob.len()).expect("blob length fits"));
    }
    checksum
}

fn owned_bytes_roundtrip(rows: &[String]) -> u64 {
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: RawEnvelope = serde_json::from_str(row).expect("raw envelope parses");
        let frame = BytesFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload.get().as_bytes().to_vec(),
        };
        let bytes = serde_json::to_vec(&frame).expect("bytes frame serializes");
        let decoded: BytesFrame = serde_json::from_slice(&bytes).expect("bytes frame decodes");
        let payload: LargeRow = serde_json::from_slice(&decoded.payload).expect("payload decodes");
        checksum = checksum.saturating_add(payload.i);
        checksum = checksum.saturating_add(u64::try_from(payload.blob.len()).expect("blob length fits"));
    }
    checksum
}

fn raw_value_per_row_protocol_roundtrip(rows: &[String]) -> u64 {
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: RawEnvelope = serde_json::from_str(row).expect("raw envelope parses");
        let frame = RawFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload,
        };
        let bytes = serde_json::to_vec(&frame).expect("raw frame serializes");
        let decoded: RawFrame = serde_json::from_slice(&bytes).expect("raw frame decodes");
        let payload: LargeRow = serde_json::from_str(decoded.payload.get()).expect("payload decodes");
        checksum = checksum.saturating_add(payload.i);
        checksum = checksum.saturating_add(u64::try_from(payload.blob.len()).expect("blob length fits"));
    }
    checksum
}

fn raw_value_batched_protocol_roundtrip(rows: &[String], batch_size: usize) -> u64 {
    let mut checksum = 0_u64;
    let mut batch = Vec::with_capacity(batch_size);
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: RawEnvelope = serde_json::from_str(row).expect("raw envelope parses");
        batch.push(RawFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload,
        });
        if batch.len() == batch_size {
            checksum = checksum.saturating_add(raw_value_batch_roundtrip(&mut batch));
        }
    }
    if !batch.is_empty() {
        checksum = checksum.saturating_add(raw_value_batch_roundtrip(&mut batch));
    }
    checksum
}

fn raw_value_batch_roundtrip(batch: &mut Vec<RawFrame>) -> u64 {
    let frame = RawFrameBatch {
        rows: std::mem::take(batch),
    };
    let bytes = serde_json::to_vec(&frame).expect("raw batch serializes");
    let decoded: RawFrameBatch = serde_json::from_slice(&bytes).expect("raw batch decodes");
    let mut checksum = 0_u64;
    for row in decoded.rows {
        let payload: LargeRow = serde_json::from_str(row.payload.get()).expect("payload decodes");
        checksum = checksum.saturating_add(payload.i);
        checksum = checksum.saturating_add(u64::try_from(payload.blob.len()).expect("blob length fits"));
    }
    checksum
}

fn bench_representation(c: &mut Criterion) {
    let rows = large_rows(ROWS);
    let mut group = c.benchmark_group("worker::row_payload::representation");
    group.throughput(Throughput::Elements(u64::try_from(ROWS).expect("row count fits")));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    group.bench_with_input(BenchmarkId::new("value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(value_roundtrip(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("raw_value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_value_roundtrip(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("raw_string_json_frame", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_string_roundtrip(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("owned_bytes_json_frame", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(owned_bytes_roundtrip(black_box(rows))));
    });
    group.finish();
}

fn bench_protocol_batching(c: &mut Criterion) {
    let rows = large_rows(ROWS);
    let mut group = c.benchmark_group("worker::row_payload::protocol_batching");
    group.throughput(Throughput::Elements(u64::try_from(ROWS).expect("row count fits")));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    group.bench_with_input(BenchmarkId::new("per_row_raw_value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_value_per_row_protocol_roundtrip(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("batch_16_raw_value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_value_batched_protocol_roundtrip(black_box(rows), 16)));
    });
    group.bench_with_input(BenchmarkId::new("batch_64_raw_value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_value_batched_protocol_roundtrip(black_box(rows), 64)));
    });
    group.finish();
}

fn bench_worker_stream(c: &mut Criterion) {
    let worker_path = worker_binary();
    if !worker_path.is_file() {
        eprintln!(
            "skipping worker stream bench: {} is missing; run `cargo build --release -p lean-rs-worker --bin lean-rs-worker-child` first",
            worker_path.display(),
        );
        return;
    }

    let command = LeanWorkerStreamingCommand::<FixtureRequest, FixtureRow, FixtureSummary>::new(
        "lean_rs_interop_consumer_worker_data_stream_many",
    );
    let request = FixtureRequest {
        source: "row-payload-bench".to_owned(),
    };
    let mut capability = LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .worker_executable(worker_path)
    .open()
    .expect("worker capability opens");

    let mut group = c.benchmark_group("worker::row_payload::stream");
    group.throughput(Throughput::Elements(u64::try_from(ROWS).expect("row count fits")));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(3));
    group.bench_function("typed_many_512", |b| {
        b.iter(|| {
            let sink = CountingTypedSink::new();
            let mut session = capability.open_session(None, None).expect("worker session opens");
            let summary = session
                .run_streaming_command(&command, &request, &sink, None, None, None)
                .expect("typed streaming command succeeds");
            black_box(summary.total_rows);
            if let Some(metadata) = summary.metadata {
                black_box(metadata.fixture);
                black_box(metadata.ok);
            }
        });
    });
    group.finish();
}

fn criterion_benchmarks(c: &mut Criterion) {
    bench_representation(c);
    bench_protocol_batching(c);
    bench_worker_stream(c);
}

criterion_group!(benches, criterion_benchmarks);
criterion_main!(benches);
