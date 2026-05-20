//! Test-only string callback ABI spike.
//!
//! This file proves the next payload shape without widening the public
//! callback registry. Rust passes an opaque handle and a test-local
//! trampoline as `USize` values. Lean calls the generic interop shim, which
//! forwards a borrowed Lean `String` object to Rust.

#![allow(clippy::expect_used, clippy::panic)]
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::slice;
use std::sync::Mutex;

use lean_rs::LeanRuntime;
use lean_rs::module::{LeanIo, LeanLibrary};
use lean_rs_sys::lean_object;
use lean_rs_sys::object::{lean_is_scalar, lean_is_string};
use lean_rs_sys::string::{lean_string_cstr, lean_string_size};

const OK: u8 = 0;
const STALE_HANDLE: u8 = 1;
const PANIC: u8 = 2;
const WRONG_PAYLOAD: u8 = 3;

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

extern "C" fn test_string_callback_trampoline(handle: usize, payload: *mut lean_object) -> u8 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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

fn dylib_path(package_dir: &[&str], new_name: &str, old_name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root")
        .to_path_buf();
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = package_dir
        .iter()
        .fold(workspace, |path, part| path.join(part))
        .join(".lake")
        .join("build")
        .join("lib");
    let new_style = lib_dir.join(format!("{new_name}.{dylib_extension}"));
    let old_style = lib_dir.join(format!("{old_name}.{dylib_extension}"));
    if old_style.is_file() && !new_style.is_file() {
        old_style
    } else {
        new_style
    }
}

fn interop_dylib_path() -> PathBuf {
    dylib_path(
        &["crates", "lean-rs", "shims", "lean-rs-interop-shims"],
        "liblean__rs__interop__shims_LeanRsInterop",
        "libLeanRsInterop",
    )
}

fn consumer_dylib_path() -> PathBuf {
    dylib_path(
        &["fixtures", "interop-shims"],
        "liblean__rs__interop__consumer_LeanRsInteropConsumer",
        "libLeanRsInteropConsumer",
    )
}

fn consumer_library() -> LeanLibrary<'static> {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let interop_path = interop_dylib_path();
    assert!(
        interop_path.exists(),
        "interop dylib not found at {} — run `cd crates/lean-rs/shims/lean-rs-interop-shims && lake build`",
        interop_path.display(),
    );
    let interop = LeanLibrary::open_globally(runtime, &interop_path).expect("interop dylib opens cleanly");
    let _interop_module = interop
        .initialize_module("lean_rs_interop_shims", "LeanRsInterop")
        .expect("interop root module initializes");

    let path = consumer_dylib_path();
    assert!(
        path.exists(),
        "interop consumer dylib not found at {} — run `cd fixtures/interop-shims && lake build`",
        path.display(),
    );
    LeanLibrary::open(runtime, &path).expect("interop consumer dylib opens cleanly")
}

fn string_callback_loop<'lean, 'lib>(
    library: &'lib LeanLibrary<'lean>,
) -> lean_rs::LeanExported<'lean, 'lib, (usize, usize, Vec<String>), LeanIo<u8>> {
    let module = library
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    module
        .exported::<(usize, usize, Vec<String>), LeanIo<u8>>("lean_rs_interop_consumer_string_callback_loop")
        .expect("string callback loop export resolves")
}

fn tick_callback_loop<'lean, 'lib>(
    library: &'lib LeanLibrary<'lean>,
) -> lean_rs::LeanExported<'lean, 'lib, (usize, usize, u64), LeanIo<u8>> {
    let module = library
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    module
        .exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")
        .expect("tick callback loop export resolves")
}

#[test]
fn lean_loop_invokes_string_trampoline_in_order() {
    let library = consumer_library();
    let callback_loop = string_callback_loop(&library);
    let probe = StringCallbackProbe::new(None);
    let status = callback_loop
        .call(
            std::ptr::from_ref(&probe).addr(),
            test_string_callback_trampoline as *const () as usize,
            vec!["alpha".to_owned(), "βeta".to_owned(), "with\0nul".to_owned()],
        )
        .expect("string callback loop returns");

    assert_eq!(status, OK);
    assert_eq!(
        probe.events(),
        vec!["alpha".to_owned(), "βeta".to_owned(), "with\0nul".to_owned()],
    );
}

#[test]
fn wrong_payload_returns_status_without_panic() {
    let library = consumer_library();
    let callback_loop = tick_callback_loop(&library);
    let probe = StringCallbackProbe::new(None);
    let status = callback_loop
        .call(
            std::ptr::from_ref(&probe).addr(),
            test_string_callback_trampoline as *const () as usize,
            1,
        )
        .expect("tick loop returns wrong-payload status");

    assert_eq!(status, WRONG_PAYLOAD);
    assert!(probe.events().is_empty());
}

#[test]
fn rust_string_callback_panic_is_caught_before_returning_to_lean() {
    let library = consumer_library();
    let callback_loop = string_callback_loop(&library);
    let probe = StringCallbackProbe::new(Some("panic"));
    let status = callback_loop
        .call(
            std::ptr::from_ref(&probe).addr(),
            test_string_callback_trampoline as *const () as usize,
            vec!["before".to_owned(), "panic".to_owned(), "after".to_owned()],
        )
        .expect("string callback loop returns after caught callback panic");

    assert_eq!(status, PANIC);
    assert_eq!(probe.events(), vec!["before".to_owned()]);
}
