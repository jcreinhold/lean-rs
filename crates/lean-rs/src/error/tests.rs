//! Invariant + display tests for the `error` module.
//!
//! The IO-result decoder tests live in [`crate::error::io::tests`]; the
//! panic-containment test lives here because it exercises only
//! `pub(crate)` items from [`super::panic`] and does not touch the Lean
//! runtime.

#![allow(clippy::expect_used, clippy::panic)]

use super::panic::catch_callback_panic;
use super::{HostStage, LEAN_ERROR_MESSAGE_LIMIT, LeanError, LeanExceptionKind};

#[test]
fn host_constructor_bounds_oversize_message() {
    let oversize = "x".repeat(LEAN_ERROR_MESSAGE_LIMIT + 1024);
    let LeanError::Host(host) = LeanError::host(HostStage::Internal, oversize) else {
        panic!("expected Host variant");
    };
    assert!(host.message().len() <= LEAN_ERROR_MESSAGE_LIMIT);
    assert_eq!(host.stage(), HostStage::Internal);
}

#[test]
fn host_constructor_passes_short_message_through() {
    let LeanError::Host(host) = LeanError::host(HostStage::Conversion, "ok") else {
        panic!("expected Host variant");
    };
    assert_eq!(host.message(), "ok");
    assert_eq!(host.stage(), HostStage::Conversion);
}

#[test]
fn lean_exception_constructor_bounds_oversize_message() {
    // A 4-byte char repeated past the limit; truncation must land on a
    // char boundary even if the cap falls inside a multibyte sequence.
    let oversize: String = "\u{1F600}".repeat(LEAN_ERROR_MESSAGE_LIMIT / 2);
    let LeanError::LeanException(exc) = LeanError::lean_exception(LeanExceptionKind::UserError, oversize) else {
        panic!("expected LeanException variant");
    };
    assert!(exc.message().len() <= LEAN_ERROR_MESSAGE_LIMIT);
    assert!(exc.message().is_char_boundary(exc.message().len()));
    assert_eq!(exc.kind(), LeanExceptionKind::UserError);
}

#[test]
fn lean_error_display_includes_stage_and_message() {
    let err = LeanError::host(HostStage::RuntimeInit, "boom");
    let rendered = err.to_string();
    assert!(rendered.starts_with("lean-rs:"), "got {rendered:?}");
    assert!(rendered.contains("RuntimeInit"), "got {rendered:?}");
    assert!(rendered.contains("boom"), "got {rendered:?}");
}

#[test]
fn lean_error_display_includes_kind_and_message() {
    let err = LeanError::lean_exception(LeanExceptionKind::UserError, "kaboom");
    let rendered = err.to_string();
    assert!(rendered.starts_with("lean-rs:"), "got {rendered:?}");
    assert!(rendered.contains("UserError"), "got {rendered:?}");
    assert!(rendered.contains("kaboom"), "got {rendered:?}");
}

#[test]
fn catch_callback_panic_returns_ok_when_closure_returns_ok() {
    let outcome: LeanError = match catch_callback_panic(|| Ok::<u32, _>(7)) {
        Ok(value) => {
            assert_eq!(value, 7);
            return;
        }
        Err(e) => e,
    };
    panic!("expected Ok, got {outcome:?}");
}

#[test]
fn catch_callback_panic_propagates_explicit_lean_error() {
    let err = catch_callback_panic(|| Err::<(), _>(LeanError::host(HostStage::Internal, "explicit")))
        .expect_err("closure returned Err; helper should pass it through");
    let LeanError::Host(host) = err else {
        panic!("expected Host");
    };
    assert_eq!(host.stage(), HostStage::Internal);
    assert_eq!(host.message(), "explicit");
}

#[test]
fn catch_callback_panic_converts_str_payload_into_callback_panic() {
    let err = catch_callback_panic::<_, ()>(|| panic!("boom")).expect_err("closure panicked");
    let LeanError::Host(host) = err else {
        panic!("expected Host");
    };
    assert_eq!(host.stage(), HostStage::CallbackPanic);
    assert!(host.message().contains("boom"), "got {:?}", host.message());
}

#[test]
fn catch_callback_panic_converts_string_payload_into_callback_panic() {
    let err = catch_callback_panic::<_, ()>(|| panic!("dynamic: {}", 42)).expect_err("closure panicked");
    let LeanError::Host(host) = err else {
        panic!("expected Host");
    };
    assert_eq!(host.stage(), HostStage::CallbackPanic);
    assert!(host.message().contains("dynamic: 42"), "got {:?}", host.message());
}

#[test]
fn catch_callback_panic_bounds_oversize_panic_payload() {
    let huge: String = "x".repeat(LEAN_ERROR_MESSAGE_LIMIT + 4096);
    let err = catch_callback_panic::<_, ()>(move || panic!("{huge}")).expect_err("closure panicked");
    let LeanError::Host(host) = err else {
        panic!("expected Host");
    };
    assert!(host.message().len() <= LEAN_ERROR_MESSAGE_LIMIT);
}
