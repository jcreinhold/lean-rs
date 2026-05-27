//! Invariant + display tests for the `error` module.
//!
//! The IO-result decoder tests live in [`crate::error::io::tests`]; the
//! panic-containment test lives here because it exercises only
//! `pub(crate)` items from [`super::panic`] and does not touch the Lean
//! runtime.

#![allow(clippy::expect_used, clippy::panic)]

use super::panic::catch_callback_panic;
use super::{HostStage, LEAN_ERROR_MESSAGE_LIMIT, LeanDiagnosticCode, LeanError, LeanExceptionKind};

#[test]
fn host_constructor_bounds_oversize_message() {
    let oversize = "x".repeat(LEAN_ERROR_MESSAGE_LIMIT + 1024);
    // Any live `LeanError::Host(...)` constructor goes through the
    // shared bounding helper; this test asserts the bound, not the
    // stage, so use a constructor with real production callers
    // (`abi_conversion` is on the every-FFI-decode path).
    let LeanError::Host(host) = LeanError::abi_conversion(oversize) else {
        panic!("expected Host variant");
    };
    assert!(host.message().len() <= LEAN_ERROR_MESSAGE_LIMIT);
    assert_eq!(host.stage(), HostStage::Conversion);
    assert_eq!(host.code(), LeanDiagnosticCode::AbiConversion);
}

#[test]
fn host_constructor_passes_short_message_through() {
    let LeanError::Host(host) = LeanError::abi_conversion("ok") else {
        panic!("expected Host variant");
    };
    assert_eq!(host.message(), "ok");
    assert_eq!(host.stage(), HostStage::Conversion);
    assert_eq!(host.code(), LeanDiagnosticCode::AbiConversion);
}

#[test]
fn lean_error_code_projects_from_variant() {
    assert_eq!(LeanError::linking("x").code(), LeanDiagnosticCode::Linking);
    assert_eq!(LeanError::module_init("x").code(), LeanDiagnosticCode::ModuleInit);
    assert_eq!(LeanError::symbol_lookup("x").code(), LeanDiagnosticCode::SymbolLookup);
    assert_eq!(LeanError::runtime_init("x").code(), LeanDiagnosticCode::RuntimeInit);
    assert_eq!(
        LeanError::lean_exception(LeanExceptionKind::UserError, "x").code(),
        LeanDiagnosticCode::LeanException,
    );
}

#[test]
fn diagnostic_code_as_str_is_stable() {
    assert_eq!(LeanDiagnosticCode::RuntimeInit.as_str(), "lean_rs.runtime_init");
    assert_eq!(LeanDiagnosticCode::Linking.as_str(), "lean_rs.linking");
    assert_eq!(LeanDiagnosticCode::ModuleInit.as_str(), "lean_rs.module_init");
    assert_eq!(LeanDiagnosticCode::SymbolLookup.as_str(), "lean_rs.symbol_lookup");
    assert_eq!(LeanDiagnosticCode::AbiConversion.as_str(), "lean_rs.abi_conversion");
    assert_eq!(LeanDiagnosticCode::LeanException.as_str(), "lean_rs.lean_exception");
    assert_eq!(LeanDiagnosticCode::Elaboration.as_str(), "lean_rs.elaboration");
    assert_eq!(LeanDiagnosticCode::Unsupported.as_str(), "lean_rs.unsupported");
    assert_eq!(LeanDiagnosticCode::Internal.as_str(), "lean_rs.internal");
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
    let err = LeanError::runtime_init("boom");
    let rendered = err.to_string();
    assert!(rendered.starts_with("lean-rs:"), "got {rendered:?}");
    assert!(rendered.contains("RuntimeInit"), "got {rendered:?}");
    assert!(rendered.contains("boom"), "got {rendered:?}");
    assert!(
        rendered.contains("lean_rs.runtime_init"),
        "expected diagnostic code in render, got {rendered:?}"
    );
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
    let err = catch_callback_panic(|| Err::<(), _>(LeanError::abi_conversion("explicit")))
        .expect_err("closure returned Err; helper should pass it through");
    let LeanError::Host(host) = err else {
        panic!("expected Host");
    };
    assert_eq!(host.stage(), HostStage::Conversion);
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

/// Regression: a long run of caught panics must not leak state between
/// invocations. Each call is supposed to be independent—the helper
/// holds no global registry, no thread-local payload buffer, no
/// accumulating counter—and a leak would surface either as a
/// monotonic growth in resident memory under the sanitizer job or as a
/// flaky assertion if the message-bounding side-channel shared state.
#[test]
fn catch_callback_panic_loop_remains_independent() {
    // The default loops a few hundred times to keep stable `cargo test`
    // cheap; the sanitizer CI job overrides this to push the count into
    // five digits so AddressSanitizer / LeakSanitizer has enough surface
    // to surface a regression. Reusing the same env var as the runtime
    // refcount stress tests keeps the operator vocabulary small.
    let iters = std::env::var("LEAN_RS_REFCOUNT_STRESS_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(512);
    for i in 0..iters {
        let err = catch_callback_panic::<_, ()>(move || panic!("iter {i}")).expect_err("closure panicked");
        let LeanError::Host(host) = err else {
            panic!("expected Host");
        };
        assert_eq!(host.stage(), HostStage::CallbackPanic);
        // The per-iteration message must reflect *this* iteration's
        // payload; any cross-iteration bleed would show up here.
        let expected = format!("iter {i}");
        assert!(
            host.message().contains(&expected),
            "iteration {i} produced unrelated payload {:?}",
            host.message(),
        );
    }
}

/// Regression: a closure that panics while holding `Box<dyn Display>`
/// (a non-`&'static str`, non-`String` payload) must still surface a
/// best-effort message rather than crash the catch path. The current
/// implementation falls back to a synthesised description when the
/// payload is not one of the well-known shapes; this test pins that
/// behaviour so future panic-payload extraction changes do not silently
/// regress to a less informative error.
#[test]
fn catch_callback_panic_handles_non_string_payload() {
    struct DisplayMarker;
    impl std::fmt::Display for DisplayMarker {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("display-marker")
        }
    }

    let err = catch_callback_panic::<_, ()>(|| std::panic::panic_any(DisplayMarker))
        .expect_err("panic_any with custom payload should still be caught");
    let LeanError::Host(host) = err else {
        panic!("expected Host");
    };
    assert_eq!(host.stage(), HostStage::CallbackPanic);
    // We do not assert on the exact wording—the catch path is only
    // contractually required to produce *some* bounded message—but
    // the message must be non-empty so a downstream reader can tell
    // a panic occurred at all.
    assert!(
        !host.message().is_empty(),
        "non-string panic payloads must still surface a non-empty diagnostic",
    );
    assert!(host.message().len() <= LEAN_ERROR_MESSAGE_LIMIT);
}
