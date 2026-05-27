//! L1 string streaming callback example.
//!
//! The example stays below `lean-rs-host`: Lean emits JSONL-like strings through
//! the generic interop shim, and Rust receives them through
//! `LeanCallbackHandle<LeanStringEvent>`.

#![allow(unsafe_code, clippy::print_stdout)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lean_rs::module::{LeanIo, LeanLibraryBundle, LeanLibraryDependency};
use lean_rs::{LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanRuntime, LeanStringEvent};

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
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let add = unsafe { consumer_module.exported_unchecked::<(u64, u64), u64>("lean_rs_interop_consumer_add") }?;
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let stream = unsafe {
        consumer_module.exported_unchecked::<(usize, usize), LeanIo<u8>>("lean_rs_interop_consumer_jsonl_stream")
    }?;

    let sum = add.call(20, 22)?;
    require(sum == 42, "ordinary Lean export returned an unexpected value")?;

    let rows = Arc::new(Mutex::new(Vec::<String>::new()));
    let callback_rows = Arc::clone(&rows);
    let callback = LeanCallbackHandle::<LeanStringEvent>::register(move |event| {
        if let Ok(mut guard) = callback_rows.lock() {
            guard.push(event.value);
        }
        LeanCallbackFlow::Continue
    })?;

    let (handle, trampoline) = callback.abi_parts();
    let status = stream.call(handle, trampoline)?;
    let status = LeanCallbackStatus::from_abi(status).unwrap_or(LeanCallbackStatus::Panic);
    require(status == LeanCallbackStatus::Ok, status.description())?;
    require(callback.last_error().is_none(), "string callback recorded an error")?;

    let observed = rows.lock().map(|guard| guard.clone()).unwrap_or_default();
    let expected = vec![
        "{\"kind\":\"module\",\"name\":\"LeanRsInteropConsumer\"}".to_owned(),
        "{\"kind\":\"declaration\",\"name\":\"lean_rs_interop_consumer_add\"}".to_owned(),
        "{\"kind\":\"done\",\"count\":2}".to_owned(),
    ];
    require(observed == expected, "string stream sequence did not match")?;

    println!("add={sum}");
    println!("string_callback_status={status:?}");
    for row in &observed {
        println!("stream_row={row}");
    }
    println!("string streaming callback example completed");

    Ok(())
}
