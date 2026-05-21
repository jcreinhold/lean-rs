#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use lean_rs_worker::{
    LeanWorkerCancellationToken, LeanWorkerCapabilityBuilder, LeanWorkerCapabilityMetadata, LeanWorkerCommandMetadata,
    LeanWorkerError, LeanWorkerJsonCommand, LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerRestartPolicy,
    LeanWorkerRestartReason, LeanWorkerStreamingCommand, LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

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

fn builder() -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .worker_executable(worker_binary())
}

fn distinct_valid_builder() -> LeanWorkerCapabilityBuilder {
    builder().restart_policy(LeanWorkerRestartPolicy::default().max_requests(99))
}

#[derive(Debug, Serialize)]
struct FixtureRequest {
    source: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct FixtureResponse {
    accepted: bool,
    kind: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct LooseRow {
    kind: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct FixtureSummary {
    fixture: String,
    ok: bool,
}

#[derive(Default)]
struct RecordingLooseRows {
    rows: Mutex<Vec<LeanWorkerTypedDataRow<LooseRow>>>,
    cancel_after_first: Option<LeanWorkerCancellationToken>,
}

impl RecordingLooseRows {
    fn with_cancellation(token: LeanWorkerCancellationToken) -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
            cancel_after_first: Some(token),
        }
    }

    fn rows(&self) -> Vec<LeanWorkerTypedDataRow<LooseRow>> {
        self.rows.lock().expect("rows lock is not poisoned").clone()
    }
}

impl LeanWorkerTypedDataSink<LooseRow> for RecordingLooseRows {
    fn report(&self, row: LeanWorkerTypedDataRow<LooseRow>) {
        self.rows.lock().expect("rows lock is not poisoned").push(row);
        if let Some(token) = &self.cancel_after_first {
            token.cancel();
        }
    }
}

fn json_command() -> LeanWorkerJsonCommand<FixtureRequest, FixtureResponse> {
    LeanWorkerJsonCommand::new("lean_rs_interop_consumer_worker_json_command")
}

fn loose_stream_command(export: &str) -> LeanWorkerStreamingCommand<FixtureRequest, LooseRow, FixtureSummary> {
    LeanWorkerStreamingCommand::new(export)
}

fn request(source: &str) -> FixtureRequest {
    FixtureRequest {
        source: source.to_owned(),
    }
}

#[test]
fn compatible_session_key_reuses_one_worker() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(2));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens first lease");
        let response = lease
            .run_json_command(&json_command(), &request("pool-reuse-1"), None, None)
            .expect("typed command succeeds");
        assert_eq!(response.kind, "fixture");
    }

    assert_eq!(pool.snapshot().workers, 1);

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool reuses compatible lease");
        let response = lease
            .run_json_command(&json_command(), &request("pool-reuse-2"), None, None)
            .expect("typed command succeeds after reuse");
        assert!(response.accepted);
    }

    assert_eq!(pool.snapshot().workers, 1);
}

#[test]
fn distinct_session_key_respects_fixed_pool_limit() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens first lease");
        let response = lease
            .run_json_command(&json_command(), &request("pool-capacity"), None, None)
            .expect("typed command succeeds");
        assert!(response.accepted);
    }

    let different_imports = LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback", "LeanRsInteropConsumer.Extra"],
    )
    .worker_executable(worker_binary());

    let err = pool
        .acquire_lease(different_imports)
        .expect_err("fixed-size pool should reject a second distinct key");
    match err {
        LeanWorkerError::WorkerPoolExhausted { max_workers } => assert_eq!(max_workers, 1),
        other => panic!("expected pool exhaustion, got {other:?}"),
    }
}

#[test]
fn child_fatal_exit_invalidates_lease_and_next_acquire_replaces_worker() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let sink = RecordingLooseRows::default();

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
        let err = lease
            .run_streaming_command(
                &loose_stream_command("lean_rs_interop_consumer_worker_data_stream_row_then_panic"),
                &request("pool-fatal"),
                &sink,
                None,
                None,
                None,
            )
            .expect_err("fatal child exit should fail the lease command");
        match err {
            LeanWorkerError::ChildPanicOrAbort { exit } => {
                assert!(!exit.success, "fatal stream should kill only the child");
            }
            other => panic!("expected child panic/abort, got {other:?}"),
        }
        assert!(!lease.is_valid(), "fatal child exit invalidates the lease");
    }

    let mut replacement = pool
        .acquire_lease(builder())
        .expect("pool replaces the dead compatible worker");
    let response = replacement
        .run_json_command(&json_command(), &request("pool-after-fatal"), None, None)
        .expect("typed command succeeds after replacement");
    assert!(response.accepted);
    assert_eq!(
        pool.snapshot().last_restart_reason,
        None,
        "crash replacement should not be reported as a policy restart",
    );
}

