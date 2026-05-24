//! Integration coverage for structured host progress.

#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::PathBuf;
use std::sync::Mutex;

use lean_rs::{HostStage, LeanDiagnosticCode, LeanError, LeanRuntime};
use lean_rs_host::{
    LeanCancellationToken, LeanCapabilities, LeanHost, LeanProgressEvent, LeanProgressSink, LeanSession,
};

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
}

fn session_over_handles<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.Handles"], None, None)
        .expect("session imports cleanly")
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<LeanProgressEvent>>,
}

impl RecordingSink {
    fn events(&self) -> Vec<LeanProgressEvent> {
        self.events.lock().expect("progress event lock is not poisoned").clone()
    }
}

impl LeanProgressSink for RecordingSink {
    fn report(&self, event: LeanProgressEvent) {
        self.events
            .lock()
            .expect("progress event lock is not poisoned")
            .push(event);
    }
}

struct CancelOnFirstEvent<'a> {
    token: &'a LeanCancellationToken,
    events: Mutex<Vec<LeanProgressEvent>>,
}

impl LeanProgressSink for CancelOnFirstEvent<'_> {
    fn report(&self, event: LeanProgressEvent) {
        self.events
            .lock()
            .expect("progress event lock is not poisoned")
            .push(event);
        if event.current >= 1 {
            self.token.cancel();
        }
    }
}

struct PanicSink;

impl LeanProgressSink for PanicSink {
    fn report(&self, event: LeanProgressEvent) {
        panic!("lean-rs progress sink deliberate panic at {}", event.current);
    }
}

fn assert_cancelled(err: LeanError) {
    assert_eq!(err.code(), LeanDiagnosticCode::Cancelled);
    match err {
        LeanError::Cancelled(_) => {}
        other @ (LeanError::LeanException(_) | LeanError::Host(_)) => {
            panic!("expected LeanError::Cancelled, got {other:?}");
        }
    }
}

#[test]
fn query_declarations_bulk_reports_ordered_multi_phase_progress() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);
    let sink = RecordingSink::default();
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let decls = session
        .query_declarations_bulk(&names, None, Some(&sink))
        .expect("progress bulk query succeeds");
    assert_eq!(decls.len(), names.len());

    let events = sink.events();
    assert_eq!(events.len(), names.len() * 2, "prepare + query phase per item");
    assert!(
        events
            .iter()
            .take(names.len())
            .all(|event| event.phase == "prepare_names"),
        "first phase prepares names: {events:?}",
    );
    assert!(
        events
            .iter()
            .skip(names.len())
            .all(|event| event.phase == "query_declarations_bulk"),
        "second phase queries declarations: {events:?}",
    );
    for (idx, event) in events.iter().take(names.len()).enumerate() {
        assert_eq!(
            event.current,
            u64::try_from(idx.saturating_add(1)).expect("idx fits u64")
        );
        assert_eq!(event.total, Some(names.len() as u64));
    }
    for (idx, event) in events.iter().skip(names.len()).enumerate() {
        assert_eq!(
            event.current,
            u64::try_from(idx.saturating_add(1)).expect("idx fits u64")
        );
        assert_eq!(event.total, Some(names.len() as u64));
    }
}

#[test]
fn progress_current_is_monotonic_within_each_phase() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);
    let sink = RecordingSink::default();
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
        "LeanRsFixture.Handles.nameToString",
    ];

    session
        .query_declarations_bulk(&names, None, Some(&sink))
        .expect("progress bulk query succeeds");

    let events = sink.events();
    for phase in ["prepare_names", "query_declarations_bulk"] {
        let mut last = 0;
        for event in events.iter().filter(|event| event.phase == phase) {
            assert!(
                event.current >= last,
                "phase {phase} regressed from {last} to {}",
                event.current,
            );
            last = event.current;
        }
    }
}

#[test]
fn pre_cancelled_progress_call_records_no_dispatch_or_event() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);
    let sink = RecordingSink::default();
    let token = LeanCancellationToken::new();
    token.cancel();

    let before = session.stats();
    let err = session
        .declaration_kind_bulk(&["LeanRsFixture.Handles.nameAnonymous"], Some(&token), Some(&sink))
        .expect_err("pre-cancelled progress call must fail");

    assert_cancelled(err);
    assert_eq!(session.stats(), before, "pre-cancelled call records no FFI");
    assert!(sink.events().is_empty(), "pre-cancelled call emits no progress");
}

#[test]
fn sink_triggered_cancellation_fires_at_next_progress_boundary() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);
    let token = LeanCancellationToken::new();
    let sink = CancelOnFirstEvent {
        token: &token,
        events: Mutex::new(Vec::new()),
    };
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let err = session
        .declaration_kind_bulk(&names, Some(&token), Some(&sink))
        .expect_err("sink-triggered cancellation must fail the operation");

    assert_cancelled(err);
    assert!(
        session.stats().ffi_calls > 0,
        "first item dispatched before cancellation"
    );
    let first_phase = {
        let events = sink.events.lock().expect("progress event lock is not poisoned");
        assert_eq!(events.len(), 1, "cancellation fires at the next loop boundary");
        events.first().expect("one progress event was recorded").phase
    };
    assert_eq!(first_phase, "declaration_kind_bulk");
}

#[test]
fn progress_sink_panic_is_contained_by_callback_trampoline() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let err = session
        .declaration_kind_bulk(&["LeanRsFixture.Handles.nameAnonymous"], None, Some(&PanicSink))
        .expect_err("progress sink panic must become a host error");

    assert_eq!(err.code(), LeanDiagnosticCode::Internal);
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::CallbackPanic);
            assert!(
                failure.message().contains("lean-rs progress sink deliberate panic"),
                "panic message should be preserved, got {:?}",
                failure.message(),
            );
        }
        other @ (LeanError::LeanException(_) | LeanError::Cancelled(_)) => {
            panic!("expected Host(CallbackPanic), got {other:?}");
        }
    }
}

#[test]
fn no_progress_bulk_path_remains_one_dispatch() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let before = session.stats();
    let kinds = session
        .declaration_kind_bulk(&names, None, None)
        .expect("no-progress bulk query succeeds");
    let after = session.stats();

    assert_eq!(kinds.len(), names.len());
    assert_eq!(after.ffi_calls - before.ffi_calls, 1);
    assert_eq!(after.batch_items - before.batch_items, names.len() as u64);
}
