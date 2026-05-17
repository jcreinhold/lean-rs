//! Lifetime-bound owned and borrowed Lean object handles.
//!
//! [`Obj`] is the only safe owner of a Lean reference count inside
//! `lean-rs`. [`Clone`] performs `lean_inc`, [`Drop`] performs
//! `lean_dec`, and both delegate to the `pub unsafe fn` mirrors in
//! [`lean_rs_sys::refcount`] (the inlined fast paths). [`ObjRef`] is the
//! borrowed view: it costs nothing on construction or drop and cannot
//! outlive either the runtime borrow or the owning [`Obj`].
//!
//! Both types are `pub(crate)` per `RD-2026-05-17-004`; the public
//! handles introduced by later prompts (`LeanExpr<'lean>`,
//! `LeanName<'lean>`, `LeanSession<'lean, '_>`, …) wrap them. Raw
//! `lean_*` symbols enter this file from `lean-rs-sys` and never leave
//! the `pub(crate)` boundary.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment naming the invariant. The blanket allow keeps
// the unsafe surface inside the smallest scope that compiles, per
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]
// `Obj` / `ObjRef` and their methods are infrastructure consumed by the
// `pub(crate) abi` module (prompt 08) and by the typed
// `LeanExported{N}` / `LeanSession` machinery added in prompts 09–18.
// The library build sees them as dead until a non-test caller lands;
// only `cargo test` exercises them (transitively through `abi::tests`).
#![allow(dead_code, reason = "non-test callers land in prompts 09–18")]

use core::fmt;
use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::ptr::NonNull;

use lean_rs_sys::lean_object;
use lean_rs_sys::refcount::{lean_dec, lean_inc};

use crate::runtime::LeanRuntime;

/// Owned handle to a Lean heap or scalar object.
///
/// `Obj<'lean>` holds exactly one Lean reference count for the duration
/// of its scope. [`Clone`] bumps that count via `lean_inc`; [`Drop`]
/// releases it via `lean_dec`. The `'lean` lifetime parameter is the
/// type-system anchor for "init-before-use": the constructor takes a
/// `&'lean LeanRuntime` borrow, so the handle cannot exist before the
/// runtime is up and cannot outlive the borrow it was derived from.
///
/// Neither [`Send`] nor [`Sync`]: the contained `NonNull<lean_object>`
/// and the `PhantomData<&'lean LeanRuntime>` (where `LeanRuntime: !Sync`)
/// both propagate the per-thread restriction. A negative-bound static
/// assertion in [`tests`] fails to compile if either auto-trait is ever
/// implemented.
pub(crate) struct Obj<'lean> {
    ptr: NonNull<lean_object>,
    _life: PhantomData<&'lean LeanRuntime>,
}

/// Borrowed view of an [`Obj`].
///
/// `ObjRef<'lean, 'a>` carries no refcount obligation: construction and
/// destruction are no-ops. The double phantom records both the runtime
/// borrow (`'lean`) and the borrow of the owning `Obj<'lean>` (`'a`);
/// the latter prevents a view from outliving its source.
pub(crate) struct ObjRef<'lean, 'a> {
    ptr: NonNull<lean_object>,
    _life: PhantomData<(&'lean LeanRuntime, &'a Obj<'lean>)>,
}

impl<'lean> Obj<'lean> {
    /// Wrap a raw owned Lean pointer.
    ///
    /// # Safety
    ///
    /// The caller must guarantee all of the following:
    ///
    /// * `ptr` is non-null and points to a live Lean object (a
    ///   scalar-tagged pointer is allowed; `lean_inc` / `lean_dec`
    ///   short-circuit on those).
    /// * The caller transfers exactly one Lean reference count to this
    ///   handle. After this call the caller must not call `lean_dec`
    ///   on `ptr` itself; releasing the count is the new `Obj`'s job
    ///   (via [`Drop`] or [`Obj::into_raw`]).
    /// * `ptr` was produced by the same Lean runtime instance witnessed
    ///   by `_runtime`. The borrow is a type-level proof that Lean is
    ///   initialised and pins the `'lean` lifetime of the returned
    ///   handle; it carries no payload of its own.
    pub(crate) unsafe fn from_owned_raw(_runtime: &'lean LeanRuntime, ptr: *mut lean_object) -> Self {
        // SAFETY: caller guarantees `ptr` is non-null (documented above);
        // `NonNull::new_unchecked` then carries the invariant in the
        // type. Pointer provenance and refcount transfer are the
        // caller's obligations.
        let ptr = unsafe { NonNull::new_unchecked(ptr) };
        Self {
            ptr,
            _life: PhantomData,
        }
    }

