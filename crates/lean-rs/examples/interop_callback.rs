//! L1 interop example using the generic Lean shim package.
//!
//! The example stays below `lean-rs-host`: it builds the generic interop shim
//! package and a downstream-style Lake target through `lean-toolchain`, opens
//! both dylibs with `LeanLibrary`, calls an ordinary Lean export from Rust, and
//! invokes a Lean loop that calls a Rust callback registered through
//! `LeanCallbackHandle`.

#![allow(clippy::print_stdout)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lean_rs::module::{LeanIo, LeanLibraryBundle, LeanLibraryDependency};
use lean_rs::{LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanProgressTick, LeanRuntime};

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

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or(manifest_dir)
}

fn require(condition: bool, message: &'static str) -> Result<(), io::Error> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = workspace_root();
    let interop_shims = lean_toolchain::build_lake_target(
        &workspace
            .join("crates")
            .join("lean-rs")
            .join("shims")
            .join("lean-rs-interop-shims"),
        "LeanRsInterop",
    )?;
    let consumer = lean_toolchain::build_lake_target(
        &workspace.join("fixtures").join("interop-shims"),
        "LeanRsInteropConsumer",
    )?;

    let runtime = LeanRuntime::init()?;
    let consumer_bundle = LeanLibraryBundle::open(
        runtime,
        &consumer,
        [LeanLibraryDependency::path(interop_shims)
            .export_symbols_for_dependents()
            .initializer("lean_rs_interop_shims", "LeanRsInterop")],
    )?;
    let consumer_module = consumer_bundle.initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")?;
    let add = consumer_module.exported::<(u64, u64), u64>("lean_rs_interop_consumer_add")?;
    let callback_loop =
        consumer_module.exported::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")?;

    let sum = add.call(20, 22)?;
    require(sum == 42, "ordinary Lean export returned an unexpected value")?;

    let events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&events);
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(move |event| {
        if let Ok(mut guard) = callback_events.lock() {
            guard.push(SeenEvent::from(event));
        }
        LeanCallbackFlow::Continue
    })?;

    let (handle, trampoline) = callback.abi_parts();
    let status = callback_loop.call(handle, trampoline, 4)?;
    let status = LeanCallbackStatus::from_abi(status).unwrap_or(LeanCallbackStatus::Panic);
    require(status == LeanCallbackStatus::Ok, status.description())?;
    let observed = events.lock().map(|guard| guard.clone()).unwrap_or_default();
    let expected = vec![
        SeenEvent { current: 0, total: 4 },
        SeenEvent { current: 1, total: 4 },
        SeenEvent { current: 2, total: 4 },
        SeenEvent { current: 3, total: 4 },
    ];
    require(observed == expected, "callback event sequence did not match")?;

    println!("add={sum}");
    println!("callback_status={status:?}");
    println!("callback_events={observed:?}");
    println!("downstream interop example completed");

    Ok(())
}
