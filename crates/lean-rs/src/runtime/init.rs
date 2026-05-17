//! Process-once Lean runtime initialization.
//!
//! The exact `lean_*` symbols and the order they are invoked are
//! intentionally not part of the public surface; see
//! `docs/architecture/01-safety-model.md`.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment naming the invariant. The blanket allow exists
// because this is a `pub(crate)` module that bridges the safe `lean-rs`
// boundary to raw `lean_rs_sys::init::*` entry points, per the safety
// model's "smallest scope that compiles" rule. Items below stay
// `pub(crate)` except for the `LeanRuntime` type and its `init`
// method, which are the only outward-facing pieces of this layer.
#![allow(unsafe_code)]

use std::any::Any;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::error::{InitError, LeanError, LeanResult};

/// Handle for the process-wide Lean runtime.
///
/// `LeanRuntime` is a zero-sized type with no public constructor. The only
/// way to obtain one is to call [`LeanRuntime::init`], which returns a
/// `'static` borrow once the Lean runtime is up. Every later handle
/// (`Obj<'lean>`, `LeanExpr<'lean>`, `LeanSession<'lean, '_>`, …) carries
/// a `'lean` lifetime tied to this borrow, so a value derived from Lean
/// cannot outlive the runtime that produced it. This is the
/// type-system anchor for the `'lean` cascade described in
/// `docs/architecture/03-host-api.md`.
///
/// Neither [`Send`] nor [`Sync`]. The Lean runtime is per-thread, and
/// shipping a Lean-derived handle to another OS thread is a soundness
/// hazard rather than an ergonomic choice; the `!Sync` claim here forces
/// `&'lean LeanRuntime` to be `!Send`, and every downstream handle that
/// holds `PhantomData<&'lean LeanRuntime>` inherits the same restriction.
pub struct LeanRuntime {
    _no_send_no_sync: PhantomData<*mut ()>,
}

impl LeanRuntime {
    /// Initialize the Lean runtime if it has not already been initialized,
    /// and return a `'static` borrow that anchors the `'lean` lifetime
    /// cascade.
    ///
    /// Idempotent and safe to call from any thread: the underlying
    /// initialization runs exactly once for the lifetime of the process.
    /// Subsequent calls — including calls from other threads — return the
    /// same borrow, or replay the cached failure if the first attempt
    /// failed.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Init`] if initialization failed; the wrapped
    /// [`InitError`] names the failure mode. Today the only reachable
    /// variant is [`InitError::RuntimePanic`], reported when a Rust panic
    /// is caught at the boundary so that it cannot unwind into Lean or C
    /// frames.
    pub fn init() -> LeanResult<&'static Self> {
        match INIT.get_or_init(do_initialize_once) {
            Ok(()) => Ok(static_ref()),
            Err(err) => Err(LeanError::Init(err.clone())),
        }
    }
}

impl std::fmt::Debug for LeanRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeanRuntime").finish()
    }
}

/// Process-wide initialization cell.
///
/// Stores the success-or-failure outcome of the first initialization
/// attempt so every later [`LeanRuntime::init`] call replays the same
/// result. Storing the outcome (rather than only a "did it run?" flag) is
/// what lets `init()` hand back the same typed error on retry without
/// re-entering the C frames a second time.
static INIT: OnceLock<Result<(), InitError>> = OnceLock::new();

/// Run the C-level initialization sequence under a panic boundary.
///
/// `OnceLock::get_or_init` guarantees this runs at most once per process,
/// which discharges the "call exactly once" obligation of the underlying
/// Lean entry points.
fn do_initialize_once() -> Result<(), InitError> {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Both calls are valid to invoke once per process before
        // any other Lean code runs; `OnceLock::get_or_init` enforces the
        // "once" half of the contract. The order
        // (`runtime_module` before the full `initialize`) follows the
        // documented Lean embedding sequence captured in
        // `crates/lean-rs-sys/src/init.rs`. Neither call takes inputs
        // from Rust state, so there is no aliasing or lifetime hazard.
        unsafe {
            lean_rs_sys::init::lean_initialize_runtime_module();
            lean_rs_sys::init::lean_initialize();
        }
    }));
    match outcome {
        Ok(()) => Ok(()),
        Err(payload) => Err(panic_payload_to_error(payload.as_ref())),
    }
}

fn panic_payload_to_error(payload: &(dyn Any + Send)) -> InitError {
    InitError::runtime_panic(payload)
}

/// Hand out a `'static` borrow of the zero-sized [`LeanRuntime`] anchor.
fn static_ref() -> &'static LeanRuntime {
    // SAFETY: `LeanRuntime` is a zero-sized type, so any aligned non-null
    // pointer is a valid `*const LeanRuntime`. `NonNull::dangling()`
    // returns exactly such a pointer. The resulting borrow carries no
    // bytes; the Lean runtime, once initialized, is never finalized for
    // the remainder of the process, so a `'static` lifetime is honest.
    // The borrow's `!Sync` (and therefore `&LeanRuntime: !Send`) property
    // is preserved by the type itself, not by where it is stored. This is
    // the standard ZST-handle pattern (cf. PyO3's `Bound`).
    unsafe { NonNull::<LeanRuntime>::dangling().as_ref() }
}
