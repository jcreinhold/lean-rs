#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lean_rs::LeanRuntime;
use lean_rs_host::{EvidenceStatus, LeanElabOptions, LeanHost};
use lean_rs_worker::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerConfig, LeanWorkerElabOptions, LeanWorkerError,
    LeanWorkerKernelStatus, LeanWorkerProgressEvent, LeanWorkerProgressSink, LeanWorkerSessionConfig,
};

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

fn fixture_root() -> PathBuf {
    workspace_root().join("fixtures").join("lean")
}

fn ensure_fixture_built() {
    let fixture = fixture_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsFixture").expect("fixture Lake target builds");
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
}

fn handles_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsFixture.Handles"],
    )
}

fn elaboration_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsHostShims.Elaboration"],
    )
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn direct_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_root()).expect("host opens cleanly")
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<LeanWorkerProgressEvent>>,
}

impl RecordingSink {
    fn events(&self) -> Vec<LeanWorkerProgressEvent> {
        self.events.lock().expect("progress lock is not poisoned").clone()
    }
}

impl LeanWorkerProgressSink for RecordingSink {
    fn report(&self, event: LeanWorkerProgressEvent) {
        self.events.lock().expect("progress lock is not poisoned").push(event);
    }
}

struct CancelOnFirstProgress<'a> {
    token: &'a LeanWorkerCancellationToken,
    events: Mutex<Vec<LeanWorkerProgressEvent>>,
}

impl LeanWorkerProgressSink for CancelOnFirstProgress<'_> {
    fn report(&self, event: LeanWorkerProgressEvent) {
        self.events.lock().expect("progress lock is not poisoned").push(event);
        self.token.cancel();
    }
}

#[test]
fn declaration_bulk_adapter_matches_in_process_session() {
    ensure_fixture_built();
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
        "This.Name.Does.Not.Exist",
    ];

    let host = direct_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("fixture capability loads");
    let mut direct = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("direct session opens");
    let direct_kinds = direct
        .declaration_kind_bulk(&names, None, None)
        .expect("direct kind bulk succeeds");
    let direct_names = direct
        .declaration_name_bulk(&names, None, None)
        .expect("direct name bulk succeeds");

    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");
    let worker_kinds = session
        .declaration_kinds(&names, None, None)
        .expect("worker kind bulk succeeds");
    let worker_names = session
        .declaration_names(&names, None, None)
        .expect("worker name bulk succeeds");

    assert_eq!(worker_kinds, direct_kinds);
    assert_eq!(worker_names, direct_names);
}

#[test]
fn elaboration_and_kernel_adapter_match_status_shape() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();

    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let ok = session
        .elaborate("(1 + 1 : Nat)", &opts, None, None)
        .expect("worker elaboration dispatch succeeds");
    let bad = session
        .elaborate("1 +", &opts, None, None)
        .expect("worker failed elaboration returns diagnostics");
    let checked = session
        .kernel_check("theorem lean_rs_worker_checked : 1 + 1 = 2 := rfl", &opts, None, None)
        .expect("worker kernel check dispatch succeeds");

    assert!(ok.success);
    assert!(!bad.success);
    assert!(!bad.diagnostics.is_empty(), "failed elaboration carries diagnostics");
    assert_eq!(checked.status, LeanWorkerKernelStatus::Checked);
    let summary = checked
        .summary
        .as_ref()
        .expect("Checked kernel result carries a proof summary");
    assert_eq!(summary.declaration_name, "lean_rs_worker_checked");
    assert_eq!(summary.kind, "theorem");
    assert!(
        summary.type_signature.contains("Nat") && summary.type_signature.contains("Eq"),
        "summary type_signature should mention the underlying Nat equality, got {:?}",
        summary.type_signature
    );

    let rejected = session
        .kernel_check("theorem lean_rs_worker_rejected : 1 + 1 = 3 := rfl", &opts, None, None)
        .expect("worker kernel check dispatch succeeds on rejected source");
    assert_eq!(rejected.status, LeanWorkerKernelStatus::Rejected);
    assert!(
        rejected.summary.is_none(),
        "Rejected kernel result must not carry a summary, got {:?}",
        rejected.summary
    );

    let host = direct_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("fixture capability loads");
    let mut direct = caps
        .session(&["LeanRsHostShims.Elaboration"], None, None)
        .expect("direct session opens");
    let direct_status = direct
        .kernel_check(
            "theorem lean_rs_worker_checked_direct : 1 + 1 = 2 := rfl",
            &LeanElabOptions::new(),
            None,
            None,
        )
        .expect("direct kernel check succeeds")
        .status();
    assert_eq!(direct_status, EvidenceStatus::Checked);
}

#[test]
fn progress_events_cross_worker_boundary_in_order() {
    ensure_fixture_built();
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];
    let sink = RecordingSink::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let kinds = session
        .declaration_kinds(&names, None, Some(&sink))
        .expect("worker kind bulk with progress succeeds");

    assert_eq!(kinds.len(), names.len());
    let events = sink.events();
    assert_eq!(events.len(), names.len());
    for (idx, event) in events.iter().enumerate() {
        assert_eq!(event.phase, "declaration_kind_bulk");
        assert_eq!(event.current, u64::try_from(idx + 1).expect("idx fits u64"));
        assert_eq!(event.total, Some(names.len() as u64));
    }
}

#[test]
fn parent_can_cancel_long_worker_request_at_progress_boundary() {
    ensure_fixture_built();
    let token = LeanWorkerCancellationToken::new();
    let sink = CancelOnFirstProgress {
        token: &token,
        events: Mutex::new(Vec::new()),
    };
    let names = vec!["LeanRsFixture.Handles.nameAnonymous"; 20_000];
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .declaration_kinds(&names, Some(&token), Some(&sink))
        .expect_err("parent cancellation should stop worker request");

    match err {
        LeanWorkerError::Cancelled { operation } => assert_eq!(operation, "worker_declaration_kinds"),
        other => panic!("expected worker cancellation, got {other:?}"),
    }
    assert!(
        !sink.events.lock().expect("progress lock is not poisoned").is_empty(),
        "cancellation happens after at least one worker progress event",
    );
}