#[test]
fn cancellation_invalidates_current_lease_but_future_lease_can_continue() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let token = LeanWorkerCancellationToken::new();
    let sink = RecordingLooseRows::with_cancellation(token.clone());

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
        let err = lease
            .run_streaming_command(
                &loose_stream_command("lean_rs_interop_consumer_worker_data_stream_slow_after_row"),
                &request("pool-cancel"),
                &sink,
                None,
                Some(&token),
                None,
            )
            .expect_err("row sink cancellation should stop the request");
        match err {
            LeanWorkerError::Cancelled { .. } => {}
            other => panic!("expected cancellation, got {other:?}"),
        }
        assert_eq!(sink.rows().len(), 1);
        assert!(
            matches!(
                lease.run_json_command(&json_command(), &request("pool-invalidated"), None, None),
                Err(LeanWorkerError::LeaseInvalidated { .. })
            ),
            "same lease should be invalid after cancellation",
        );
    }

    let mut lease = pool
        .acquire_lease(builder())
        .expect("fresh lease opens after cancellation cycle");
    let response = lease
        .run_json_command(&json_command(), &request("pool-after-cancel"), None, None)
        .expect("typed command succeeds after fresh lease");
    assert!(response.accepted);
}

#[test]
fn timeout_invalidates_current_lease_but_future_lease_can_continue() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let sink = RecordingLooseRows::default();

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
        lease
            .set_request_timeout(Duration::from_millis(150))
            .expect("lease timeout can be set after session startup");
        let err = lease
            .run_streaming_command(
                &loose_stream_command("lean_rs_interop_consumer_worker_data_stream_slow_after_row"),
                &request("pool-timeout"),
                &sink,
                None,
                None,
                None,
            )
            .expect_err("slow stream should time out");
        match err {
            LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "worker_run_data_stream"),
            other => panic!("expected timeout, got {other:?}"),
        }
        assert!(
            matches!(
                lease.run_json_command(&json_command(), &request("pool-invalidated"), None, None),
                Err(LeanWorkerError::LeaseInvalidated { .. })
            ),
            "same lease should be invalid after timeout",
        );
    }

    let mut lease = pool
        .acquire_lease(builder())
        .expect("fresh lease opens after timeout cycle");
    let response = lease
        .run_json_command(&json_command(), &request("pool-after-timeout"), None, None)
        .expect("fast typed command succeeds after timeout cycle");
    assert!(response.accepted);
}

#[test]
fn explicit_cycle_invalidates_only_the_current_lease() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
        lease.cycle().expect("explicit cycle succeeds");
        assert!(
            matches!(
                lease.run_json_command(&json_command(), &request("pool-invalidated"), None, None),
                Err(LeanWorkerError::LeaseInvalidated { .. })
            ),
            "same lease should be invalid after explicit cycle",
        );
    }

    let mut lease = pool
        .acquire_lease(builder())
        .expect("fresh lease opens after explicit cycle");
    let response = lease
        .run_json_command(&json_command(), &request("pool-after-cycle"), None, None)
        .expect("typed command succeeds after fresh lease");
    assert!(response.accepted);
}

#[test]
fn metadata_mismatch_is_typed() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let wrong_metadata = LeanWorkerCapabilityMetadata {
        commands: vec![LeanWorkerCommandMetadata {
            name: "wrong".to_owned(),
            version: "0".to_owned(),
        }],
        capabilities: Vec::new(),
        lean_version: None,
        extra: None,
    };

    let err = pool
        .acquire_lease(builder().expect_metadata(
            "lean_rs_interop_consumer_worker_metadata",
            json!({"caller": "pool-metadata-mismatch"}),
            wrong_metadata,
        ))
        .expect_err("metadata mismatch should be typed");

    match err {
        LeanWorkerError::CapabilityMetadataMismatch { export, .. } => {
            assert_eq!(export, "lean_rs_interop_consumer_worker_metadata");
        }
        other => panic!("expected metadata mismatch, got {other:?}"),
    }
}

