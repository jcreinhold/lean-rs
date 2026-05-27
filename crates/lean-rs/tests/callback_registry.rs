//! Public callback registry tests.
//!
//! These tests reuse the Lean callback-loop export from the trampoline
//! spike, but the Rust side goes through the public RAII registry
//! instead of passing a stack pointer and test-local trampoline.

#![allow(unsafe_code, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lean_rs::module::{LeanIo, LeanLibrary, LeanLibraryBundle, LeanLibraryDependency};
use lean_rs::{
    HostStage, LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanCapability, LeanDiagnosticCode, LeanError,
    LeanProgressTick, LeanRuntime, LeanStringEvent,
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

fn consumer_bundle() -> LeanLibraryBundle<'static> {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let interop_path = interop_dylib_path();
    assert!(
        interop_path.exists(),
        "interop dylib not found at {} — run `cd crates/lean-rs/shims/lean-rs-interop-shims && lake build`",
        interop_path.display(),
    );
    let path = consumer_dylib_path();
    assert!(
        path.exists(),
        "interop consumer dylib not found at {} — run `cd fixtures/interop-shims && lake build`",
        path.display(),
    );
    LeanLibraryBundle::open(
        runtime,
        &path,
        [LeanLibraryDependency::path(interop_path)
            .export_symbols_for_dependents()
            .initializer("lean_rs_interop_shims", "LeanRsInterop")],
    )
    .expect("interop consumer bundle opens cleanly")
}

fn consumer_capability() -> LeanCapability<'static> {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    LeanCapability::open_with_dependencies(
        runtime,
        consumer_dylib_path(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        [LeanLibraryDependency::path(interop_dylib_path())
            .export_symbols_for_dependents()
            .initializer("lean_rs_interop_shims", "LeanRsInterop")],
    )
    .expect("interop consumer capability opens cleanly")
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
    let bundle = consumer_bundle();
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
fn capability_bundle_keeps_dependency_alive_after_open_helper_returns() {
    let capability = consumer_capability();
    assert_eq!(capability.bundle().dependency_count(), 1);
    let callback_loop = callback_loop(capability.library());
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(|_| LeanCallbackFlow::Continue)
        .expect("callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    let status = callback_loop
        .call(handle, trampoline, 1)
        .expect("callback loop returns after helper-created capability opened");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Ok));
}

#[test]
fn registered_string_callback_decodes_owned_events() {
    let bundle = consumer_bundle();
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
    let status = callback_loop
        .call(
            handle,
            trampoline,
            vec!["alpha".to_owned(), "βeta".to_owned(), "with\0nul".to_owned()],
        )
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
    let bundle = consumer_bundle();
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
    let status = callback_loop
        .call(handle, trampoline, 5)
        .expect("callback loop returns after requested stop");

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
fn wrong_payload_returns_status_without_calling_callback() {
    let bundle = consumer_bundle();
    let callback_loop = callback_loop(bundle.library());
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
    let status = callback_loop
        .call(handle, trampoline, 1)
        .expect("tick loop returns wrong-payload status");

    assert_eq!(
        LeanCallbackStatus::from_abi(status),
        Some(LeanCallbackStatus::WrongPayload),
    );
    assert!(events.lock().expect("callback events lock is not poisoned").is_empty());
}

#[test]
fn wrong_string_payload_returns_status_without_calling_tick_callback() {
    let bundle = consumer_bundle();
    let callback_loop = string_callback_loop(bundle.library());
    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&events);
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(move |event| {
        callback_events
            .lock()
            .expect("callback events lock is not poisoned")
            .push(SeenEvent::from(event));
        LeanCallbackFlow::Continue
    })
    .expect("tick callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    let status = callback_loop
        .call(handle, trampoline, vec!["not-a-tick".to_owned()])
        .expect("string loop returns wrong-payload status");

    assert_eq!(
        LeanCallbackStatus::from_abi(status),
        Some(LeanCallbackStatus::WrongPayload),
    );
    assert!(events.lock().expect("callback events lock is not poisoned").is_empty());
}

#[test]
fn dropped_handle_reports_stale_without_use_after_drop() {
    let bundle = consumer_bundle();
    let callback_loop = callback_loop(bundle.library());
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(|_| LeanCallbackFlow::Continue)
        .expect("callback registration succeeds");
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
    let bundle = consumer_bundle();
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
