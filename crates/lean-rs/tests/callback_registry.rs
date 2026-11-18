//! Public callback registry tests.
//!
//! These tests reuse the Lean callback-loop export from the trampoline
//! spike, but the Rust side goes through the public RAII registry
//! instead of passing a stack pointer and test-local trampoline.

#![allow(unsafe_code, clippy::expect_used, clippy::panic)]

#[path = "../examples/support/callback_fixture.rs"]
mod callback_fixture;
use std::sync::{Arc, Mutex};

use lean_rs::abi::traits::IntoLean;
use lean_rs::module::{LeanIo, LeanLibrary};
use lean_rs::{
    HostStage, LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanDiagnosticCode, LeanError,
    LeanProgressCallback, LeanProgressTick, LeanRuntime, LeanStringEvent,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SeenEvent {
    current: u64,
    total: u64,
}

impl From<LeanProgressTick> for SeenEvent {
    fn from(value: LeanProgressTick) -> Self {
        Self {
            current: value.current,
            total: value.total,
        }
    }
}

fn callback_loop<'lean, 'lib>(
    library: &'lib LeanLibrary<'lean>,
) -> lean_rs::LeanExported<'lean, 'lib, (usize, usize, u64), LeanIo<u8>> {
    let module = library
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    unsafe { module.exported_unchecked::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop") }
        .expect("callback loop export resolves")
}

fn string_callback_loop<'lean, 'lib>(
    library: &'lib LeanLibrary<'lean>,
) -> lean_rs::LeanExported<'lean, 'lib, (usize, usize, Vec<String>), LeanIo<u8>> {
    let module = library
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    unsafe {
        module.exported_unchecked::<(usize, usize, Vec<String>), LeanIo<u8>>(
            "lean_rs_interop_consumer_string_callback_loop",
        )
    }
    .expect("string callback loop export resolves")
}

#[test]
fn registered_callback_runs_through_typed_lean_export() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = callback_loop(bundle.library());
    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&events);
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(move |event| {
        callback_events
            .lock()
            .expect("callback events lock is not poisoned")
            .push(SeenEvent::from(event));
        LeanCallbackFlow::Continue
    })
    .expect("callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    // SAFETY: The live registry registration supplies the exact trampoline and handle for this synchronous export.
    let status = unsafe { callback_loop.call(handle, trampoline, 4) }.expect("callback loop returns");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Ok),);
    assert!(callback.last_error().is_none());
    assert_eq!(
        events.lock().expect("callback events lock is not poisoned").as_slice(),
        &[
            SeenEvent { current: 0, total: 4 },
            SeenEvent { current: 1, total: 4 },
            SeenEvent { current: 2, total: 4 },
            SeenEvent { current: 3, total: 4 },
        ],
    );
}

#[test]
fn scoped_progress_callback_can_borrow_stack_context() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = callback_loop(bundle.library());
    let events = Mutex::new(Vec::new());
    let callback = LeanProgressCallback::register(|event| {
        events
            .lock()
            .expect("callback events lock is not poisoned")
            .push(SeenEvent::from(event));
        LeanCallbackFlow::Continue
    })
    .expect("scoped progress callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    // SAFETY: The live registry registration supplies the exact trampoline and handle for this synchronous export.
    let status = unsafe { callback_loop.call(handle, trampoline, 3) }.expect("callback loop returns");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Ok));
    assert_eq!(
        events.lock().expect("callback events lock is not poisoned").as_slice(),
        &[
            SeenEvent { current: 0, total: 3 },
            SeenEvent { current: 1, total: 3 },
            SeenEvent { current: 2, total: 3 },
        ],
    );
}

