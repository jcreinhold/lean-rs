#![allow(clippy::expect_used)]

use std::hint::black_box;
use std::io::{Cursor, Read as _};
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
const SMALL_ROWS: usize = 8192;

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
struct TypedEnvelope<T> {
    stream: String,
    payload: T,
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
struct TypedFrame<T> {
    stream: String,
    sequence: u64,
    payload: T,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawFrameBatch {
    rows: Vec<RawFrame>,
}

trait RowChecksum {
    fn checksum(&self) -> u64;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SmallRow {
    i: u64,
    name: String,
    kind: String,
}

impl RowChecksum for SmallRow {
    fn checksum(&self) -> u64 {
        self.i
            .saturating_add(u64::try_from(self.name.len()).expect("name length fits in u64"))
            .saturating_add(u64::try_from(self.kind.len()).expect("kind length fits in u64"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LargeRow {
    i: u64,
    blob: String,
}

impl RowChecksum for LargeRow {
    fn checksum(&self) -> u64 {
        self.i
            .saturating_add(u64::try_from(self.blob.len()).expect("blob length fits in u64"))
    }
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

fn small_rows(count: usize) -> Vec<String> {
    (0..count)
        .map(|i| {
            serde_json::to_string(&json!({
                "stream": if i % 2 == 0 { "declarations" } else { "features" },
                "payload": {
                    "i": i,
                    "name": format!("Fixture.decl_{i}"),
                    "kind": if i % 2 == 0 { "theorem" } else { "definition" },
                },
            }))
            .expect("small row serializes")
        })
        .collect()
}

fn value_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
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
        let payload: T = serde_json::from_value(decoded.payload).expect("payload decodes");
        checksum = checksum.saturating_add(payload.checksum());
    }
    checksum
}

fn raw_value_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
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
        let payload: T = serde_json::from_str(decoded.payload.get()).expect("payload decodes");
        checksum = checksum.saturating_add(payload.checksum());
    }
    checksum
}

fn raw_string_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
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
        let payload: T = serde_json::from_str(&decoded.payload).expect("payload decodes");
        checksum = checksum.saturating_add(payload.checksum());
    }
    checksum
}

fn owned_bytes_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
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
        let payload: T = serde_json::from_slice(&decoded.payload).expect("payload decodes");
        checksum = checksum.saturating_add(payload.checksum());
    }
    checksum
}

fn raw_value_per_row_protocol_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
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
        let payload: T = serde_json::from_str(decoded.payload.get()).expect("payload decodes");
        checksum = checksum.saturating_add(payload.checksum());
    }
    checksum
}

fn raw_value_batched_protocol_roundtrip<T>(rows: &[String], batch_size: usize) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
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
            checksum = checksum.saturating_add(raw_value_batch_roundtrip::<T>(&mut batch));
        }
    }
    if !batch.is_empty() {
        checksum = checksum.saturating_add(raw_value_batch_roundtrip::<T>(&mut batch));
    }
    checksum
}

fn raw_value_batch_roundtrip<T>(batch: &mut Vec<RawFrame>) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
    let frame = RawFrameBatch {
        rows: std::mem::take(batch),
    };
    let bytes = serde_json::to_vec(&frame).expect("raw batch serializes");
    let decoded: RawFrameBatch = serde_json::from_slice(&bytes).expect("raw batch decodes");
    let mut checksum = 0_u64;
    for row in decoded.rows {
        let payload: T = serde_json::from_str(row.payload.get()).expect("payload decodes");
        checksum = checksum.saturating_add(payload.checksum());
    }
    checksum
}

fn binary_json_payload_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + for<'de> Deserialize<'de>,
{
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: RawEnvelope = serde_json::from_str(row).expect("raw envelope parses");
        let frame = encode_binary_frame(
            &envelope.stream,
            u64::try_from(sequence).expect("sequence fits"),
            envelope.payload.get().as_bytes(),
        );
        let (_stream, _sequence, payload) = decode_binary_frame(&frame);
        let payload: T = serde_json::from_slice(&payload).expect("payload decodes");
        checksum = checksum.saturating_add(payload.checksum());
    }
    checksum
}

