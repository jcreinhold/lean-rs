//! Refcount fast paths—Rust mirrors of `lean.h:536–563`.
//!
//! The header encodes the runtime mode in the RC sign:
//! - `m_rc > 0` —single-threaded: bump or decrement in place.
//! - `m_rc < 0` —multi-threaded: RC is negated; `fetch_sub` is the relaxed
//!   atomic decrement, mirroring `atomic_fetch_sub_explicit(.., relaxed)` in
//!   `lean.h`.
//! - `m_rc == 0`—persistent (compact regions); never refcounted.
//!
//! Each mirror reaches `m_rc` through [`AtomicI32::from_ptr`] so the actual
//! load / store / `fetch_sub` call site sees a safe `&AtomicI32`.

// These mirrors of `lean.h`'s `static inline` helpers must inline through the
// FFI boundary for the fast paths to be free; that is the design.
#![allow(clippy::inline_always)]

use core::sync::atomic::{AtomicI32, Ordering};

use crate::object::lean_is_scalar;
use crate::repr::LeanObjectRepr;
use crate::types::lean_object;

unsafe extern "C" {
    /// Cold path for refcount-zero decrement (`lean.h:552`). Invoked by
    /// [`lean_dec_ref`] when the live count would cross to zero and the
    /// object actually needs freeing.
    pub fn lean_dec_ref_cold(o: *mut lean_object);

    /// Mark a heap object—and everything reachable from it—as
    /// multi-threaded (`lean.h:612`).
    pub fn lean_mark_mt(o: *mut lean_object);

    /// Mark a heap object as persistent so its refcount is no longer
    /// updated (`lean.h:613`).
    pub fn lean_mark_persistent(o: *mut lean_object);
}

#[inline(always)]
unsafe fn rc_atom<'a>(o: *mut lean_object) -> &'a AtomicI32 {
    // SAFETY: caller guarantees `o` is a valid non-scalar Lean heap object.
    // `LeanObjectRepr`'s layout is pinned by `LEAN_HEADER_DIGEST`; the
    // `m_rc` field is at offset 0. `AtomicI32::from_ptr` requires the
    // pointer to be aligned (Lean allocates `lean_object`s with at least
    // 4-byte alignment—pinned by the same digest) and valid for shared
    // access for the duration of `'a`.
    unsafe {
        let repr = o.cast::<LeanObjectRepr>();
        AtomicI32::from_ptr(&raw mut (*repr).m_rc)
    }
}

/// Bump `o`'s reference count by `n` (`lean.h:536–546`, `lean_inc_ref_n`).
///
/// # Safety
///
/// `o` must be a valid non-scalar Lean object pointer per the `lean_obj_arg`
/// calling convention; the caller must ensure the increment cannot overflow
/// (Lean's runtime preserves this invariant by construction). The layout of
/// `lean_object` matches the crate-private `LeanObjectRepr` per the
/// build-time digest check.
///
/// # Panics
///
/// Panics if adding `n` to the single-threaded refcount would overflow
/// `i32`. This indicates a runtime invariant breach (an object referenced
/// >2 billion times) rather than a recoverable condition.
#[inline(always)]
pub unsafe fn lean_inc_ref_n(o: *mut lean_object, n: usize) {
    // SAFETY: precondition above; `n` fits in i32 in any realistic program.
    let rc = unsafe { rc_atom(o) };
    let cur = rc.load(Ordering::Relaxed);
    let delta = n as i32;
    if cur > 0 {
        // `strict_add` panics on overflow in both debug and release; a
        // refcount overflow indicates a runtime invariant breach (a single
        // object held by >2 billion references), not a recoverable
        // condition.
        rc.store(cur.strict_add(delta), Ordering::Relaxed);
    } else if cur != 0 {
        // Multi-threaded: m_rc is negated, so increment-by-n is fetch_sub(n).
        rc.fetch_sub(delta, Ordering::Relaxed);
    }
}

/// Bump `o`'s refcount by one (`lean.h:548–550`).
///
/// # Safety
///
/// Same as [`lean_inc_ref_n`].
#[inline(always)]
pub unsafe fn lean_inc_ref(o: *mut lean_object) {
    // SAFETY: forwards to lean_inc_ref_n with the same precondition.
    unsafe { lean_inc_ref_n(o, 1) }
}

/// Bump `o`'s refcount by one, or no-op for scalar-tagged pointers
/// (`lean.h:561`).
///
/// # Safety
///
/// `o` must be either a scalar-tagged pointer (low bit set) or a valid Lean
/// heap object. The scalar branch is unconditionally safe; the heap branch
/// inherits [`lean_inc_ref`]'s precondition.
#[inline(always)]
pub unsafe fn lean_inc(o: *mut lean_object) {
    // SAFETY: scalar check inspects pointer bits only; the heap branch
    // inherits `lean_inc_ref`'s precondition (non-scalar object).
    unsafe {
        if !lean_is_scalar(o) {
            lean_inc_ref(o);
        }
    }
}