    /// Consume the handle and return the underlying raw owned pointer
    /// without running [`Drop`].
    ///
    /// The returned pointer carries the same one reference count that
    /// this `Obj` held. Whoever receives it inherits the obligation to
    /// release it (via `lean_dec`, or by reconstructing an `Obj` with
    /// [`Obj::from_owned_raw`]).
    pub(crate) fn into_raw(self) -> *mut lean_object {
        // `ManuallyDrop` blocks the destructor so we do not decrement
        // the count on the way out — the caller takes ownership.
        ManuallyDrop::new(self).ptr.as_ptr()
    }

    /// Borrow the underlying raw pointer for a borrowed-argument FFI
    /// call (`b_lean_obj_arg`).
    ///
    /// The returned pointer must not be passed where a Lean function
    /// expects to consume one reference count; that is what
    /// [`Obj::into_raw`] is for.
    pub(crate) fn as_raw_borrowed(&self) -> *mut lean_object {
        self.ptr.as_ptr()
    }

    /// Produce a borrowed view tied to this `Obj`.
    pub(crate) fn borrow(&self) -> ObjRef<'lean, '_> {
        ObjRef {
            ptr: self.ptr,
            _life: PhantomData,
        }
    }

    /// Recover the runtime borrow that anchors this handle's `'lean`.
    ///
    /// `LeanRuntime` is a ZST whose only construction path goes through
    /// [`LeanRuntime::init`]; the `'lean` lifetime carried by `self` is
    /// already a witness that a runtime borrow is live in the caller's
    /// scope. Container readers (e.g. `Vec<T>::try_from_lean`) use this to
    /// wrap extracted fields as fresh `Obj<'lean>` values without forcing
    /// the runtime through every signature in the trait surface.
    ///
    /// `&self` rather than an associated function because the borrow
    /// pins the inferred `'lean` lifetime to this `Obj`'s lifetime —
    /// callers do not need to spell the parameter out at the call site.
    #[allow(clippy::unused_self, reason = "`&self` pins the inferred 'lean lifetime parameter")]
    pub(crate) fn runtime(&self) -> &'lean LeanRuntime {
        // SAFETY: `LeanRuntime` is zero-sized; `NonNull::dangling()`
        // produces an aligned non-null pointer suitable for a ZST borrow.
        // The Lean runtime is alive whenever `'lean` is alive, so the
        // synthesised reference is indistinguishable from the original
        // `&LeanRuntime` borrow that witnessed `self`'s construction.
        unsafe { NonNull::<LeanRuntime>::dangling().as_ref() }
    }
}

impl ObjRef<'_, '_> {
    /// Borrow the underlying raw pointer for a borrowed-argument FFI
    /// call (`b_lean_obj_arg`).
    pub(crate) fn as_raw_borrowed(&self) -> *mut lean_object {
        self.ptr.as_ptr()
    }
}

impl Clone for Obj<'_> {
    fn clone(&self) -> Self {
        // SAFETY: `self.ptr` is a live Lean object whose ownership we
        // hold (refcount is at least 1); `lean_inc` short-circuits on
        // scalar-tagged pointers and otherwise bumps the heap refcount.
        // The Lean runtime is initialised because we hold an `Obj`
        // bound by `'lean` to a `&LeanRuntime` borrow.
        unsafe { lean_inc(self.ptr.as_ptr()) }
        Self {
            ptr: self.ptr,
            _life: PhantomData,
        }
    }
}

impl Drop for Obj<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` is the live Lean object whose one owned
        // refcount this handle is about to release; `lean_dec` handles
        // the scalar short-circuit and the cold-free path. The runtime
        // is still initialised because `'lean` is alive while `self`
        // is alive.
        unsafe { lean_dec(self.ptr.as_ptr()) }
    }
}

