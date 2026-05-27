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

use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::OnceLock;

#[cfg(doc)]
use crate::error::HostStage;
use crate::error::{LeanError, LeanResult};

/// Handle for the process-wide Lean runtime.
///
/// `LeanRuntime` is a zero-sized type with no public constructor. The only
/// way to obtain one is to call [`LeanRuntime::init`], which returns a
/// `'static` borrow once the Lean runtime is up. Every later handle
/// (`Obj<'lean>`, `LeanExpr<'lean>`, `LeanSession<'lean, '_>`, …) carries
/// a `'lean` lifetime tied to this borrow, so a value derived from Lean
/// cannot outlive the runtime that produced it. This is the
/// type-system anchor for the `'lean` cascade described in
/// `docs/architecture/03-host-stack.md`.
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
    /// Subsequent calls—including calls from other threads—return the
    /// same borrow, or replay the cached failure if the first attempt
    /// failed.
    ///
    /// # Worker threads
    ///
    /// `init` starts a process-wide Lean task manager. The worker thread
    /// count is Lean's compiled-in default—typically one worker per
    /// hardware core—unless the `LEAN_RS_NUM_THREADS` environment
    /// variable is set to a positive integer before the first call to
    /// `init`. The first call captures the value; later changes to the
    /// variable have no effect. Set `LEAN_RS_NUM_THREADS` when several
    /// Lean-using processes run side by side (CI test matrices, batch
    /// jobs, multi-tenant workers) to avoid oversubscribing cores. The
    /// pool is process-lifetime; there is no `set_num_threads`-style
    /// reconfiguration once `init` has run.
    ///
    /// # Errors
    ///
    /// Returns a [`LeanError::Host`] with stage [`HostStage::RuntimeInit`]
    /// if initialization failed. Today the only reachable failure is a
    /// caught panic from the Lean `lean_initialize_*` entry points; the
    /// panic payload is rendered into a bounded message so it cannot
    /// unwind into Lean or C frames.
    pub fn init() -> LeanResult<&'static Self> {
        let _span = tracing::info_span!(target: "lean_rs", "lean_rs.runtime.init").entered();
        match INIT.get_or_init(do_initialize_once) {
            Ok(()) => {
                // The build script already accepts only `lean.h` digests in
                // the [`SUPPORTED_TOOLCHAINS`](lean_rs_sys::SUPPORTED_TOOLCHAINS)
                // window, so this probe is a belt over the buckle: it catches
                // a hand-edited `consts.rs` or a stale build artifact, and it
                // emits the resolved version into the trace span on every
                // initialization so operators can see which toolchain is
                // actually live. Lean's C ABI exposes no runtime version
                // query (`lean::get_short_version_string` is a C++ symbol
                // not part of the documented surface), so a live-library
                // substitution after build cannot be detected here.
                if lean_rs_sys::supported_for(lean_rs_sys::LEAN_VERSION).is_none() {
                    tracing::error!(
                        target: "lean_rs",
                        code = crate::error::LeanDiagnosticCode::RuntimeInit.as_str(),
                        version = lean_rs_sys::LEAN_VERSION,
                        "active Lean toolchain not in the supported window",
                    );
                    return Err(LeanError::runtime_init_unsupported_toolchain(lean_rs_sys::LEAN_VERSION));
                }
                tracing::debug!(
                    target: "lean_rs",
                    resolved_version = lean_rs_sys::LEAN_RESOLVED_VERSION,
                    discovered_version = lean_rs_sys::LEAN_VERSION,
                    "Lean runtime initialized",
                );
                // Every successful `init()` mints the caller's permission
                // to invoke Lean from the calling thread. The first call
                // also runs the C-level `lean_initialize*` sequence inside
                // `do_initialize_once`; subsequent calls (including those
                // from other threads) only mark the local TLS depth. The
                // call is idempotent and `!Send`-coherent: every safe
                // handle requires a `&'lean LeanRuntime` to construct, so
                // any thread that reaches a host call has provably been
                // through `init()` on that thread.
                super::thread::mark_calling_thread_permitted();
                Ok(static_ref())
            }
            Err(err) => {
                tracing::error!(
                    target: "lean_rs",
                    code = crate::error::LeanDiagnosticCode::RuntimeInit.as_str(),
                    "Lean runtime initialization failed",
                );
                Err(err.clone())
            }
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
static INIT: OnceLock<Result<(), LeanError>> = OnceLock::new();

/// Run the C-level initialization sequence under a panic boundary.
///
/// `OnceLock::get_or_init` guarantees this runs at most once per process,
/// which discharges the "call exactly once" obligation of the underlying
/// Lean entry points.
fn do_initialize_once() -> Result<(), LeanError> {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        let workers = read_num_threads_env();
        // SAFETY: All four calls are valid to invoke once per process
        // before any other Lean code runs; `OnceLock::get_or_init`
        // enforces the "once" half of the contract. The order
        // (`runtime_module` before the full `initialize`; mark-end of
        // bootstrap initialization next; task manager last) follows
        // the documented Lean embedding sequence captured in
        // `crates/lean-rs-sys/src/init.rs`. None of the calls take
        // inputs from Rust state (the `workers` count is a plain
        // `u32` read from the environment above), so there is no
        // aliasing or lifetime hazard.
        //
        // `lean_io_mark_end_initialization()` flips Lean's
        // `IO.initializing` flag to `false`. Several Lean APIs gate on
        // this flag—most notably `Lean.mkEmptyEnvironment` (called
        // transitively by `Lean.Parser.parseHeader`), which throws
        // `IO.userError "environment objects cannot be created during
        // initialization"` otherwise. Omitting this call leaves the
        // runtime stuck in pre-init mode forever; downstream module
        // initializers loaded via `LeanLibrary::initialize_module`
        // still run normally because Lake-emitted initializers do not
        // check the flag.
        //
        // The task manager is required for any code path that spawns
        // Lean tasks—including `Lean.Elab.Frontend.process` (driven
        // by `kernel_check`), which would otherwise abort with a
        // "g_task_manager" assertion on the first
        // `Language.Lean.processCommands` call. Lean tears the
        // manager down at process exit; we hold no Drop handle here
        // because the runtime itself is intentionally process-lifetime.
        unsafe {
            lean_rs_sys::init::lean_initialize_runtime_module();
            lean_rs_sys::init::lean_initialize();
            lean_rs_sys::io::lean_io_mark_end_initialization();
            match workers {
                Some(n) => lean_rs_sys::init::lean_init_task_manager_using(n),
                None => lean_rs_sys::init::lean_init_task_manager(),
            }
        }
    }));
    match outcome {
        Ok(()) => Ok(()),
        Err(payload) => Err(LeanError::runtime_init_panic(payload.as_ref())),
    }
}

/// Parse the `LEAN_RS_NUM_THREADS` environment variable for the worker
/// count to hand to `lean_init_task_manager_using`.
///
/// Returns `Some(n)` for any positive integer; returns `None` (Lean's
/// compiled-in default) if the variable is unset or holds a value that
/// is not a positive integer. Invalid values emit a single `warn!`
/// against the `lean_rs` target so the operator notices the typo without
/// breaking the run.
fn read_num_threads_env() -> Option<u32> {
    let raw = std::env::var("LEAN_RS_NUM_THREADS").ok()?;
    match raw.trim().parse::<u32>() {
        Ok(n) if n >= 1 => Some(n),
        _ => {
            tracing::warn!(
                target: "lean_rs",
                value = %raw,
                "LEAN_RS_NUM_THREADS must be a positive integer; falling back to the Lean default",
            );
            None
        }
    }
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
