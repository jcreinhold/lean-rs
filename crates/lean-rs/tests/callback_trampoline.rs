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
use lean_rs::module::{LeanIo, LeanLibrary};

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

extern "C" fn test_callback_trampoline(handle: usize, current: u64, total: u64) -> u8 {
    let result = catch_unwind(AssertUnwindSafe(|| {
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

#[test]
fn lean_loop_invokes_rust_trampoline_in_order() {
    let library = shims_library();
    let module = library
        .initialize_module("lean_rs_host_shims", "LeanRsHostShims")
        .expect("shim root module initializes");
    let callback_loop = module
        .exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_test_callback_loop")
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
    let library = shims_library();
    let module = library
        .initialize_module("lean_rs_host_shims", "LeanRsHostShims")
        .expect("shim root module initializes");
    let callback_loop = module
        .exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_test_callback_loop")
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