impl fmt::Debug for Obj<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Pointer-only: never dereference. Header inspection belongs in
        // the `lean-rs-sys` predicates (`lean_obj_tag`,
        // `lean_is_exclusive`, …), not in `Debug`.
        f.debug_struct("Obj").field("ptr", &self.ptr.as_ptr()).finish()
    }
}

impl fmt::Debug for ObjRef<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjRef").field("ptr", &self.ptr.as_ptr()).finish()
    }
}

#[cfg(test)]
mod tests {
    //! `cargo test -p lean-rs --lib runtime::obj::tests`
    //!
    //! Tests share a process; `LeanRuntime::init()` is the same cell
    //! used by `runtime::tests`. The refcount-observation tests rely on
    //! the `pub unsafe fn` predicates in `lean-rs-sys::object`
    //! (`lean_is_exclusive`, `lean_is_shared`) — neither dereferences
    //! the object payload, only the header's refcount.

    #![allow(clippy::expect_used)]

    use core::ffi::c_char;

    use lean_rs_sys::object::{lean_box, lean_is_exclusive, lean_is_shared};
    use lean_rs_sys::refcount::lean_dec;
    use lean_rs_sys::string::lean_mk_string;

    use super::{Obj, ObjRef};
    use crate::runtime::LeanRuntime;

