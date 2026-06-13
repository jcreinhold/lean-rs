#![allow(clippy::expect_used)]

use std::env;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerCapabilityBuilder, LeanWorkerConfig, LeanWorkerError,
    LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
    LeanWorkerTypedStreamSummary,
};
use lean_rs_worker_protocol::protocol::{
    MAX_FRAME_BYTES, Message, PROTOCOL_VERSION, Request, Response, read_frame, write_frame,
};
use serde::{Deserialize, Serialize};

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

fn capability_builder() -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .worker_executable(worker_binary())
    .validate_metadata(
        "lean_rs_interop_consumer_worker_shape_metadata",
        serde_json::json!({"source": "worker-capability-bench"}),
    )
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_index")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_extract")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_panic_after_row")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_mathlib_scale_index")
}

fn fake_worker_config(mode: &'static str) -> LeanWorkerConfig {
    LeanWorkerConfig::new(env::current_exe().expect("bench executable path is available"))
        .env("LEAN_RS_WORKER_CAPABILITY_BENCH_FAKE_CHILD", mode)
        .startup_timeout(Duration::from_secs(1))
        .shutdown_timeout(Duration::from_millis(20))
}

fn run_fake_child(mode: &str) {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    write_frame(
        &mut stdout,
        Message::Handshake {
            worker_version: "fake-worker-capability-bench".to_owned(),
            protocol_version: PROTOCOL_VERSION,
        },
        MAX_FRAME_BYTES,
    )
    .expect("fake child writes handshake");

    let frame_limit = {
        let mut stdin = io::stdin().lock();
        read_frame(&mut stdin, MAX_FRAME_BYTES).expect("fake child reads frame-limit")
    };
    match frame_limit.message {
        Message::ConfigureFrameLimit { .. } => {}
        other => panic!("expected frame-limit configuration, got {other:?}"),
    }

    loop {
        let frame = {
            let mut stdin = io::stdin().lock();
            let Ok(frame) = read_frame(&mut stdin, MAX_FRAME_BYTES) else {
                return;
            };
            frame
        };
        let Message::Request(request) = frame.message else {
            continue;
        };
        match request {
            Request::Terminate if mode == "terminate_hang" => loop {
                thread::sleep(Duration::from_mins(1));
            },
            Request::Terminate => {
                write_frame(&mut stdout, Message::Response(Response::Terminating), MAX_FRAME_BYTES)
                    .expect("fake child writes terminating response");
                stdout.flush().expect("fake child flushes terminating response");
                return;
            }
            other => {
                write_frame(
                    &mut stdout,
                    Message::Response(Response::Error {
                        code: "fake.unsupported".to_owned(),
                        message: format!("unsupported fake request: {other:?}"),
                    }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes unsupported response");
            }
        }
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
            workspace: "bench-workspace".to_owned(),
            modules: vec!["Fixture.Basic".to_owned()],
            limit: 512,
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
    rows: Mutex<SinkMetrics>,
}

#[derive(Default)]
struct SinkMetrics {
    count: u64,
    checksum: u64,
}

impl CountingSink {
    fn count(&self) -> u64 {
        self.rows.lock().expect("row lock is not poisoned").count
    }
}

impl LeanWorkerTypedDataSink<ShapeRow> for CountingSink {
    fn report(&self, row: LeanWorkerTypedDataRow<ShapeRow>) {
        let mut metrics = self.rows.lock().expect("row lock is not poisoned");
        metrics.count = metrics.count.saturating_add(1);
        metrics.checksum = metrics.checksum.saturating_add(row.payload.checksum());
        metrics.checksum = metrics.checksum.saturating_add(row.sequence);
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

fn skip_if_missing_worker() -> bool {
    let worker = worker_binary();
    if worker.is_file() {
        false
    } else {
        eprintln!(
            "skipping worker capability bench: {} is missing; run `cargo build --release -p lean-rs-worker --bin lean-rs-worker-child` first",
            worker.display(),
        );
        true
    }
}

fn run_stream(export: &'static str) -> Result<LeanWorkerTypedStreamSummary<ShapeSummary>, LeanWorkerError> {
    let mut capability = capability_builder().open()?;
    let mut session = capability.open_session(None, None)?;
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(export);
    let sink = CountingSink::default();
    let summary = session.run_streaming_command(&command, &ShapeRequest::default(), &sink, None, None, None)?;
    assert_eq!(summary.total_rows, sink.count());
    Ok(summary)
}

fn run_pool_stream(
    max_workers: usize,
    export: &'static str,
) -> Result<LeanWorkerTypedStreamSummary<ShapeSummary>, LeanWorkerError> {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(max_workers).max_total_child_rss_kib(u64::MAX));
    let mut lease = pool.acquire_lease(capability_builder())?;
    let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(export);
    let sink = CountingSink::default();
    let summary = lease.run_streaming_command(&command, &ShapeRequest::default(), &sink, None, None, None)?;
    assert_eq!(summary.total_rows, sink.count());
    Ok(summary)
}

fn bench_operational_shape(c: &mut Criterion) {
    if skip_if_missing_worker() {
        return;
    }

    let mut group = c.benchmark_group("worker::capability_shape");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("cold_startup_builder_open", |b| {
        b.iter(|| {
            let capability = capability_builder().open().expect("capability opens");
            let metadata_len = capability
                .validated_metadata()
                .map_or(0, |metadata| metadata.commands.len());
            std::hint::black_box(metadata_len);
        });
    });

    group.bench_function("first_import_open_session", |b| {
        b.iter(|| {
            let mut capability = capability_builder().open().expect("capability opens");
            let session = capability.open_session(None, None).expect("session opens");
            std::hint::black_box(session.request_timeout());
        });
    });

    group.throughput(Throughput::Elements(4));
    group.bench_with_input(BenchmarkId::new("import_once_stream", "index"), &(), |b, ()| {
        b.iter(|| {
            let summary = run_stream("lean_rs_interop_consumer_worker_shape_index").expect("index stream succeeds");
            std::hint::black_box(summary.total_rows);
            std::hint::black_box(summary.metadata.map(|metadata| (metadata.ok, metadata.rows)));
        });
    });

    group.throughput(Throughput::Elements(2));
    group.bench_with_input(BenchmarkId::new("row_throughput", "extract"), &(), |b, ()| {
        b.iter(|| {
            let summary = run_stream("lean_rs_interop_consumer_worker_shape_extract").expect("extract stream succeeds");
            std::hint::black_box(summary.total_rows);
        });
    });

    group.bench_function("worker_cycle", |b| {
        b.iter(|| {
            let mut capability = capability_builder().open().expect("capability opens");
            capability.cycle().expect("worker cycle succeeds");
            std::hint::black_box(capability.stats().restarts);
        });
    });

    group.bench_function("graceful_shutdown", |b| {
        b.iter(|| {
            let capability = capability_builder().open().expect("capability opens");
            let report = capability.shutdown().expect("worker shutdown succeeds");
            std::hint::black_box(report.outcome);
            std::hint::black_box(report.exit.success);
        });
    });

    group.bench_function("terminate_timeout_kill", |b| {
        b.iter(|| {
            let worker = LeanWorker::spawn(&fake_worker_config("terminate_hang")).expect("fake worker starts");
            let report = worker
                .shutdown()
                .expect("terminate-hung fake worker is killed and reaped");
            std::hint::black_box(report.outcome);
            std::hint::black_box(report.exit.success);
        });
    });

    group.bench_function("fatal_exit_recovery", |b| {
        b.iter(|| {
            let mut capability = capability_builder().open().expect("capability opens");
            let sink = CountingSink::default();
            let err = {
                let mut session = capability.open_session(None, None).expect("session opens");
                let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
                    "lean_rs_interop_consumer_worker_shape_panic_after_row",
                );
                session
                    .run_streaming_command(&command, &ShapeRequest::default(), &sink, None, None, None)
                    .expect_err("panic stream should fail")
            };
            std::hint::black_box(matches!(err, LeanWorkerError::ChildPanicOrAbort { .. }));
        });
    });

    group.bench_function("cancellation_latency", |b| {
        b.iter_custom(|iters| {
            let started = Instant::now();
            for _ in 0..iters {
                let mut capability = capability_builder().open().expect("capability opens");
                let token = LeanWorkerCancellationToken::new();
                let sink = CancelAfterFirst {
                    token: &token,
                    rows: Mutex::new(0),
                };
                let mut session = capability.open_session(None, None).expect("session opens");
                let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
                    "lean_rs_interop_consumer_worker_shape_extract",
                );
                let err = session
                    .run_streaming_command(&command, &ShapeRequest::default(), &sink, None, Some(&token), None)
                    .expect_err("cancelled stream should fail");
                assert!(matches!(err, LeanWorkerError::Cancelled { .. }));
            }
            started.elapsed()
        });
    });

    group.throughput(Throughput::Elements(47));
    group.bench_function("mathlib_scale_single_worker_pool", |b| {
        b.iter(|| {
            let summary = run_pool_stream(1, "lean_rs_interop_consumer_worker_shape_mathlib_scale_index")
                .expect("mathlib-scale pool stream succeeds");
            std::hint::black_box(summary.total_rows);
        });
    });

    group.throughput(Throughput::Elements(47));
    group.bench_function("mathlib_scale_pool_max_2", |b| {
        b.iter(|| {
            let summary = run_pool_stream(2, "lean_rs_interop_consumer_worker_shape_mathlib_scale_index")
                .expect("mathlib-scale pool stream succeeds");
            std::hint::black_box(summary.total_rows);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_operational_shape);

fn main() {
    if let Ok(mode) = env::var("LEAN_RS_WORKER_CAPABILITY_BENCH_FAKE_CHILD") {
        run_fake_child(&mode);
        return;
    }
    benches();
}
