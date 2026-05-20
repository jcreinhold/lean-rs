//! Public callback registry tests.
//!
//! These tests reuse the Lean callback-loop export from the trampoline
//! spike, but the Rust side goes through the public RAII registry
//! instead of passing a stack pointer and test-local trampoline.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lean_rs::module::{LeanIo, LeanLibrary};
use lean_rs::{
    HostStage, LeanCallbackEvent, LeanCallbackHandle, LeanCallbackStatus, LeanDiagnosticCode, LeanError, LeanRuntime,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SeenEvent {
    current: u64,
    total: u64,
}

impl From<LeanCallbackEvent> for SeenEvent {
    fn from(value: LeanCallbackEvent) -> Self {
        Self {
            current: value.current,
            total: value.total,
        }
    }
}

fn shims_dylib_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = workspace
        .join("lake")
        .join("lean-rs-host-shims")
        .join(".lake")
        .join("build")
        .join("lib");
    let new_style = lib_dir.join(format!("liblean__rs__host__shims_LeanRsHostShims.{dylib_extension}"));
    let old_style = lib_dir.join(format!("libLeanRsHostShims.{dylib_extension}"));
    if old_style.is_file() && !new_style.is_file() {
        old_style
    } else {
        new_style
    }
}

fn shims_library() -> LeanLibrary<'static> {
    let path = shims_dylib_path();
    assert!(
        path.exists(),
        "shim dylib not found at {} — run `cd lake/lean-rs-host-shims && lake build`",
        path.display(),
    );
    LeanLibrary::open(
        LeanRuntime::init().expect("Lean runtime initialisation must succeed"),
        &path,
    )
    .expect("shim dylib opens cleanly")
}

fn callback_loop<'lean, 'lib>(
    library: &'lib LeanLibrary<'lean>,
) -> lean_rs::LeanExported<'lean, 'lib, (usize, usize, u64), LeanIo<u8>> {
    let module = library
        .initialize_module("lean_rs_host_shims", "LeanRsHostShims")
        .expect("shim root module initializes");
    module
        .exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_test_callback_loop")
        .expect("callback loop export resolves")
}

#[test]
fn registered_callback_runs_through_typed_lean_export() {
    let library = shims_library();
    let callback_loop = callback_loop(&library);
    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&events);
    let callback = LeanCallbackHandle::register(move |event| {
        callback_events
            .lock()
            .expect("callback events lock is not poisoned")
            .push(SeenEvent::from(event));
    })
    .expect("callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    let status = callback_loop
        .call(handle, trampoline, 4)
        .expect("callback loop returns");

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
fn dropped_handle_reports_stale_without_use_after_drop() {
    let library = shims_library();
    let callback_loop = callback_loop(&library);
    let callback = LeanCallbackHandle::register(|_| {}).expect("callback registration succeeds");
    let (handle, trampoline) = callback.abi_parts();
    drop(callback);

    let status = callback_loop
        .call(handle, trampoline, 1)
        .expect("callback loop returns");

    assert_eq!(
        LeanCallbackStatus::from_abi(status),
        Some(LeanCallbackStatus::StaleHandle),
    );
}

#[test]
fn callback_panic_is_contained_at_registry_trampoline() {
    let library = shims_library();
    let callback_loop = callback_loop(&library);
    let callback = LeanCallbackHandle::register(|event| {
        assert_ne!(
            event.current, 2,
            "lean-rs callback registry deliberate panic at {}",
            event.current,
        );
    })
    .expect("callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    let status = callback_loop
        .call(handle, trampoline, 5)
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
