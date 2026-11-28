//! Integration tests for the cooperative cancellation contract.
//!
//! Cancellation is a public host-stack behavior, so these tests exercise
//! it through `LeanCapabilities` and `LeanSession` rather than through
//! the private check helper.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/fixed_fixture.rs"]
mod fixture;

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lean_rs::{LeanDiagnosticCode, LeanError, LeanRuntime};
use lean_rs_host::{LeanCancellationToken, LeanHost};

fn fixture_lake_root() -> PathBuf {
    fixture::native_fixture_root()
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
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = caps
        .session(&fixture::native_imports(&caps, "LeanRsFixture.Handles"), None, None)
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
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = caps
        .session(&fixture::native_imports(&caps, "LeanRsFixture.Handles"), None, None)
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
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = caps
        .session(&fixture::native_imports(&caps, "LeanRsFixture.Handles"), None, None)
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
