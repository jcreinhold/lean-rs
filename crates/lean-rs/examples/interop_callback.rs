//! Same-process interop example using the generic Lean shim package.
//!
//! The example stays below `lean-rs-host`: it builds the generic interop shim
//! package and a downstream-style Lake target through `lean-toolchain`, opens
//! both dylibs with `LeanLibrary`, calls an ordinary Lean export from Rust, and
//! invokes a Lean loop that calls a Rust callback registered through
//! `LeanCallbackHandle`.

#![allow(unsafe_code, clippy::print_stdout)]

use std::io;
#[path = "support/callback_fixture.rs"]
mod callback_fixture;
use std::sync::{Arc, Mutex};

use lean_rs::module::LeanIo;
use lean_rs::{LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanProgressTick};

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

fn require(condition: bool, message: &'static str) -> Result<(), io::Error> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let consumer_bundle = callback_fixture::bundle()?;
    let consumer_module = consumer_bundle.initialize_module("lean_rs_interop_consumer", "LeanRsInteropConsumer")?;
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let add = unsafe { consumer_module.exported_unchecked::<(u64, u64), u64>("lean_rs_interop_consumer_add") }?;
    let callback_loop =
        // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
        unsafe { consumer_module.exported_unchecked::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop") }?;

    // SAFETY: the admitted consumer adds two unboxed UInt64 values and returns UInt64.
    let sum = unsafe { add.call(20, 22) }?;
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
    // SAFETY: this live registration supplies the matching event trampoline and handle
    // for this synchronous loop; the admitted consumer and dependency remain loaded.
    let status = unsafe { callback_loop.call(handle, trampoline, 4) }?;
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
