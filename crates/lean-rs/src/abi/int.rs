//! `Int` ↔ Rust signed-integer conversions.
//!
//! Mirrors [`crate::abi::nat`] for signed values. The writers in
//! [`lean_rs_sys::scalar`] dispatch between the scalar-tagged fast path
//! (values fitting `[LEAN_MIN_SMALL_INT, LEAN_MAX_SMALL_INT]`) and the
//! heap-MPZ slow path. The readers in this module refuse heap-MPZ values
//! with a `HostStage::Conversion` failure because the safe API does not
//! link MPZ readers.

#![allow(unsafe_code)]

use lean_rs_sys::object::{lean_is_scalar, lean_obj_tag};
use lean_rs_sys::scalar::{lean_int64_to_int, lean_scalar_to_int64};

use crate::abi::traits::conversion_error;
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Construct a Lean `Int` from a Rust `i64`.
///
/// Scalar-tagged for values in `[LEAN_MIN_SMALL_INT, LEAN_MAX_SMALL_INT]`;
/// falls back to a heap MPZ via `lean_big_int64_to_int` otherwise.
#[must_use]
pub(crate) fn from_i64(runtime: &LeanRuntime, n: i64) -> Obj<'_> {
    // SAFETY: `lean_int64_to_int` returns an owned `lean_obj_res`.
    unsafe { Obj::from_owned_raw(runtime, lean_int64_to_int(n)) }
}

/// Construct a Lean `Int` from a Rust `isize`.
#[must_use]
pub(crate) fn from_isize(runtime: &LeanRuntime, n: isize) -> Obj<'_> {
    from_i64(runtime, n as i64)
}

/// Decode a Lean `Int` into a Rust `i64`.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the `Int` is a
/// heap MPZ (the safe API does not link an MPZ reader for general `i64`
/// decoding).
#[allow(
    clippy::needless_pass_by_value,
    reason = "Obj is consumed by Drop on return; that releases the refcount"
)]
pub(crate) fn try_to_i64(obj: Obj<'_>) -> LeanResult<i64> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        // SAFETY: scalar branch verified; `lean_scalar_to_int64` reads the
        // payload and sign-extends to `i64`.
        Ok(unsafe { lean_scalar_to_int64(ptr) })
    } else {
        // SAFETY: non-scalar branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(bignum_int(found_tag))
    }
}

/// Decode a Lean `Int` into a Rust `isize`.
///
/// # Errors
///
/// See [`try_to_i64`]; also returns `LeanError::Host { stage: Conversion }`
/// if the scalar-fitting `i64` does not fit `isize` (32-bit targets only).
pub(crate) fn try_to_isize(obj: Obj<'_>) -> LeanResult<isize> {
    let value = try_to_i64(obj)?;
    isize::try_from(value).map_err(|_| out_of_range_isize())
}

fn bignum_int(found_tag: u32) -> LeanError {
    conversion_error(format!(
        "expected Lean Int (scalar-fitting), found object with tag {found_tag}"
    ))
}

fn out_of_range_isize() -> LeanError {
    conversion_error("Lean Int value does not fit Rust isize")
}