#[test]
fn memory_budget_rejects_new_distinct_worker_when_known_rss_is_exhausted() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(2).max_total_child_rss_kib(1));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens first lease");
        let response = lease
            .run_json_command(&json_command(), &request("pool-rss-budget"), None, None)
            .expect("typed command succeeds");
        assert!(response.accepted);
    }

    match pool.acquire_lease(distinct_valid_builder()) {
        Err(LeanWorkerError::WorkerPoolMemoryBudgetExceeded { current_kib, limit_kib }) => {
            assert_eq!(limit_kib, 1);
            assert!(current_kib >= limit_kib);
            assert_eq!(pool.snapshot().memory_budget_rejections, 1);
        }
        Ok(_lease) => {
            assert!(
                pool.snapshot().rss_samples_unavailable > 0,
                "budget admission should only proceed when RSS samples are unavailable",
            );
        }
        Err(other) => panic!("expected memory budget error or RSS-unavailable admission, got {other:?}"),
    }
}

#[test]
fn queue_wait_timeout_is_typed_when_pool_is_full() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).queue_wait_timeout(Duration::from_millis(10)));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens first lease");
        let response = lease
            .run_json_command(&json_command(), &request("pool-queue-timeout"), None, None)
            .expect("typed command succeeds");
        assert!(response.accepted);
    }

    let err = pool
        .acquire_lease(distinct_valid_builder())
        .expect_err("full pool should wait only until the configured queue timeout");
    match err {
        LeanWorkerError::WorkerPoolQueueTimeout { waited } => assert_eq!(waited, Duration::from_millis(10)),
        other => panic!("expected queue timeout, got {other:?}"),
    }
    assert_eq!(pool.snapshot().queue_timeouts, 1);
}

#[test]
fn per_worker_rss_policy_invalidates_old_lease_before_work() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).per_worker_rss_ceiling_kib(1));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
        let err = lease
            .run_json_command(&json_command(), &request("pool-memory-cycle"), None, None)
            .expect_err("low RSS ceiling should cycle before assigning work");
        match err {
            LeanWorkerError::LeaseInvalidated { reason } => {
                assert!(reason.contains("memory policy"), "unexpected reason: {reason}");
            }
            other => panic!("expected lease invalidation, got {other:?}"),
        }
    }

    let snapshot = pool.snapshot();
    assert_eq!(snapshot.policy_restarts, 1);
    assert!(matches!(
        snapshot.last_restart_reason,
        Some(LeanWorkerRestartReason::RssCeiling { limit_kib: 1, .. })
    ));
}

#[test]
fn idle_policy_invalidates_lease_and_fresh_lease_can_continue() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).idle_cycle_after(Duration::from_millis(1)));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
        thread::sleep(Duration::from_millis(15));
        let err = lease
            .run_json_command(&json_command(), &request("pool-idle-cycle"), None, None)
            .expect_err("idle cycle should invalidate the stale lease before work");
        match err {
            LeanWorkerError::LeaseInvalidated { reason } => {
                assert!(reason.contains("idle policy"), "unexpected reason: {reason}");
            }
            other => panic!("expected lease invalidation, got {other:?}"),
        }
    }

    let snapshot = pool.snapshot();
    assert_eq!(snapshot.policy_restarts, 1);
    assert!(matches!(
        snapshot.last_restart_reason,
        Some(LeanWorkerRestartReason::Idle { .. })
    ));

    let mut lease = pool
        .acquire_lease(builder())
        .expect("fresh lease opens after idle policy cycle");
    let response = lease
        .run_json_command(&json_command(), &request("pool-after-idle-cycle"), None, None)
        .expect("typed command succeeds after fresh lease");
    assert!(response.accepted);
}

#[test]
fn rss_snapshot_records_available_samples_on_supported_platforms() {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(2).max_total_child_rss_kib(u64::MAX));

    {
        let mut lease = pool.acquire_lease(builder()).expect("pool opens lease");
        let response = lease
            .run_json_command(&json_command(), &request("pool-rss-snapshot"), None, None)
            .expect("typed command succeeds");
        assert!(response.accepted);
    }

    let _lease = pool
        .acquire_lease(distinct_valid_builder())
        .expect("second lease admission samples existing worker RSS");
    let snapshot = pool.snapshot();
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert!(
            snapshot.total_child_rss_kib.is_some(),
            "supported RSS platforms should record a total sample",
        );
    } else {
        assert!(
            snapshot.rss_samples_unavailable > 0,
            "unsupported RSS platforms should record unavailable samples",
        );
    }
}
