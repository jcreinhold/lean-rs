//! `ByteArray` ↔ Rust `&[u8]` / `Vec<u8>` conversions via free helpers.
//!
//! Lean's `ByteArray` is a scalar-array (`LeanScalarArray`-tagged
//! `lean_sarray_object`) with `elem_size = 1`. The writers use the
//! `lean_alloc_sarray` mirror added in prompt 08 plus a single
//! `copy_nonoverlapping`; readers borrow the byte view directly without
//! a Rust-side allocation.
//!
//! Unlike `String`, this module does **not** implement
//! [`IntoLean`](crate::abi::traits::IntoLean) /
//! [`TryFromLean`](crate::abi::traits::TryFromLean) on `Vec<u8>`. The
//! same Rust type also names `Array UInt8` (a *boxed* element array,
//! distinct Lean tag and ABI). Overloading the trait on `Vec<u8>` would
//! pick one of two equally legitimate Lean shapes by accident; instead
//! we mirror the [`nat::from_u64`](crate::abi::nat::from_u64) convention
//! and expose `ByteArray` exclusively through the free helpers
//! [`from_bytes`], [`to_vec`], and [`borrow_bytes`]. Callers reach for
//! `Vec<T>::into_lean` when they want `Array T` and for [`from_bytes`]
//! when they want `ByteArray`; the type system disambiguates by API
//! rather than by overload resolution.

#![allow(unsafe_code)]

use core::ptr;
use core::slice;

use lean_rs_sys::array::{lean_alloc_sarray, lean_sarray_cptr, lean_sarray_elem_size, lean_sarray_size};
use lean_rs_sys::object::{lean_is_sarray, lean_is_scalar, lean_obj_tag};

use crate::abi::traits::conversion_error;
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::{Obj, ObjRef};

/// Build a Lean `ByteArray` from a borrowed Rust `&[u8]`.
///
/// One Lean-side allocation plus a single `memcpy`-equivalent copy. Works
/// for any input including empty slices and bytes containing NUL.
#[must_use]
pub(crate) fn from_bytes<'lean>(runtime: &'lean LeanRuntime, bytes: &[u8]) -> Obj<'lean> {
    // SAFETY: `lean_alloc_sarray(1, len, len)` returns an owned
    // scalar-array with `elem_size = 1`, size and capacity both `len`,
    // payload bytes uninitialised. We immediately fill the payload before
    // the object escapes.
    unsafe {
        let raw = lean_alloc_sarray(1, bytes.len(), bytes.len());
        if !bytes.is_empty() {
            ptr::copy_nonoverlapping(bytes.as_ptr(), lean_sarray_cptr(raw), bytes.len());
        }
        Obj::from_owned_raw(runtime, raw)
    }
}

/// Decode an owned Lean `ByteArray` into a fresh `Vec<u8>`.
///
/// Consumes the [`Obj`] — even on failure, the refcount is released by
/// `Obj`'s `Drop`. Allocates one Rust buffer of the payload size; for a
/// zero-copy view tied to the source's lifetime, use [`borrow_bytes`]
/// against an [`ObjRef`] instead.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if `obj` is not a
/// Lean `ByteArray` (a scalar array with `elem_size = 1`).
#[allow(
    clippy::needless_pass_by_value,
    reason = "Obj is consumed by Drop on return; that releases the refcount"
)]
pub(crate) fn to_vec(obj: Obj<'_>) -> LeanResult<Vec<u8>> {
    require_byte_array(&obj)?;
    // SAFETY: kind verified above; read the byte payload into a fresh
    // `Vec<u8>` of the recorded `size`.
    let owned = unsafe {
        let ptr = obj.as_raw_borrowed();
        let len = lean_sarray_size(ptr);
        let data = lean_sarray_cptr(ptr);
        let slice = slice::from_raw_parts(data, len);
        slice.to_vec()
    };
    Ok(owned)
}

/// Borrow a `&[u8]` view of a Lean `ByteArray` without copying.
///
/// The returned slice is tied to `obj`'s lifetime; it must not outlive
/// the borrowed `ObjRef`. This is the zero-allocation read path.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if `obj` is not a
/// Lean `ByteArray` (a scalar array with `elem_size = 1`).
pub(crate) fn borrow_bytes<'a>(obj: &'a ObjRef<'_, '_>) -> LeanResult<&'a [u8]> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(wrong_kind_scalar());
    }
    // SAFETY: non-scalar branch; tag and `m_other` read on the borrowed
    // source object.
    if !unsafe { lean_is_sarray(ptr) } || unsafe { lean_sarray_elem_size(ptr) } != 1 {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        return Err(wrong_kind_heap(found_tag));
    }
    // SAFETY: kind verified; slice borrows the payload for `'a`, bounded by
    // the `ObjRef`'s lifetime.
    let view = unsafe {
        let len = lean_sarray_size(ptr);
        let data = lean_sarray_cptr(ptr);
        slice::from_raw_parts(data, len)
    };
    Ok(view)
}

fn require_byte_array(obj: &Obj<'_>) -> LeanResult<()> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(wrong_kind_scalar());
    }
    // SAFETY: non-scalar branch.
    if unsafe { lean_is_sarray(ptr) } && unsafe { lean_sarray_elem_size(ptr) } == 1 {
        Ok(())
    } else {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(wrong_kind_heap(found_tag))
    }
}

fn wrong_kind_scalar() -> LeanError {
    conversion_error("expected Lean ByteArray, found scalar-tagged object")
}

fn wrong_kind_heap(found_tag: u32) -> LeanError {
    conversion_error(format!("expected Lean ByteArray, found object with tag {found_tag}"))
}
