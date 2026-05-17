//! Object and scalar arrays — externs and inline accessors from
//! `lean.h:815–1028`.

#![allow(clippy::inline_always)]

use crate::repr::{LeanArrayObjectRepr, LeanSArrayObjectRepr};
use crate::types::{b_lean_obj_arg, lean_obj_arg, lean_obj_res, lean_object};

unsafe extern "C" {
    pub fn lean_array_mk(l: lean_obj_arg) -> *mut lean_object;
    pub fn lean_array_to_list(a: lean_obj_arg) -> *mut lean_object;
    pub fn lean_array_get_panic(def_val: lean_obj_arg) -> lean_obj_res;
    pub fn lean_array_set_panic(a: lean_obj_arg, v: lean_obj_arg) -> lean_obj_res;
    pub fn lean_array_push(a: lean_obj_arg, v: lean_obj_arg) -> *mut lean_object;
    pub fn lean_mk_array(n: lean_obj_arg, v: lean_obj_arg) -> *mut lean_object;
}

#[inline(always)]
unsafe fn as_array<'a>(o: *mut lean_object) -> &'a LeanArrayObjectRepr {
    // SAFETY: caller asserts `o` is a Lean array object.
    unsafe { &*o.cast::<LeanArrayObjectRepr>() }
}

#[inline(always)]
unsafe fn as_sarray<'a>(o: *mut lean_object) -> &'a LeanSArrayObjectRepr {
    // SAFETY: caller asserts `o` is a Lean scalar-array object.
    unsafe { &*o.cast::<LeanSArrayObjectRepr>() }
}

/// `m_size` of an object array (`lean.h:823`).
///
/// # Safety
///
/// `o` must be a borrowed Lean array object.
#[inline(always)]
pub unsafe fn lean_array_size(o: b_lean_obj_arg) -> usize {
    // SAFETY: precondition above.
    unsafe { as_array(o).size }
}

/// `m_capacity` of an object array (`lean.h:824`).
///
/// # Safety
///
/// Same as [`lean_array_size`].
#[inline(always)]
pub unsafe fn lean_array_capacity(o: b_lean_obj_arg) -> usize {
    // SAFETY: precondition above.
    unsafe { as_array(o).capacity }
}

/// Pointer to the array's element storage (`lean.h:831`).
///
/// # Safety
///
/// Same as [`lean_array_size`]. The returned pointer is valid for
/// `lean_array_capacity(o)` elements.
#[inline(always)]
pub unsafe fn lean_array_cptr(o: *mut lean_object) -> *mut *mut lean_object {
    // SAFETY: precondition above; flexible-array member offset is fixed.
    // `&raw mut` keeps us in pointer-land so there is no constness laundering.
    unsafe { (&raw mut (*o.cast::<LeanArrayObjectRepr>()).data).cast::<*mut lean_object>() }
}

/// Borrow element `i` of an object array (`lean.h:838–841`).
///
/// # Safety
///
/// `o` must be a borrowed Lean array object and `i < lean_array_size(o)`.
#[inline(always)]
pub unsafe fn lean_array_get_core(o: b_lean_obj_arg, i: usize) -> *mut lean_object {
    // SAFETY: precondition above.
    unsafe { *lean_array_cptr(o).add(i) }
}

/// Write element `i` of an object array (`lean.h:842–848`).
///
/// # Safety
///
/// `o` must be a unique (exclusive) Lean array object and `i <
/// lean_array_size(o)`. The caller transfers ownership of `v`.
#[inline(always)]
pub unsafe fn lean_array_set_core(o: *mut lean_object, i: usize, v: lean_obj_arg) {
    // SAFETY: precondition above.
    unsafe { *lean_array_cptr(o).add(i) = v }
}

/// Element size of a scalar array (`lean.h:1011–1014`).
///
/// # Safety
///
/// `o` must be a borrowed Lean scalar-array object.
#[inline(always)]
pub unsafe fn lean_sarray_elem_size(o: *mut lean_object) -> u8 {
    // SAFETY: stored in m_other; layout pinned by build digest.
    unsafe { crate::object::lean_ptr_other(o) }
}

/// `m_size` of a scalar array (`lean.h:1019`).
///
/// # Safety
///
/// `o` must be a borrowed Lean scalar-array object.
#[inline(always)]
pub unsafe fn lean_sarray_size(o: b_lean_obj_arg) -> usize {
    // SAFETY: precondition above.
    unsafe { as_sarray(o).size }
}

/// `m_capacity` of a scalar array (`lean.h:1015`).
///
/// # Safety
///
/// Same as [`lean_sarray_size`].
#[inline(always)]
pub unsafe fn lean_sarray_capacity(o: *mut lean_object) -> usize {
    // SAFETY: precondition above.
    unsafe { as_sarray(o).capacity }
}

/// Pointer to the scalar array's byte storage (`lean.h:1028`).
///
/// # Safety
///
/// Same as [`lean_sarray_size`]. The returned pointer is valid for
/// `lean_sarray_capacity(o) * lean_sarray_elem_size(o)` bytes.
#[inline(always)]
pub unsafe fn lean_sarray_cptr(o: *mut lean_object) -> *mut u8 {
    // SAFETY: precondition above; flexible-array member offset is fixed.
    unsafe { (&raw mut (*o.cast::<LeanSArrayObjectRepr>()).data).cast::<u8>() }
}
