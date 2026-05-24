//! Integration tests for the cooperative cancellation contract.
//!
//! Cancellation is a public host-stack behavior, so these tests exercise
//! it through `LeanCapabilities`, `LeanSession`, and `SessionPool` rather
//! than through the private check helper.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lean_rs::{LeanDiagnosticCode, LeanError, LeanRuntime};
use lean_rs_host::{LeanCancellationToken, LeanHost, SessionPool};

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

fn assert_cancelled(err: LeanError) {
    assert_eq!(err.code(), LeanDiagnosticCode::Cancelled);
    match err {
        LeanError::Cancelled(cancelled) => {
            assert!(
                cancelled.message().contains("cancelled"),
                "cancelled payload should name the condition, got {:?}",
                cancelled.message(),
            );
        }
        LeanError::Host(failure) => panic!("expected LeanError::Cancelled, got Host {failure:?}"),
        LeanError::LeanException(exc) => panic!("expected LeanError::Cancelled, got LeanException {exc:?}"),
    }
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn cancellation_token_is_send_sync() {
    assert_send_sync::<LeanCancellationToken>();
}

#[test]
fn pre_cancelled_session_call_returns_cancelled_without_ffi() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("session imports cleanly");
    let token = LeanCancellationToken::new();
    token.cancel();

    let baseline = session.stats();
    let err = session
        .query_declaration("LeanRsFixture.Handles.nameAnonymous", Some(&token))
        .expect_err("pre-cancelled token should stop before dispatch");

    assert_cancelled(err);
    assert_eq!(
        session.stats(),
        baseline,
        "pre-cancelled calls must not record an FFI dispatch",
    );
}

#[test]
fn never_cancelled_token_preserves_successful_call() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("session imports cleanly");
    let token = LeanCancellationToken::new();

    let kind = session
        .declaration_kind("LeanRsFixture.Handles.nameAnonymous", Some(&token))
        .expect("uncancelled token should preserve normal success");

    assert_eq!(kind, "definition");
    assert!(!token.is_cancelled());
}

#[test]
fn bulk_query_observes_cancellation_from_another_thread() {
    const ITEMS: usize = 100_000;

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("session imports cleanly");
    let names = vec!["LeanRsFixture.Handles.nameAnonymous"; ITEMS];
    let token = LeanCancellationToken::new();
    let canceller = token.clone();
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(2));
        let cancelled_at = Instant::now();
        canceller.cancel();
        tx.send(cancelled_at).expect("parent still receives cancellation time");
    });

    let err = session
        .query_declarations_bulk(&names, Some(&token), None)
        .expect_err("bulk loop should observe cancellation between per-name dispatches");
    let observed_at = Instant::now();
    handle.join().expect("canceller thread exits cleanly");
    let cancelled_at = rx.recv().expect("canceller sent timestamp");

    assert_cancelled(err);
    assert!(
        session.stats().ffi_calls > 0,
        "canceller sleeps briefly so the worker should complete at least one FFI dispatch first",
    );
    assert!(
        observed_at.duration_since(cancelled_at) < Duration::from_secs(2),
        "cooperative cancellation should be observed at the next per-item check",
    );
}

#[test]
fn pool_acquire_cancelled_before_import_does_not_update_stats() {
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let pool = SessionPool::with_capacity(runtime, 1);
    let token = LeanCancellationToken::new();
    token.cancel();

    let err = pool
        .acquire(&caps, &["LeanRsFixture.Handles"], Some(&token), None)
        .expect_err("pre-cancelled acquire should not import");

    assert_cancelled(err);
    assert_eq!(
        pool.stats().imports_performed,
        0,
        "pre-cancelled acquire must not call Lean.importModules",
    );
    assert_eq!(pool.stats().acquired, 0, "pre-cancelled acquire is not an acquire");
}