fn messagepack_typed_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + Serialize + for<'de> Deserialize<'de>,
{
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: TypedEnvelope<T> = serde_json::from_str(row).expect("typed envelope parses");
        let frame = TypedFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload,
        };
        let bytes = rmp_serde::to_vec(&frame).expect("messagepack frame serializes");
        let decoded: TypedFrame<T> = rmp_serde::from_slice(&bytes).expect("messagepack frame decodes");
        checksum = checksum.saturating_add(decoded.payload.checksum());
    }
    checksum
}

fn cbor_typed_roundtrip<T>(rows: &[String]) -> u64
where
    T: RowChecksum + Serialize + for<'de> Deserialize<'de>,
{
    let mut checksum = 0_u64;
    for (sequence, row) in rows.iter().enumerate() {
        let envelope: TypedEnvelope<T> = serde_json::from_str(row).expect("typed envelope parses");
        let frame = TypedFrame {
            stream: envelope.stream,
            sequence: u64::try_from(sequence).expect("sequence fits"),
            payload: envelope.payload,
        };
        let mut bytes = Vec::new();
        ciborium::into_writer(&frame, &mut bytes).expect("cbor frame serializes");
        let decoded: TypedFrame<T> = ciborium::from_reader(bytes.as_slice()).expect("cbor frame decodes");
        checksum = checksum.saturating_add(decoded.payload.checksum());
    }
    checksum
}