/// Bump `o`'s refcount by `n`, or no-op for scalar-tagged pointers
/// (`lean.h:562`).
///
/// # Safety
///
/// Same as [`lean_inc`].
#[inline(always)]
pub unsafe fn lean_inc_n(o: *mut lean_object, n: usize) {
    // SAFETY: scalar check inspects pointer bits only; the heap branch
    // inherits `lean_inc_ref_n`'s precondition (non-scalar object).
    unsafe {
        if !lean_is_scalar(o) {
            lean_inc_ref_n(o, n);
        }
    }
}

/// Decrement `o`'s refcount, taking the cold path to free if it reaches zero
/// (`lean.h:554–560`).
///
/// # Safety
///
/// `o` must be a valid non-scalar Lean heap object. The runtime invariant
/// that `m_rc == 0` means "persistent, no refcounting" is honoured: this
/// function is a no-op in that case.
///
/// # Panics
///
/// The single-threaded fast path only runs when `m_rc > 1`, so subtracting
/// 1 cannot underflow; the panic guard exists to catch a future invariant
/// breach (e.g. unsynchronized concurrent mutation) rather than a
/// recoverable condition.
#[inline(always)]
pub unsafe fn lean_dec_ref(o: *mut lean_object) {
    // SAFETY: precondition above; cold path is `LEAN_EXPORT`'d.
    let rc = unsafe { rc_atom(o) };
    let cur = rc.load(Ordering::Relaxed);
    if cur > 1 {
        // `cur > 1` so subtracting 1 cannot wrap; `strict_sub` surfaces any
        // future invariant breach as a panic instead of silently wrapping.
        rc.store(cur.strict_sub(1), Ordering::Relaxed);
    } else if cur != 0 {
        // Either ST with cur==1, or MT with negated count whose abs value is
        // the live count. The C source calls `lean_dec_ref_cold` in either
        // case; the cold path handles MT subtraction internally.
        // SAFETY: cold path is the only safe way to free; it takes ownership.
        unsafe { lean_dec_ref_cold(o) }
    }
}

/// Decrement `o`'s refcount, or no-op for scalar-tagged pointers
/// (`lean.h:563`).
///
/// # Safety
///
/// Same as [`lean_inc`].
#[inline(always)]
pub unsafe fn lean_dec(o: *mut lean_object) {
    // SAFETY: scalar check inspects pointer bits only; heap branch inherits
    // `lean_dec_ref`'s precondition (non-scalar Lean heap object).
    unsafe {
        if !lean_is_scalar(o) {
            lean_dec_ref(o);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pure-Rust coverage of the refcount fast paths over a synthetic,
    //! Rust-owned object header. These never cross the refcount to zero, so
    //! the `lean_dec_ref_cold` extern is never reached and the tests run
    //! unchanged under `cargo miri test`—where they validate the
    //! `AtomicI32::from_ptr` provenance and alignment the FFI relies on.

    use super::{lean_dec, lean_dec_ref, lean_inc, lean_inc_n, lean_inc_ref, lean_inc_ref_n};
    use crate::object::lean_box;
    use crate::repr::test_support::MockObject;

    #[test]
    fn single_threaded_inc_dec_round_trips() {
        let obj = MockObject::new(1, 0, 0);
        let o = obj.ptr();
        // SAFETY: `o` is a valid non-scalar header; rc stays > 0 throughout,
        // so the decrement never reaches the cold path.
        unsafe {
            lean_inc_ref(o);
            assert_eq!(obj.rc(), 2);
            lean_dec_ref(o);
            assert_eq!(obj.rc(), 1);
        }
    }

    #[test]
    fn inc_ref_n_adds_in_place() {
        let obj = MockObject::new(1, 0, 0);
        let o = obj.ptr();
        // SAFETY: as above; only the single-threaded store path runs.
        unsafe {
            lean_inc_ref_n(o, 9);
        }
        assert_eq!(obj.rc(), 10);
    }

    #[test]
    fn multi_threaded_inc_decrements_negated_count() {
        // Multi-threaded objects store the live count negated; an increment is
        // a `fetch_sub`, so the field moves further negative.
        let obj = MockObject::new(-1, 0, 0);
        let o = obj.ptr();
        // SAFETY: valid non-scalar header; the MT branch is a relaxed
        // `fetch_sub` with no cold-path dispatch.
        unsafe {
            lean_inc_ref(o);
        }
        assert_eq!(obj.rc(), -2);
    }

    #[test]
    fn scalar_pointers_are_left_untouched() {
        // A scalar-tagged pointer never aliases an allocation; inc/dec must be
        // pure pointer-bit no-ops that touch no memory.
        // SAFETY: `lean_box` is pointer arithmetic; `lean_inc`/`lean_dec`
        // inspect the scalar tag and return without dereferencing.
        unsafe {
            let scalar = lean_box(42);
            lean_inc(scalar);
            lean_inc_n(scalar, 7);
            lean_dec(scalar);
            assert_eq!(scalar, lean_box(42));
        }
    }
}
