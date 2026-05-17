//! String objects — externs and inline accessors from `lean.h:1157–1234`.

#![allow(clippy::inline_always)]

use core::ffi::c_char;

use crate::repr::LeanStringObjectRepr;
use crate::types::{b_lean_obj_arg, lean_obj_arg, lean_obj_res, lean_object};

unsafe extern "C" {
    pub fn lean_utf8_strlen(s: *const c_char) -> usize;
    pub fn lean_utf8_n_strlen(s: *const c_char, n: usize) -> usize;

    pub fn lean_mk_string(s: *const c_char) -> lean_obj_res;
    pub fn lean_mk_string_unchecked(s: *const c_char, sz: usize, len: usize) -> lean_obj_res;
    pub fn lean_mk_string_from_bytes(s: *const c_char, sz: usize) -> lean_obj_res;
    pub fn lean_mk_string_from_bytes_unchecked(s: *const c_char, sz: usize) -> lean_obj_res;
    pub fn lean_mk_ascii_string_unchecked(s: *const c_char) -> lean_obj_res;

    pub fn lean_string_push(s: lean_obj_arg, c: u32) -> lean_obj_res;
    pub fn lean_string_append(s1: lean_obj_arg, s2: b_lean_obj_arg) -> lean_obj_res;
    pub fn lean_string_mk(cs: lean_obj_arg) -> lean_obj_res;
    pub fn lean_string_data(s: lean_obj_arg) -> lean_obj_res;

    pub fn lean_string_utf8_get(s: b_lean_obj_arg, i: b_lean_obj_arg) -> u32;
    pub fn lean_string_utf8_next(s: b_lean_obj_arg, i: b_lean_obj_arg) -> lean_obj_res;
    pub fn lean_string_utf8_prev(s: b_lean_obj_arg, i: b_lean_obj_arg) -> lean_obj_res;
    pub fn lean_string_utf8_set(s: lean_obj_arg, i: b_lean_obj_arg, c: u32) -> lean_obj_res;
    pub fn lean_string_utf8_extract(
        s: b_lean_obj_arg,
        b: b_lean_obj_arg,
        e: b_lean_obj_arg,
    ) -> lean_obj_res;

    pub fn lean_string_eq_cold(s1: b_lean_obj_arg, s2: b_lean_obj_arg) -> bool;
    pub fn lean_string_lt(s1: b_lean_obj_arg, s2: b_lean_obj_arg) -> bool;
    pub fn lean_string_hash(s: b_lean_obj_arg) -> u64;
}

#[inline(always)]
unsafe fn as_string<'a>(o: *mut lean_object) -> &'a LeanStringObjectRepr {
    // SAFETY: caller asserts `o` is a Lean string heap object.
    unsafe { &*o.cast::<LeanStringObjectRepr>() }
}

/// `m_size` field — byte length including the trailing `\0` (`lean.h:1182`).
///
/// # Safety
///
/// `o` must be a borrowed Lean string object.
#[inline(always)]
pub unsafe fn lean_string_size(o: b_lean_obj_arg) -> usize {
    // SAFETY: precondition above; layout pinned by build digest.
    unsafe { as_string(o).size }
}

/// `m_length` field — UTF-8 character count (`lean.h:1183`).
///
/// # Safety
///
/// Same as [`lean_string_size`].
#[inline(always)]
pub unsafe fn lean_string_len(o: b_lean_obj_arg) -> usize {
    // SAFETY: precondition above.
    unsafe { as_string(o).length }
}

/// `m_capacity` field (`lean.h:1169`).
///
/// # Safety
///
/// Same as [`lean_string_size`].
#[inline(always)]
pub unsafe fn lean_string_capacity(o: *mut lean_object) -> usize {
    // SAFETY: precondition above.
    unsafe { as_string(o).capacity }
}

/// Pointer to the string's NUL-terminated UTF-8 bytes (`lean.h:1178–1181`).
///
/// # Safety
///
/// `o` must be a borrowed Lean string object. The returned pointer is valid
/// for as long as `o` retains the refcount that the caller borrowed.
#[inline(always)]
pub unsafe fn lean_string_cstr(o: b_lean_obj_arg) -> *const c_char {
    // SAFETY: precondition above; flexible-array member at fixed offset.
    unsafe { as_string(o).data.as_ptr().cast::<c_char>() }
}

/// Total allocation footprint of `o` (`lean.h:1170`).
///
/// # Safety
///
/// Same as [`lean_string_size`].
///
/// # Panics
///
/// Panics if `size_of::<LeanStringObjectRepr>() + capacity` overflows
/// `usize`. The string came from a valid Lean allocation so the sum cannot
/// realistically overflow; the panic surfaces a corrupted header rather
/// than a recoverable condition.
#[inline(always)]
pub unsafe fn lean_string_byte_size(o: *mut lean_object) -> usize {
    // SAFETY: precondition above. `capacity` came from a valid Lean string
    // so the addition cannot overflow; `strict_add` surfaces a corrupted
    // header as a panic instead of producing a wrong byte size.
    unsafe { core::mem::size_of::<LeanStringObjectRepr>().strict_add(lean_string_capacity(o)) }
}
