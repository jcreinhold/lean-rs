//! L1 interop example using the generic Lean shim package.
//!
//! The example stays below `lean-rs-host`: it builds the generic interop shim
//! package and a downstream-style Lake target through `lean-toolchain`, opens
//! both dylibs with `LeanLibrary`, and invokes a Lean loop that calls a Rust
//! callback registered through `LeanCallbackHandle`.

#![allow(clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lean_rs::module::{LeanIo, LeanLibrary};
use lean_rs::{LeanCallbackEvent, LeanCallbackHandle, LeanCallbackStatus, LeanRuntime};

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

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or(manifest_dir)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = workspace_root();
    let interop_shims =
        lean_toolchain::build_lake_target(&workspace.join("lake").join("lean-rs-interop-shims"), "LeanRsInterop")?;
    let consumer = lean_toolchain::build_lake_target(
        &workspace.join("fixtures").join("interop-shims"),
        "LeanRsInteropConsumer",
    )?;

    let runtime = LeanRuntime::init()?;
    let interop_library = LeanLibrary::open_globally(runtime, &interop_shims)?;
    let _interop_module = interop_library.initialize_module("lean_rs_interop_shims", "LeanRsInterop")?;

    let consumer_library = LeanLibrary::open(runtime, &consumer)?;
    let consumer_module = consumer_library.initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")?;
    let callback_loop =
        consumer_module.exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")?;

    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&events);
    let callback = LeanCallbackHandle::register(move |event| {
        if let Ok(mut guard) = callback_events.lock() {
            guard.push(SeenEvent::from(event));
        }
    })?;

    let (handle, trampoline) = callback.abi_parts();
    let status = callback_loop.call(handle, trampoline, 4)?;
    let status = LeanCallbackStatus::from_abi(status).unwrap_or(LeanCallbackStatus::Panic);
    println!("status={status:?}");
    println!(
        "events={:?}",
        events.lock().map(|guard| guard.clone()).unwrap_or_default()
    );

    Ok(())
}
