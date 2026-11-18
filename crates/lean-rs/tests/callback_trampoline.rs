//! Test-only callback ABI spike.
//!
//! The test intentionally avoids a public callback registry. Rust passes an
//! opaque handle and a function pointer as `USize` values to a Lean export.
//! Lean calls a tiny C helper linked into the shim dylib; that helper casts
//! the function pointer and invokes it on the same thread.

#![allow(clippy::expect_used, clippy::panic)]
#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
#[path = "../examples/support/callback_fixture.rs"]
mod callback_fixture;
use std::sync::Mutex;

use lean_rs::module::LeanIo;

const PAYLOAD_TICK: u8 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CallbackEvent {
    current: u64,
    total: u64,
}

#[derive(Debug)]
struct CallbackProbe {
    events: Mutex<Vec<CallbackEvent>>,
    panic_at: Option<u64>,
}

impl CallbackProbe {
    fn new(panic_at: Option<u64>) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            panic_at,
        }
    }

    fn report(&self, current: u64, total: u64) {
        self.events
            .lock()
            .expect("callback event lock is not poisoned")
            .push(CallbackEvent { current, total });

        assert_ne!(
            self.panic_at,
            Some(current),
            "lean-rs callback trampoline spike panic at {current}",
        );
    }

    fn events(&self) -> Vec<CallbackEvent> {
        self.events.lock().expect("callback event lock is not poisoned").clone()
    }
}

extern "C" fn test_callback_trampoline(
    handle: usize,
    payload_tag: u8,
    current: u64,
    total: u64,
    _payload: *mut std::ffi::c_void,
) -> u8 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        assert_eq!(payload_tag, PAYLOAD_TICK, "callback payload tag must be tick");
        let probe = {
            assert_ne!(handle, 0, "callback handle must be non-null");
            let ptr = handle as *const CallbackProbe;
            // SAFETY: the integration test passes a pointer to a stack-local
            // `CallbackProbe` and waits for the Lean call to return before the
            // probe is dropped. The callback runs synchronously on the same
            // thread through the C shim helper.
            unsafe { &*ptr }
        };
        probe.report(current, total);
    }));

    match result {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[test]
fn lean_loop_invokes_rust_trampoline_in_order() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let module = bundle
        .library()
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let callback_loop = unsafe {
        module.exported_unchecked::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")
    }
    .expect("callback loop export resolves");

    let probe = CallbackProbe::new(None);
    // SAFETY: The probe lives through this synchronous call and the trampoline implements the fixture callback ABI.
    let status = unsafe {
        callback_loop.call(
            std::ptr::from_ref(&probe).addr(),
            test_callback_trampoline as *const () as usize,
            5,
        )
    }
    .expect("callback loop returns");

    assert_eq!(status, 0);
    assert_eq!(
        probe.events(),
        vec![
            CallbackEvent { current: 0, total: 5 },
            CallbackEvent { current: 1, total: 5 },
            CallbackEvent { current: 2, total: 5 },
            CallbackEvent { current: 3, total: 5 },
            CallbackEvent { current: 4, total: 5 },
        ],
    );
}

#[test]
fn rust_callback_panic_is_caught_before_returning_to_lean() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let module = bundle
        .library()
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let callback_loop = unsafe {
        module.exported_unchecked::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")
    }
    .expect("callback loop export resolves");

    let probe = CallbackProbe::new(Some(2));
    // SAFETY: The probe lives through this synchronous call and the trampoline implements the fixture callback ABI.
    let status = unsafe {
        callback_loop.call(
            std::ptr::from_ref(&probe).addr(),
            test_callback_trampoline as *const () as usize,
            5,
        )
    }
    .expect("callback loop returns after caught callback panic");

    assert_eq!(status, 1);
    assert_eq!(
        probe.events(),
        vec![
            CallbackEvent { current: 0, total: 5 },
            CallbackEvent { current: 1, total: 5 },
            CallbackEvent { current: 2, total: 5 },
        ],
    );
}
