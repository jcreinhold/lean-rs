//! Test-only string callback ABI spike.
//!
//! This file proves the next payload shape without widening the public
//! callback registry. Rust passes an opaque handle and a test-local
//! trampoline as `USize` values. Lean calls the generic interop shim, which
//! forwards a borrowed Lean `String` object to Rust.

#![allow(clippy::expect_used, clippy::panic)]
#![allow(unsafe_code)]

#[path = "../examples/support/callback_fixture.rs"]
mod callback_fixture;
use std::slice;
use std::sync::Mutex;

use lean_rs::module::{LeanIo, LeanLibrary};
use lean_rs_sys::lean_object;
use lean_rs_sys::object::{lean_is_scalar, lean_is_string};
use lean_rs_sys::string::{lean_string_cstr, lean_string_size};

const OK: u8 = 0;
const STALE_HANDLE: u8 = 1;
const PANIC: u8 = 2;
const WRONG_PAYLOAD: u8 = 3;
const PAYLOAD_STRING: u8 = 1;

#[derive(Debug)]
struct StringCallbackProbe {
    events: Mutex<Vec<String>>,
    panic_on: Option<&'static str>,
}

impl StringCallbackProbe {
    fn new(panic_on: Option<&'static str>) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            panic_on,
        }
    }

    fn report(&self, value: String) {
        assert_ne!(
            self.panic_on,
            Some(value.as_str()),
            "lean-rs string callback ABI deliberate panic on {value}",
        );
        self.events
            .lock()
            .expect("callback event lock is not poisoned")
            .push(value);
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("callback event lock is not poisoned").clone()
    }
}

extern "C" fn test_string_callback_trampoline(
    handle: usize,
    payload_tag: u8,
    _arg0: u64,
    _arg1: u64,
    payload: *mut lean_object,
) -> u8 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if payload_tag != PAYLOAD_STRING {
            return WRONG_PAYLOAD;
        }
        let Some(probe) = probe_from_handle(handle) else {
            return STALE_HANDLE;
        };
        let Some(value) = decode_string_payload(payload) else {
            return WRONG_PAYLOAD;
        };
        // SAFETY: `probe_from_handle` checked for the null sentinel. Tests pass
        // a pointer to a stack-local probe that outlives this synchronous Lean
        // callback loop.
        unsafe { &*probe }.report(value);
        OK
    }));

    match result {
        Ok(status) => status,
        Err(_) => PANIC,
    }
}

fn probe_from_handle(handle: usize) -> Option<*const StringCallbackProbe> {
    if handle == 0 {
        return None;
    }
    Some(handle as *const StringCallbackProbe)
}

fn decode_string_payload(payload: *mut lean_object) -> Option<String> {
    if payload.is_null() {
        return None;
    }
    // SAFETY: scalar check inspects pointer bits only and is valid for every
    // Lean-shaped value the trampoline may receive.
    if unsafe { lean_is_scalar(payload) } {
        return None;
    }
    // SAFETY: for the happy path, Lean passed a borrowed `String` object
    // through `payload : @& String`; for the wrong-payload test we only pass
    // null/scalar-shaped values and return before this heap predicate.
    if !unsafe { lean_is_string(payload) } {
        return None;
    }
    // SAFETY: kind verified; the string is borrowed for the duration of the
    // extern call. Copy the bytes into Rust before invoking user code so no
    // Lean object lifetime escapes the trampoline.
    let bytes = unsafe {
        let size_with_nul = lean_string_size(payload);
        let len = size_with_nul.saturating_sub(1);
        let data = lean_string_cstr(payload).cast::<u8>();
        slice::from_raw_parts(data, len)
    };
    String::from_utf8(bytes.to_vec()).ok()
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
fn lean_loop_invokes_string_trampoline_in_order() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = string_callback_loop(bundle.library());
    let probe = StringCallbackProbe::new(None);
    // SAFETY: The probe lives through this synchronous call and the trampoline implements the fixture callback ABI.
    let status = unsafe {
        callback_loop.call(
            std::ptr::from_ref(&probe).addr(),
            test_string_callback_trampoline as *const () as usize,
            vec!["alpha".to_owned(), "βeta".to_owned(), "with\0nul".to_owned()],
        )
    }
    .expect("string callback loop returns");

    assert_eq!(status, OK);
    assert_eq!(
        probe.events(),
        vec!["alpha".to_owned(), "βeta".to_owned(), "with\0nul".to_owned()],
    );
}

#[test]
fn wrong_payload_returns_status_without_panic() {
    let probe = StringCallbackProbe::new(None);
    let status = test_string_callback_trampoline(std::ptr::from_ref(&probe).addr(), 0, 0, 1, std::ptr::null_mut());
    assert_eq!(status, WRONG_PAYLOAD);
    assert!(probe.events().is_empty());
}

#[test]
fn rust_string_callback_panic_is_caught_before_returning_to_lean() {
    let bundle = callback_fixture::bundle().expect("admitted callback fixture pair");
    let callback_loop = string_callback_loop(bundle.library());
    let probe = StringCallbackProbe::new(Some("panic"));
    // SAFETY: The probe lives through this synchronous call and the trampoline implements the fixture callback ABI.
    let status = unsafe {
        callback_loop.call(
            std::ptr::from_ref(&probe).addr(),
            test_string_callback_trampoline as *const () as usize,
            vec!["before".to_owned(), "panic".to_owned(), "after".to_owned()],
        )
    }
    .expect("string callback loop returns after caught callback panic");

    assert_eq!(status, PANIC);
    assert_eq!(probe.events(), vec!["before".to_owned()]);
}
