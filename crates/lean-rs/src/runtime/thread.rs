//! RAII attach guard for OS threads that did not originate inside Lean.
//!
//! Code that calls into Lean from a thread the Lean runtime did not start
//! itself must attach that thread before any Lean work and detach it
//! before the thread exits. [`LeanThreadGuard`] is the RAII type that
//! does both: [`LeanThreadGuard::attach`] performs the attach,
//! [`LeanThreadGuard::drop`] performs the detach.

// SAFETY DOC: each `unsafe { ... }` block carries its own `// SAFETY:`
// comment. The blanket allow keeps the unsafe surface in the smallest
// `pub(crate)` scope that compiles, per the safety model in
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use std::marker::PhantomData;

use super::LeanRuntime;

/// RAII handle that attaches the current OS thread to the Lean runtime.
///
/// When constructed via [`LeanThreadGuard::attach`], the guard registers
/// the calling thread with the Lean runtime so that thread may invoke
/// compiled Lean code. When the guard is dropped (or its scope ends), the
/// thread is detached again. Threads spawned by Lean itself are already
/// attached and do **not** need a guard.
///
/// The guard is neither [`Send`] nor [`Sync`]: it represents a per-thread
/// resource, and shipping it to a different OS thread to be dropped would
/// detach the wrong thread. The `'lean` lifetime parameter ties the guard
/// to a [`LeanRuntime`] borrow, so a guard cannot outlive the runtime it
/// is bound to.
///
/// # Example
///
/// ```ignore
/// // Inside a worker thread you spawned yourself:
/// let runtime = lean_rs::LeanRuntime::init()?;
/// let _guard = lean_rs::runtime::thread::LeanThreadGuard::attach(runtime);
/// // ... call into Lean from this thread ...
/// // `_guard` is dropped at scope exit, detaching the thread cleanly.
/// ```
// Used by `runtime::tests` and reachable from the rest of the crate; the
// crate-root re-export is deferred to prompt 24 per
// `docs/architecture/03-host-api.md`. `expect` rather than `allow` so the
// lint fires once that re-export lands and this attribute can be removed.
#[allow(dead_code, reason = "public re-export deferred to prompt 24")]
pub(crate) struct LeanThreadGuard<'lean> {
    _runtime: PhantomData<&'lean LeanRuntime>,
    _no_send_no_sync: PhantomData<*mut ()>,
}

impl<'lean> LeanThreadGuard<'lean> {
    /// Attach the current OS thread to the Lean runtime.
    ///
    /// Construction requires a borrow of the process [`LeanRuntime`]
    /// handle; that borrow is the type-level proof that the runtime has
    /// been initialized on this process. The returned guard must be
    /// dropped on the same thread that attached it.
    #[allow(dead_code, reason = "public re-export deferred to prompt 24")]
    #[must_use = "the guard detaches the thread on Drop; bind it to a name"]
    pub(crate) fn attach(_runtime: &'lean LeanRuntime) -> Self {
        // SAFETY: We hold a `&LeanRuntime` borrow, so the Lean runtime is
        // initialized on this process. Attaching the current OS thread has
        // no other precondition; the guard's `Drop` is the only path that
        // detaches, and the guard is `!Send` so it cannot be detached on
        // a different thread.
        unsafe {
            lean_rs_sys::init::lean_initialize_thread();
        }
        Self {
            _runtime: PhantomData,
            _no_send_no_sync: PhantomData,
        }
    }
}

impl Drop for LeanThreadGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: Paired with the attach call in `LeanThreadGuard::attach`.
        // Construction of `LeanThreadGuard` is the only path that produces
        // a value of this type, and the type is `!Send`, so this `Drop`
        // necessarily runs on the same OS thread that performed the
        // attach.
        unsafe {
            lean_rs_sys::init::lean_finalize_thread();
        }
    }
}