    /// Build a scalar-tagged handle for tests that only need to
    /// exercise pointer-handling paths. `lean_inc` / `lean_dec`
    /// short-circuit on scalars, so refcount math does not run.
    fn scalar_obj(runtime: &LeanRuntime) -> Obj<'_> {
        // SAFETY: `lean_box` is pure pointer arithmetic; the resulting
        // scalar-tagged pointer is non-null and the refcount helpers
        // treat it as a no-op.
        unsafe { Obj::from_owned_raw(runtime, lean_box(7)) }
    }

    /// Build a freshly-allocated heap string (`refcount == 1`).
    fn heap_string(runtime: &LeanRuntime) -> Obj<'_> {
        let cstr = c"abc".as_ptr().cast::<c_char>();
        // SAFETY: `cstr` is a valid NUL-terminated UTF-8 byte string
        // with `'static` lifetime; `lean_mk_string` returns a
        // `lean_obj_res` (owned, refcount 1). The runtime borrow
        // witnesses that Lean is initialised.
        unsafe { Obj::from_owned_raw(runtime, lean_mk_string(cstr)) }
    }

    #[test]
    fn scalar_construction_and_drop_is_a_noop() {
        let runtime = LeanRuntime::init().expect("runtime init must succeed");
        let obj = scalar_obj(runtime);
        // Drop at scope exit must not panic. The wrapper is wired to
        // `lean_dec`, which short-circuits on scalar-tagged pointers.
        drop(obj);
    }

    #[test]
    fn clone_increments_heap_refcount() {
        let runtime = LeanRuntime::init().expect("runtime init must succeed");
        let obj = heap_string(runtime);

        // SAFETY: `obj` owns the only reference; predicate only inspects
        // the header refcount.
        assert!(unsafe { lean_is_exclusive(obj.as_raw_borrowed()) });

        let copy = obj.clone();
        // SAFETY: `obj` and `copy` are both live owners.
        assert!(unsafe { lean_is_shared(obj.as_raw_borrowed()) });
        assert!(unsafe { lean_is_shared(copy.as_raw_borrowed()) });

        drop(copy);
        // SAFETY: only `obj` remains; refcount must be back to 1.
        assert!(unsafe { lean_is_exclusive(obj.as_raw_borrowed()) });
    }

    #[test]
    fn into_raw_does_not_decrement() {
        let runtime = LeanRuntime::init().expect("runtime init must succeed");
        let obj = heap_string(runtime);
        let witness = obj.clone();
        // Both handles live, refcount == 2.
        // SAFETY: header-only inspection.
        assert!(unsafe { lean_is_shared(witness.as_raw_borrowed()) });

        let raw = obj.into_raw();
        // `into_raw` transferred ownership without decrementing; the
        // witness must still see refcount == 2.
        // SAFETY: header-only inspection.
        assert!(unsafe { lean_is_shared(witness.as_raw_borrowed()) });

        // SAFETY: `raw` is the one reference count produced by `obj`;
        // releasing it here pairs the transfer and keeps `witness`
        // sole owner.
        unsafe { lean_dec(raw) };
        // SAFETY: header-only inspection.
        assert!(unsafe { lean_is_exclusive(witness.as_raw_borrowed()) });
    }

    #[test]
    fn borrow_does_not_adjust_refcount() {
        let runtime = LeanRuntime::init().expect("runtime init must succeed");
        let obj = heap_string(runtime);
        // SAFETY: header-only inspection.
        assert!(unsafe { lean_is_exclusive(obj.as_raw_borrowed()) });

        let view: ObjRef<'_, '_> = obj.borrow();
        let raw = view.as_raw_borrowed();
        assert_eq!(raw, obj.as_raw_borrowed());

        // SAFETY: header-only inspection; the borrow did not touch RC.
        assert!(unsafe { lean_is_exclusive(obj.as_raw_borrowed()) });
        // The view is a `Copy`-shaped no-op view; falling out of scope
        // is the natural release path. Re-check the predicate after the
        // last use to confirm no implicit RC adjustment happened.
        let _ = view;
        // SAFETY: header-only inspection.
        assert!(unsafe { lean_is_exclusive(obj.as_raw_borrowed()) });
    }

    #[test]
    fn debug_format_renders_pointer_without_dereferencing() {
        let runtime = LeanRuntime::init().expect("runtime init must succeed");
        let obj = scalar_obj(runtime);
        let rendered = format!("{obj:?}");
        // The shape is `Obj { ptr: 0x… }`; the contract is that
        // formatting never reads through the pointer.
        assert!(rendered.starts_with("Obj"));
        assert!(rendered.contains("ptr"));

        let view = obj.borrow();
        let rendered_ref = format!("{view:?}");
        assert!(rendered_ref.starts_with("ObjRef"));
    }

    /// `Obj<'lean>` is `!Send` and `!Sync`. Both checks use the
    /// canonical `AmbiguousIfSend` ZST trick (`static_assertions`-style)
    /// so this test module fails to compile if either auto-trait is
    /// ever implemented for `Obj`.
    ///
    /// The marker structs (`Invalid`, `InvalidSync`) and the helper
    /// traits exist only as type-level arguments to the trait selector;
    /// they are never constructed or called dynamically, so we silence
    /// `dead_code` locally rather than relax it crate-wide.
    #[allow(dead_code, reason = "items are consumed only via trait selection at compile time")]
    mod not_send_not_sync {
        use super::Obj;

        trait AmbiguousIfSend<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {}
        struct Invalid;
        impl<T: ?Sized + Send> AmbiguousIfSend<Invalid> for T {}

        trait AmbiguousIfSync<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSync<()> for T {}
        struct InvalidSync;
        impl<T: ?Sized + Sync> AmbiguousIfSync<InvalidSync> for T {}

        fn _obj_is_not_send() {
            // Compile-time: trait selection is ambiguous iff
            // `Obj<'static>: Send`, which yields E0283 — that is the
            // compile-fail signal this assertion produces. Picking
            // `'static` is fine: `Send`/`Sync` are lifetime-invariant.
            <Obj<'static> as AmbiguousIfSend<_>>::check();
        }

        fn _obj_is_not_sync() {
            <Obj<'static> as AmbiguousIfSync<_>>::check();
        }
    }

    /// Positive lifetime check: the returned `Obj<'_>` is tied to the
    /// input runtime borrow, not `'static`. If a future refactor
    /// weakened the lifetime — e.g. by erasing the `&LeanRuntime`
    /// argument or returning `Obj<'static>` — the borrow checker would
    /// refuse this function and any caller that tried to outlive the
    /// runtime borrow.
    fn _lifetime_anchored_to_runtime_borrow(runtime: &LeanRuntime) -> Obj<'_> {
        // SAFETY: `lean_box` produces a scalar-tagged pointer; the
        // refcount helpers treat it as a no-op.
        unsafe { Obj::from_owned_raw(runtime, lean_box(0)) }
    }
}