#[test]
fn scoped_progress_callback_decodes_progress_shim_statuses() {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let callback = LeanProgressCallback::register(|_| {
        panic!("lean-rs scoped progress callback deliberate panic");
    })
    .expect("scoped progress callback registration succeeds");
    let (handle, trampoline) = callback.abi_parts();
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = callback_loop(bundle.library());

    // SAFETY: The live registry registration supplies the exact trampoline and handle for this synchronous export.
    let status = unsafe { callback_loop.call(handle, trampoline, 1) }
        .expect("callback loop returns after contained callback panic");
    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Panic));

    let raw = Err::<(), u8>(status).into_lean(runtime);
    let err = callback
        .decode_result::<()>(raw)
        .expect_err("panic status decodes to stored callback error");

    assert_eq!(err.code(), LeanDiagnosticCode::Internal);
    let LeanError::Host(host) = err else {
        panic!("expected progress callback panic to decode as a host failure");
    };
    assert_eq!(host.stage(), HostStage::CallbackPanic);
    assert!(
        host.message()
            .contains("lean-rs scoped progress callback deliberate panic")
    );
}

#[test]
fn scoped_progress_callback_rejects_stop_status_for_progress_shims() {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let callback = LeanProgressCallback::register(|_| LeanCallbackFlow::Stop)
        .expect("scoped progress callback registration succeeds");
    let raw = Err::<(), u8>(LeanCallbackStatus::Stopped.as_abi()).into_lean(runtime);

    let err = callback
        .decode_result::<()>(raw)
        .expect_err("progress shims do not define stop semantics");

    assert_eq!(err.code(), LeanDiagnosticCode::Internal);
    let LeanError::Host(host) = err else {
        panic!("expected stop status to decode as a host failure");
    };
    assert!(host.message().contains("do not define stop semantics"));
}

#[test]
fn registered_string_callback_decodes_owned_events() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = string_callback_loop(bundle.library());
    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&events);
    let callback = LeanCallbackHandle::<LeanStringEvent>::register(move |event| {
        callback_events
            .lock()
            .expect("callback events lock is not poisoned")
            .push(event.value);
        LeanCallbackFlow::Continue
    })
    .expect("string callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    // SAFETY: The live registry registration supplies the exact trampoline and handle for this synchronous export.
    let status = unsafe {
        callback_loop.call(
            handle,
            trampoline,
            vec!["alpha".to_owned(), "βeta".to_owned(), "with\0nul".to_owned()],
        )
    }
    .expect("string callback loop returns");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Ok));
    assert!(callback.last_error().is_none());
    assert_eq!(
        events.lock().expect("callback events lock is not poisoned").as_slice(),
        &["alpha".to_owned(), "βeta".to_owned(), "with\0nul".to_owned()],
    );
}

#[test]
fn callback_can_stop_lean_loop_cleanly() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = callback_loop(bundle.library());
    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&events);
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(move |event| {
        callback_events
            .lock()
            .expect("callback events lock is not poisoned")
            .push(SeenEvent::from(event));
        if event.current == 2 {
            LeanCallbackFlow::Stop
        } else {
            LeanCallbackFlow::Continue
        }
    })
    .expect("callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    // SAFETY: The live registry registration supplies the exact trampoline and handle for this synchronous export.
    let status =
        unsafe { callback_loop.call(handle, trampoline, 5) }.expect("callback loop returns after requested stop");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Stopped));
    assert!(callback.last_error().is_none());
    assert_eq!(
        events.lock().expect("callback events lock is not poisoned").as_slice(),
        &[
            SeenEvent { current: 0, total: 5 },
            SeenEvent { current: 1, total: 5 },
            SeenEvent { current: 2, total: 5 },
        ],
    );
}

#[test]
fn callback_panic_is_contained_at_registry_trampoline() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = callback_loop(bundle.library());
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(|event| {
        assert_ne!(
            event.current, 2,
            "lean-rs callback registry deliberate panic at {}",
            event.current,
        );
        LeanCallbackFlow::Continue
    })
    .expect("callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    // SAFETY: The live registry registration supplies the exact trampoline and handle for this synchronous export.
    let status = unsafe { callback_loop.call(handle, trampoline, 5) }
        .expect("callback loop returns after contained callback panic");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Panic),);
    let err = callback.last_error().expect("callback panic records a LeanError");
    assert_eq!(err.code(), LeanDiagnosticCode::Internal);
    let LeanError::Host(host) = err else {
        panic!("expected callback panic to record a host failure");
    };
    assert_eq!(host.stage(), HostStage::CallbackPanic);
    assert!(host.message().contains("lean-rs callback registry deliberate panic"));
}
