//! Test-only callback ABI spike.
//!
//! The test intentionally avoids a public callback registry. Rust passes an
//! opaque handle and a function pointer as `USize` values to a Lean export.
//! Lean calls a tiny C helper linked into the shim dylib; that helper casts
//! the function pointer and invokes it on the same thread.

#![allow(clippy::expect_used, clippy::panic)]
#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Mutex;

use lean_rs::LeanRuntime;
use lean_rs::module::{LeanIo, LeanLibraryBundle, LeanLibraryDependency};

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

#[test]
fn lean_loop_invokes_rust_trampoline_in_order() {
    let bundle = consumer_bundle();
    let module = bundle
        .library()
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    let callback_loop = module
        .exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")
        .expect("callback loop export resolves");

    let probe = CallbackProbe::new(None);
    let status = callback_loop
        .call(
            std::ptr::from_ref(&probe).addr(),
            test_callback_trampoline as *const () as usize,
            5,
        )
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
    let bundle = consumer_bundle();
    let module = bundle
        .library()
        .initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")
        .expect("consumer root module initializes");
    let callback_loop = module
        .exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")
        .expect("callback loop export resolves");

    let probe = CallbackProbe::new(Some(2));
    let status = callback_loop
        .call(
            std::ptr::from_ref(&probe).addr(),
            test_callback_trampoline as *const () as usize,
            5,
        )
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
