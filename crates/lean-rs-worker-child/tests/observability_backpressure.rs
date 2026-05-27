#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use lean_rs_worker_parent::{
    LeanWorkerCancellationToken, LeanWorkerError, LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerStreamingCommand,
    LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
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

fn builder() -> lean_rs_worker_parent::LeanWorkerCapabilityBuilder {
    lean_rs_worker_parent::LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_many")
    .streaming_command_export("lean_rs_interop_consumer_worker_data_stream_many_then_panic")
    .worker_executable(worker_binary())
}

#[derive(Debug, Serialize)]
struct FixtureRequest {
    source: String,
}

fn request(source: &str) -> FixtureRequest {
    FixtureRequest {
        source: source.to_owned(),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct ManyRow {
    i: u64,
}

fn many_command(export: &str) -> LeanWorkerStreamingCommand<FixtureRequest, ManyRow, Value> {
    LeanWorkerStreamingCommand::new(export)
}

#[derive(Default)]
struct RecordingRows {
    rows: Mutex<Vec<LeanWorkerTypedDataRow<ManyRow>>>,
}

impl RecordingRows {
    fn len(&self) -> usize {
        self.rows.lock().expect("rows lock is not poisoned").len()
    }
}

impl LeanWorkerTypedDataSink<ManyRow> for RecordingRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ManyRow>) {
        self.rows.lock().expect("rows lock is not poisoned").push(row);
    }
}

struct SlowRows {
    rows: Mutex<Vec<LeanWorkerTypedDataRow<ManyRow>>>,
    delay: Duration,
}

impl SlowRows {
    fn new(delay: Duration) -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
            delay,
        }
    }

    fn len(&self) -> usize {
        self.rows.lock().expect("rows lock is not poisoned").len()
    }
}

impl LeanWorkerTypedDataSink<ManyRow> for SlowRows {
    fn report(&self, row: LeanWorkerTypedDataRow<ManyRow>) {
        self.rows.lock().expect("rows lock is not poisoned").push(row);
        thread::sleep(self.delay);
    }
}

struct SlowCancelRows<'a> {
    token: &'a LeanWorkerCancellationToken,
    rows: Mutex<u64>,
    delay: Duration,
    cancel_after: u64,
}

impl LeanWorkerTypedDataSink<ManyRow> for SlowCancelRows<'_> {
    fn report(&self, _row: LeanWorkerTypedDataRow<ManyRow>) {
        let mut rows = self.rows.lock().expect("rows lock is not poisoned");
        *rows = rows.saturating_add(1);
        if *rows >= self.cancel_after {
            self.token.cancel();
        }
        drop(rows);
        thread::sleep(self.delay);
    }
}

#[test]
fn pool_and_lease_snapshots_record_stream_throughput() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
    let initial = lease.snapshot();
    assert_eq!(initial.active_workers, 1);
    assert_eq!(initial.warm_leases, 0);

    let rows = RecordingRows::default();
    let summary = lease
        .run_streaming_command(
            &many_command("lean_rs_interop_consumer_worker_data_stream_many"),
            &request("observability-throughput"),
            &rows,
            None,
            None,
            None,
        )
        .expect("many-row stream succeeds");

    assert_eq!(summary.total_rows, 512);
    assert_eq!(rows.len(), 512);
    let lease_snapshot = lease.snapshot();
    assert_eq!(lease_snapshot.stream_requests, 1);
    assert_eq!(lease_snapshot.stream_successes, 1);
    assert_eq!(lease_snapshot.stream_failures, 0);
    assert_eq!(lease_snapshot.data_rows_delivered, summary.total_rows);
    assert!(lease_snapshot.data_row_payload_bytes > 0);
    assert!(lease_snapshot.stream_elapsed > Duration::ZERO);
    drop(lease);

    let pool_snapshot = pool.snapshot();
    assert_eq!(pool_snapshot.active_workers, 0);
    assert_eq!(pool_snapshot.warm_leases, 1);
    assert_eq!(pool_snapshot.data_rows_delivered, summary.total_rows);
    assert_eq!(pool_snapshot.queue_depth, 0);
}

#[test]
fn slow_sink_applies_bounded_backpressure_without_losing_rows() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
    let rows = SlowRows::new(Duration::from_millis(2));
    let summary = lease
        .run_streaming_command(
            &many_command("lean_rs_interop_consumer_worker_data_stream_many"),
            &request("observability-slow-sink"),
            &rows,
            None,
            None,
            None,
        )
        .expect("slow sink stream succeeds under bounded backpressure");

    let snapshot = lease.snapshot();
    assert_eq!(summary.total_rows, 512);
    assert_eq!(rows.len(), 512);
    assert_eq!(snapshot.data_rows_delivered, 512);
    assert!(
        snapshot.backpressure_waits > 0,
        "slow sink should make the bounded reader wait at least once",
    );
    assert_eq!(snapshot.backpressure_failures, 0);
}

#[test]
fn cancellation_during_backpressure_invalidates_lease() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
    let token = LeanWorkerCancellationToken::new();
    let rows = SlowCancelRows {
        token: &token,
        rows: Mutex::new(0),
        delay: Duration::from_millis(2),
        cancel_after: 96,
    };

    let err = lease
        .run_streaming_command(
            &many_command("lean_rs_interop_consumer_worker_data_stream_many"),
            &request("observability-cancel"),
            &rows,
            None,
            Some(&token),
            None,
        )
        .expect_err("sink cancellation should stop a backpressured stream");

    match err {
        LeanWorkerError::Cancelled { operation } => assert_eq!(operation, "worker_run_data_stream"),
        other => panic!("expected cancellation, got {other:?}"),
    }
    let snapshot = lease.snapshot();
    assert!(!lease.is_valid());
    assert_eq!(snapshot.stream_failures, 1);
    assert_eq!(snapshot.cancelled_restarts, 1);
    assert!(snapshot.data_rows_delivered >= 96);
    assert!(snapshot.backpressure_waits > 0);
}

#[test]
fn fatal_exit_after_buffered_rows_records_failure_without_commit() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
    let rows = RecordingRows::default();

    let err = lease
        .run_streaming_command(
            &many_command("lean_rs_interop_consumer_worker_data_stream_many_then_panic"),
            &request("observability-fatal"),
            &rows,
            None,
            None,
            None,
        )
        .expect_err("fatal child exit should fail the stream");

    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => assert!(!exit.success),
        other => panic!("expected child panic/abort, got {other:?}"),
    }
    let snapshot = lease.snapshot();
    assert!(!lease.is_valid());
    assert!(rows.len() > 0, "rows before fatal exit remain tentative");
    assert_eq!(snapshot.stream_successes, 0);
    assert_eq!(snapshot.stream_failures, 1);
    assert_eq!(snapshot.data_rows_delivered as usize, rows.len());
}
