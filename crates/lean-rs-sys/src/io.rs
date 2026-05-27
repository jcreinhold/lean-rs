//! `IO`-result helpers—mirrors `lean.h:2893–2907`.
//!
//! `IO α` results from compiled Lean are encoded as constructors with tag 0
//! (`ok`) or tag 1 (`error`). The single object payload sits in constructor
//! slot 0; reading it requires the `ctor` accessors implemented against
//! `LeanCtorObjectRepr`.

#![allow(clippy::inline_always)]

use crate::repr::LeanCtorObjectRepr;
use crate::types::{b_lean_obj_arg, b_lean_obj_res, lean_obj_arg, lean_obj_res, lean_object};

unsafe extern "C" {
    /// Signal that runtime initialization is complete; subsequent allocations
    /// should not flag objects as persistent (`lean.h:2907`).
    pub fn lean_io_mark_end_initialization();
}

#[inline(always)]
unsafe fn ctor_get0(r: b_lean_obj_arg) -> *mut lean_object {
    // SAFETY: caller asserts `r` is an IO-result constructor whose first
    // object slot holds the payload.
    unsafe {
        let ctor = r.cast::<LeanCtorObjectRepr>();
        *(*ctor).objs.as_ptr()
    }
}

/// True if `r` is an `IO.ok` result (`lean.h:2893`).
///
/// # Safety
///
/// `r` must be a borrowed Lean `IO α` result.
#[inline(always)]
pub unsafe fn lean_io_result_is_ok(r: b_lean_obj_arg) -> bool {
    // SAFETY: precondition above; tag read is layout-pinned.
    unsafe { crate::object::lean_ptr_tag(r) == 0 }
}

/// True if `r` is an `IO.error` result (`lean.h:2894`).
///
/// # Safety
///
/// Same as [`lean_io_result_is_ok`].
#[inline(always)]
pub unsafe fn lean_io_result_is_error(r: b_lean_obj_arg) -> bool {
    // SAFETY: precondition above.
    unsafe { crate::object::lean_ptr_tag(r) == 1 }
}

/// Borrow the value out of an `IO.ok` result (`lean.h:2895`).
///
/// # Safety
///
/// `r` must be a borrowed Lean `IO α` result tagged `ok`. The returned
/// pointer aliases storage owned by `r`; the caller must not free `r` while
/// the borrow is live.
#[inline(always)]
pub unsafe fn lean_io_result_get_value(r: b_lean_obj_arg) -> b_lean_obj_res {
    // SAFETY: precondition above.
    unsafe { ctor_get0(r) }
}

/// Borrow the error out of an `IO.error` result (`lean.h:2896`).
///
/// # Safety
///
/// `r` must be a borrowed Lean `IO α` result tagged `error`.
#[inline(always)]
pub unsafe fn lean_io_result_get_error(r: b_lean_obj_arg) -> b_lean_obj_res {
    // SAFETY: precondition above.
    unsafe { ctor_get0(r) }
}

/// Take ownership of the value out of an `IO.ok` result (`lean.h:2898–2903`).
///
/// # Safety
///
/// `r` must be a Lean `IO α` result tagged `ok` and owned by the caller.
/// This function bumps the value's refcount and decrements `r`'s; after the
/// call `r` is no longer valid.
#[inline(always)]
pub unsafe fn lean_io_result_take_value(r: lean_obj_arg) -> lean_obj_res {
    // SAFETY: precondition above; calls into refcount helpers with the same
    // invariants.
    unsafe {
        let v = ctor_get0(r);
        crate::refcount::lean_inc(v);
        crate::refcount::lean_dec(r);
        v
    }
}
