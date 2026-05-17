//! Object and scalar arrays — externs and inline accessors from
//! `lean.h:815–1028`.

#![allow(clippy::inline_always)]
// `lean_alloc_sarray` takes `elem_size: u32` to mirror the C `unsigned`
// signature but stores the value in `m_other` (u8). The truncation is
// gated by a documented caller precondition (`elem_size <= u8::MAX`).
#![allow(clippy::cast_possible_truncation)]

use core::mem::size_of;

use crate::consts::LEAN_SCALAR_ARRAY;
use crate::object::lean_alloc_object;
use crate::repr::{LeanArrayObjectRepr, LeanObjectRepr, LeanSArrayObjectRepr};
use crate::types::{b_lean_obj_arg, lean_obj_arg, lean_obj_res, lean_object};

unsafe extern "C" {
    pub fn lean_array_mk(l: lean_obj_arg) -> *mut lean_object;
    pub fn lean_array_to_list(a: lean_obj_arg) -> *mut lean_object;
    pub fn lean_array_get_panic(def_val: lean_obj_arg) -> lean_obj_res;
    pub fn lean_array_set_panic(a: lean_obj_arg, v: lean_obj_arg) -> lean_obj_res;
    pub fn lean_array_push(a: lean_obj_arg, v: lean_obj_arg) -> *mut lean_object;
    pub fn lean_mk_array(n: lean_obj_arg, v: lean_obj_arg) -> *mut lean_object;
}

/// Allocate a freshly initialised scalar-array (`lean.h:1004–1010`).
///
/// Returns a `LeanScalarArray`-tagged object with `m_size = size`,
/// `m_capacity = capacity`, and `elem_size` bytes per element. The
/// payload bytes are uninitialised; the caller must write to
/// [`lean_sarray_cptr`] before the array escapes.
///
/// # Safety
///
/// * `elem_size` must fit a `u8` (Lean stores it in `m_other`) and must be
///   one of `{1, 2, 4, 8}` for the existing scalar-array consumers
///   (`ByteArray` uses `1`).
/// * `size <= capacity`.
/// * `size_of::<LeanSArrayObjectRepr>() + elem_size * capacity` must not
///   overflow `usize`; the helper checks this with `strict_*` arithmetic
///   and panics on overflow (mirroring `lean_alloc_sarray_would_overflow`).
#[inline(always)]
pub unsafe fn lean_alloc_sarray(elem_size: u32, size: usize, capacity: usize) -> lean_obj_res {
    let elem_size_usize = elem_size as usize;
    let total = size_of::<LeanSArrayObjectRepr>().strict_add(elem_size_usize.strict_mul(capacity));
    // SAFETY: `lean_alloc_object` returns a non-null pointer to `total` bytes
    // of uninitialised Lean-managed memory; we install the scalar-array
    // header before returning so the object is immediately well-formed for
    // every existing predicate (`lean_is_sarray`, `lean_sarray_*`).
    unsafe {
        let o = lean_alloc_object(total);
        let header = o.cast::<LeanObjectRepr>();
        (*header).m_rc = 1;
        (*header).m_tag = LEAN_SCALAR_ARRAY;
        // `elem_size` is asserted by the caller to fit a `u8`; cast loss
        // is impossible inside the documented contract.
        (*header).m_other = elem_size as u8;
        let sarray = o.cast::<LeanSArrayObjectRepr>();
        (*sarray).size = size;
        (*sarray).capacity = capacity;
        o
    }
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
