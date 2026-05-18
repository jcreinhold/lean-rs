//! RAII attach guard for OS threads that did not originate inside Lean.
//!
//! Code that calls into Lean from a thread the Lean runtime did not start
//! itself must attach that thread before any Lean work and detach it
//! before the thread exits. [`LeanThreadGuard`] is the RAII type that
//! does both: [`LeanThreadGuard::attach`] performs the attach,
//! [`LeanThreadGuard::drop`] performs the detach.
//!
//! Every thread that successfully called [`super::LeanRuntime::init`] is
//! implicitly marked as permitted to invoke Lean from that thread. The
//! [`debug_assert_attached`] check at the host-call funnel is therefore
//! a defensive instrument: in correctly-typed code it always passes
//! (every host call requires a `&'lean LeanRuntime`, which is `!Send`,
//! so the holding thread necessarily went through `init` on this
//! thread). The assertion catches unsafe-code misuse and accidental
//! `Send` impls.

// SAFETY DOC: each `unsafe { ... }` block carries its own `// SAFETY:`
// comment. The blanket allow keeps the unsafe surface in the smallest
// `pub(crate)` scope that compiles, per the safety model in
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use std::cell::Cell;
use std::marker::PhantomData;

use super::LeanRuntime;

thread_local! {
    /// Per-thread attach depth. `0` means the current OS thread has
    /// never called [`super::LeanRuntime::init`] on this thread and does
    /// not hold a [`LeanThreadGuard`]; invoking Lean from such a thread
    /// is a soundness bug.
    ///
    /// Every successful `init()` call lifts the floor to `1` (via
    /// [`mark_calling_thread_permitted`]). Each [`LeanThreadGuard`]
    /// further increments on `attach` and decrements on `Drop`.
    /// Nesting is legal and pairs depth-for-depth.
    static ATTACH_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Mark the calling thread as permitted to invoke Lean from this thread.
///
/// Called from [`super::LeanRuntime::init`] on every successful return,
/// not only the first. Subsequent calls on the same thread are no-ops:
/// the depth never drops below the permanent floor of `1` that `init`
/// installs. The init thread is, by this rule, also covered — it called
/// `init` on itself like every other client.
pub(crate) fn mark_calling_thread_permitted() {
    ATTACH_DEPTH.with(|d| {
        if d.get() == 0 {
            d.set(1);
        }
    });
}

/// Debug-mode assertion that the current thread is attached to Lean.
///
/// Inserted at every host call funnel (notably
/// [`crate::module::LeanExported::call`]) so that a worker thread that
/// forgets to construct a [`LeanThreadGuard`] panics with a clear Rust
/// message instead of tripping a Lean-side assertion. Compiles to a
/// no-op in release builds.
///
/// The `stage` argument is the textual label included in the panic
/// message; pass a `&'static str` identifying the call site.
pub(crate) fn debug_assert_attached(stage: &'static str) {
    debug_assert!(
        ATTACH_DEPTH.with(|d| d.get()) >= 1,
        "lean-rs: {stage} invoked on a thread that is not attached to the Lean runtime; \
         construct a `lean_rs::LeanThreadGuard` on this thread first \
         (see docs/architecture/04-concurrency.md)",
    );
}

/// RAII handle that attaches the current OS thread to the Lean runtime.
///
/// When constructed via [`LeanThreadGuard::attach`], the guard registers
/// the calling thread with the Lean runtime so that thread may invoke
/// compiled Lean code. When the guard is dropped (or its scope ends), the
/// thread is detached again. Threads spawned by Lean itself are already
/// attached and do **not** need a guard; the thread that called
/// [`crate::LeanRuntime::init`] is likewise treated as attached for its
/// entire lifetime.
///
/// The guard is neither [`Send`] nor [`Sync`]: it represents a per-thread
/// resource, and shipping it to a different OS thread to be dropped would
/// detach the wrong thread. The `'lean` lifetime parameter ties the guard
/// to a [`LeanRuntime`] borrow, so a guard cannot outlive the runtime it
/// is bound to.
///
/// Nested attaches are legal: a worker thread already inside an outer
/// `LeanThreadGuard` may construct another (e.g. inside a callback) and
/// the per-thread attach depth tracks the pairs so the host-call
/// attach assertion does not fire prematurely.
///
/// # Example
///
/// ```ignore
/// // Inside a worker thread you spawned yourself:
/// let runtime = lean_rs::LeanRuntime::init()?;
/// let _guard = lean_rs::LeanThreadGuard::attach(runtime);
/// // ... call into Lean from this thread ...
/// // `_guard` is dropped at scope exit, detaching the thread cleanly.
/// ```
pub struct LeanThreadGuard<'lean> {
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
    #[must_use = "the guard detaches the thread on Drop; bind it to a name"]
    pub fn attach(_runtime: &'lean LeanRuntime) -> Self {
        // SAFETY: We hold a `&LeanRuntime` borrow, so the Lean runtime is
        // initialized on this process. Attaching the current OS thread has
        // no other precondition; the guard's `Drop` is the only path that
        // detaches, and the guard is `!Send` so it cannot be detached on
        // a different thread.
        unsafe {
            lean_rs_sys::init::lean_initialize_thread();
        }
        ATTACH_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
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
        ATTACH_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}