fn encode_binary_frame(stream: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let stream_len = u32::try_from(stream.len()).expect("stream length fits in u32");
    let payload_len = u32::try_from(payload.len()).expect("payload length fits in u32");
    let capacity = 4_usize
        .saturating_add(stream.len())
        .saturating_add(8)
        .saturating_add(4)
        .saturating_add(payload.len());
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(&stream_len.to_be_bytes());
    bytes.extend_from_slice(stream.as_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&payload_len.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn decode_binary_frame(bytes: &[u8]) -> (String, u64, Vec<u8>) {
    let mut cursor = Cursor::new(bytes);
    let stream_len = read_u32(&mut cursor) as usize;
    let mut stream = vec![0_u8; stream_len];
    cursor.read_exact(&mut stream).expect("stream bytes read");
    let sequence = read_u64(&mut cursor);
    let payload_len = read_u32(&mut cursor) as usize;
    let mut payload = vec![0_u8; payload_len];
    cursor.read_exact(&mut payload).expect("payload bytes read");
    (String::from_utf8(stream).expect("stream is utf-8"), sequence, payload)
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> u32 {
    let mut bytes = [0_u8; 4];
    cursor.read_exact(&mut bytes).expect("u32 bytes read");
    u32::from_be_bytes(bytes)
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> u64 {
    let mut bytes = [0_u8; 8];
    cursor.read_exact(&mut bytes).expect("u64 bytes read");
    u64::from_be_bytes(bytes)
}

fn bench_representation(c: &mut Criterion) {
    let rows = large_rows(ROWS);
    let mut group = c.benchmark_group("worker::row_payload::representation");
    group.throughput(Throughput::Elements(u64::try_from(ROWS).expect("row count fits")));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    group.bench_with_input(BenchmarkId::new("value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(value_roundtrip::<LargeRow>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("raw_value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_value_roundtrip::<LargeRow>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("raw_string_json_frame", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_string_roundtrip::<LargeRow>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("owned_bytes_json_frame", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(owned_bytes_roundtrip::<LargeRow>(black_box(rows))));
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
        b.iter(|| black_box(raw_value_per_row_protocol_roundtrip::<LargeRow>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("batch_16_raw_value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_value_batched_protocol_roundtrip::<LargeRow>(black_box(rows), 16)));
    });
    group.bench_with_input(BenchmarkId::new("batch_64_raw_value", ROWS), &rows, |b, rows| {
        b.iter(|| black_box(raw_value_batched_protocol_roundtrip::<LargeRow>(black_box(rows), 64)));
    });
    group.finish();
}

fn bench_data_plane_formats(c: &mut Criterion) {
    bench_data_plane_format_shape::<SmallRow>(c, "small_rows_8192", &small_rows(SMALL_ROWS));
    bench_data_plane_format_shape::<LargeRow>(c, "large_rows_512", &large_rows(ROWS));
}

fn bench_data_plane_format_shape<T>(c: &mut Criterion, shape: &str, rows: &[String])
where
    T: RowChecksum + Serialize + for<'de> Deserialize<'de>,
{
    eprintln!("{}", format_size_line::<T>(shape, rows));
    let mut group = c.benchmark_group(format!("worker::row_payload::data_plane/{shape}"));
    group.throughput(Throughput::Elements(u64::try_from(rows.len()).expect("row count fits")));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    group.bench_with_input(BenchmarkId::new("serde_json_value", rows.len()), rows, |b, rows| {
        b.iter(|| black_box(value_roundtrip::<T>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("raw_json", rows.len()), rows, |b, rows| {
        b.iter(|| black_box(raw_value_roundtrip::<T>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("batched_raw_json_64", rows.len()), rows, |b, rows| {
        b.iter(|| black_box(raw_value_batched_protocol_roundtrip::<T>(black_box(rows), 64)));
    });
    group.bench_with_input(BenchmarkId::new("binary_json_payload", rows.len()), rows, |b, rows| {
        b.iter(|| black_box(binary_json_payload_roundtrip::<T>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("messagepack_typed", rows.len()), rows, |b, rows| {
        b.iter(|| black_box(messagepack_typed_roundtrip::<T>(black_box(rows))));
    });
    group.bench_with_input(BenchmarkId::new("cbor_typed", rows.len()), rows, |b, rows| {
        b.iter(|| black_box(cbor_typed_roundtrip::<T>(black_box(rows))));
    });
    group.finish();
}

fn format_size_line<T>(shape: &str, rows: &[String]) -> String
where
    T: RowChecksum + Serialize + for<'de> Deserialize<'de>,
{
    let first = rows.first().expect("benchmark shape has at least one row");
    let raw_envelope: RawEnvelope = serde_json::from_str(first).expect("raw envelope parses");
    let typed_envelope: TypedEnvelope<T> = serde_json::from_str(first).expect("typed envelope parses");
    let value_envelope: ValueEnvelope = serde_json::from_str(first).expect("value envelope parses");
    let value_frame = ValueFrame {
        stream: value_envelope.stream,
        sequence: 0,
        payload: value_envelope.payload,
    };
    let raw_frame = RawFrame {
        stream: raw_envelope.stream.clone(),
        sequence: 0,
        payload: raw_envelope.payload,
    };
    let typed_frame = TypedFrame {
        stream: typed_envelope.stream,
        sequence: 0,
        payload: typed_envelope.payload,
    };
    let binary = encode_binary_frame(
        &raw_frame.stream,
        raw_frame.sequence,
        raw_frame.payload.get().as_bytes(),
    );
    let mut cbor = Vec::new();
    ciborium::into_writer(&typed_frame, &mut cbor).expect("cbor frame serializes");
    let messagepack = rmp_serde::to_vec(&typed_frame).expect("messagepack frame serializes");
    let value_json = serde_json::to_vec(&value_frame).expect("value frame serializes");
    let raw_json = serde_json::to_vec(&raw_frame).expect("raw frame serializes");
    format!(
        "data_plane_size shape={shape} value_json={} raw_json={} binary_json_payload={} messagepack={} cbor={}",
        value_json.len(),
        raw_json.len(),
        binary.len(),
        messagepack.len(),
        cbor.len(),
    )
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
    bench_data_plane_formats(c);
    bench_worker_stream(c);
}

criterion_group!(benches, criterion_benchmarks);
criterion_main!(benches);
