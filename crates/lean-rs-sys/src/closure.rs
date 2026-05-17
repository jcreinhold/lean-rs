//! Closure objects — externs and inline accessors from `lean.h:762–813`.

#![allow(clippy::inline_always)]
// `arity` and `num_fixed` are stored as `u16` in the Lean closure object,
// but the public allocator takes them as `u32` to match Lean's compiler ABI.
// The narrowing is bounded by `LEAN_CLOSURE_MAX_ARGS == 16`, asserted via
// `debug_assert!` below.
#![allow(clippy::cast_possible_truncation)]

use core::ffi::c_void;

use crate::repr::LeanClosureObjectRepr;
use crate::types::{b_lean_obj_arg, lean_obj_arg, lean_obj_res, lean_object, u_lean_obj_arg};

unsafe extern "C" {
    pub fn lean_apply_1(f: *mut lean_object, a1: *mut lean_object) -> *mut lean_object;
    pub fn lean_apply_2(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_3(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_4(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_5(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_6(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_7(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_8(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_9(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_10(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
        a10: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_11(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
        a10: *mut lean_object,
        a11: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_12(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
        a10: *mut lean_object,
        a11: *mut lean_object,
        a12: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_13(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
        a10: *mut lean_object,
        a11: *mut lean_object,
        a12: *mut lean_object,
        a13: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_14(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
        a10: *mut lean_object,
        a11: *mut lean_object,
        a12: *mut lean_object,
        a13: *mut lean_object,
        a14: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_15(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
        a10: *mut lean_object,
        a11: *mut lean_object,
        a12: *mut lean_object,
        a13: *mut lean_object,
        a14: *mut lean_object,
        a15: *mut lean_object,
    ) -> *mut lean_object;
    pub fn lean_apply_16(
        f: *mut lean_object,
        a1: *mut lean_object,
        a2: *mut lean_object,
        a3: *mut lean_object,
        a4: *mut lean_object,
        a5: *mut lean_object,
        a6: *mut lean_object,
        a7: *mut lean_object,
        a8: *mut lean_object,
        a9: *mut lean_object,
        a10: *mut lean_object,
        a11: *mut lean_object,
        a12: *mut lean_object,
        a13: *mut lean_object,
        a14: *mut lean_object,
        a15: *mut lean_object,
        a16: *mut lean_object,
    ) -> *mut lean_object;

    /// Apply a closure to `n` arguments, where `n <= 16`.
    pub fn lean_apply_n(
        f: *mut lean_object,
        n: u32,
        args: *mut *mut lean_object,
    ) -> *mut lean_object;
    /// Apply a closure to `n` arguments, where `n > 16`.
    pub fn lean_apply_m(
        f: *mut lean_object,
        n: u32,
        args: *mut *mut lean_object,
    ) -> *mut lean_object;
}

#[inline(always)]
unsafe fn as_closure<'a>(o: *mut lean_object) -> &'a LeanClosureObjectRepr {
    // SAFETY: caller asserts `o` is a Lean closure object.
    unsafe { &*o.cast::<LeanClosureObjectRepr>() }
}

/// Code pointer of the closure (`lean.h:764`).
///
/// # Safety
///
/// `o` must be a borrowed Lean closure object.
#[inline(always)]
pub unsafe fn lean_closure_fun(o: *mut lean_object) -> *mut c_void {
    // SAFETY: precondition above.
    unsafe { as_closure(o).fun }
}

/// Arity of the closure's underlying function (`lean.h:765`).
///
/// # Safety
///
/// Same as [`lean_closure_fun`].
#[inline(always)]
pub unsafe fn lean_closure_arity(o: *mut lean_object) -> u16 {
    // SAFETY: precondition above.
    unsafe { as_closure(o).arity }
}

/// Number of arguments already captured in the closure (`lean.h:766`).
///
/// # Safety
///
/// Same as [`lean_closure_fun`].
#[inline(always)]
pub unsafe fn lean_closure_num_fixed(o: *mut lean_object) -> u16 {
    // SAFETY: precondition above.
    unsafe { as_closure(o).num_fixed }
}

/// Pointer to the closure's captured-argument array (`lean.h:767`).
///
/// # Safety
///
/// Same as [`lean_closure_fun`]; valid for `lean_closure_num_fixed(o)`
/// elements.
#[inline(always)]
pub unsafe fn lean_closure_arg_cptr(o: *mut lean_object) -> *mut *mut lean_object {
    // SAFETY: precondition above; flexible-array member offset is fixed.
    unsafe { (&raw mut (*o.cast::<LeanClosureObjectRepr>()).objs).cast::<*mut lean_object>() }
}

/// Read the i-th captured argument (`lean.h:778`).
///
/// # Safety
///
/// `o` must be a borrowed Lean closure object and `i < num_fixed(o)`.
#[inline(always)]
pub unsafe fn lean_closure_get(o: b_lean_obj_arg, i: u32) -> *mut lean_object {
    // SAFETY: precondition above.
    unsafe { *lean_closure_arg_cptr(o).add(i as usize) }
}

/// Write the i-th captured argument (`lean.h:782`).
///
/// # Safety
///
/// `o` must be a unique Lean closure object, `i < num_fixed(o)`, and the
/// caller transfers ownership of `a`.
#[inline(always)]
pub unsafe fn lean_closure_set(o: u_lean_obj_arg, i: u32, a: lean_obj_arg) {
    // SAFETY: precondition above.
    unsafe { *lean_closure_arg_cptr(o).add(i as usize) = a }
}

/// Allocate a closure of the requested arity with no captured arguments
/// pre-bound (`lean.h:768–777`).
///
/// # Safety
///
/// `arity > 0`, `num_fixed < arity`. The returned object has tag
/// `LeanClosure`, refcount 1, and uninitialized captured-arg storage; the
/// caller must `lean_closure_set` every captured slot before invoking the
/// closure.
///
/// # Panics
///
/// Panics if the closure's total allocation size overflows `usize`. This is
/// impossible under Lean's `LEAN_CLOSURE_MAX_ARGS == 16` cap; the panic
/// surfaces a runtime invariant breach rather than a recoverable condition.
#[inline(always)]
pub unsafe fn lean_alloc_closure(fun: *mut c_void, arity: u32, num_fixed: u32) -> lean_obj_res {
    debug_assert!(arity > 0);
    debug_assert!(num_fixed < arity);
    // The product is bounded by `LEAN_CLOSURE_MAX_ARGS == 16`; a `usize`
    // overflow is impossible on any supported target. `strict_*` panics on
    // overflow so we surface a runtime invariant breach instead of silently
    // mis-sizing the allocation.
    let captured_bytes = core::mem::size_of::<*mut lean_object>().strict_mul(num_fixed as usize);
    let size = core::mem::size_of::<LeanClosureObjectRepr>().strict_add(captured_bytes);
    // SAFETY: `lean_alloc_object` is the runtime's small-object allocator;
    // we initialize the header + fun + arity + num_fixed before returning.
    unsafe {
        let raw = crate::object::lean_alloc_object(size);
        let repr = raw.cast::<LeanClosureObjectRepr>();
        (*repr).header.m_rc = 1;
        (*repr).header.m_tag = crate::consts::LEAN_CLOSURE;
        (*repr).header.m_other = 0;
        (*repr).header.m_cs_sz = 0;
        (*repr).fun = fun;
        (*repr).arity = arity as u16;
        (*repr).num_fixed = num_fixed as u16;
        raw
    }
}
