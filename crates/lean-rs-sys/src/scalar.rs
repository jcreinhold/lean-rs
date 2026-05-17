//! Boxed-scalar conversions — Rust mirrors of `lean.h:1356–2065`.
//!
//! [`crate::object::lean_box`] / [`crate::object::lean_unbox`] live in
//! [`crate::object`]; this module focuses on the `uintN_to_nat` /
//! `intN_to_int` widening helpers and their inverse `*_of_nat` family.
//! Big-number fallbacks dispatch into the externs declared in
//! [`crate::nat_int`].

// FFI mirrors: inlining is the design.
#![allow(clippy::inline_always)]
// The C ABI specifies narrowing conversions like `usize -> u32` and
// `i64 -> i32`; the lossy-cast lints fire by design here.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::nat_int::{
    LEAN_MAX_SMALL_INT, LEAN_MAX_SMALL_NAT, LEAN_MIN_SMALL_INT, lean_big_int_to_int,
    lean_big_int64_to_int, lean_big_size_t_to_int, lean_big_uint64_to_nat, lean_big_usize_to_nat,
    lean_uint8_of_big_nat,
};
use crate::object::{lean_box, lean_is_scalar, lean_unbox};
use crate::types::{b_lean_obj_arg, lean_obj_res};

/// Widen a `usize` into a Lean `Nat` (`lean.h:1356–1361`).
///
/// # Safety
///
/// Calls `lean_big_usize_to_nat` for values exceeding `LEAN_MAX_SMALL_NAT`;
/// no further preconditions.
#[inline(always)]
pub unsafe fn lean_usize_to_nat(n: usize) -> lean_obj_res {
    // SAFETY: scalar branch is pointer arithmetic; extern branch is
    // `LEAN_EXPORT`'d and allocates a fresh big-int object.
    unsafe {
        if n <= LEAN_MAX_SMALL_NAT {
            lean_box(n)
        } else {
            lean_big_usize_to_nat(n)
        }
    }
}

/// Widen an `unsigned` into a Lean `Nat` (`lean.h:1362–1364`).
///
/// # Safety
///
/// Same as [`lean_usize_to_nat`].
#[inline(always)]
pub unsafe fn lean_unsigned_to_nat(n: u32) -> lean_obj_res {
    // SAFETY: forwards to lean_usize_to_nat.
    unsafe { lean_usize_to_nat(n as usize) }
}

/// Widen a `u64` into a Lean `Nat` (`lean.h:1365–1370`).
///
/// # Safety
///
/// Calls `lean_big_uint64_to_nat` when `n > LEAN_MAX_SMALL_NAT`; no other
/// preconditions.
#[inline(always)]
pub unsafe fn lean_uint64_to_nat(n: u64) -> lean_obj_res {
    // SAFETY: same as lean_usize_to_nat; extern is `LEAN_EXPORT`'d.
    unsafe {
        if (n as usize as u64) == n && (n as usize) <= LEAN_MAX_SMALL_NAT {
            lean_box(n as usize)
        } else {
            lean_big_uint64_to_nat(n)
        }
    }
}

/// Narrow a Lean `Nat` to `u8` (`lean.h:1872`).
///
/// # Safety
///
/// `a` must be a valid borrowed Lean `Nat`. The big-int branch reads the
/// bignum payload through `lean_uint8_of_big_nat`.
#[inline(always)]
pub unsafe fn lean_uint8_of_nat(a: b_lean_obj_arg) -> u8 {
    // SAFETY: scalar branch is pointer arithmetic; extern handles bignum.
    unsafe {
        if lean_is_scalar(a) {
            lean_unbox(a) as u8
        } else {
            lean_uint8_of_big_nat(a)
        }
    }
}

/// Widen a `u8` to a Lean `Nat` (`lean.h:1875`).
///
/// # Safety
///
/// Always returns a scalar-boxed value; safe inputs only.
#[inline(always)]
pub unsafe fn lean_uint8_to_nat(a: u8) -> lean_obj_res {
    // SAFETY: forwards to lean_usize_to_nat.
    unsafe { lean_usize_to_nat(a as usize) }
}

/// Widen a `u16` to a Lean `Nat` (mirrors the `lean_uint16_to_nat` inline).
///
/// # Safety
///
/// Always returns a scalar-boxed value; safe inputs only.
#[inline(always)]
pub unsafe fn lean_uint16_to_nat(a: u16) -> lean_obj_res {
    // SAFETY: forwards to lean_usize_to_nat.
    unsafe { lean_usize_to_nat(a as usize) }
}

/// Widen a `u32` to a Lean `Nat` (mirrors the `lean_uint32_to_nat` inline).
///
/// # Safety
///
/// Always returns a scalar-boxed value; safe inputs only.
#[inline(always)]
pub unsafe fn lean_uint32_to_nat(a: u32) -> lean_obj_res {
    // SAFETY: forwards to lean_usize_to_nat.
    unsafe { lean_usize_to_nat(a as usize) }
}

/// Widen a signed `int` to a Lean `Int` (`lean.h:1565–1572`).
///
/// # Safety
///
/// Falls back to `lean_big_int_to_int` on 32-bit targets when `n` is out of
/// range; on 64-bit targets the boxed encoding always fits.
#[inline(always)]
pub unsafe fn lean_int_to_int(n: i32) -> lean_obj_res {
    // On 64-bit hosts every `i32` fits in a scalar Lean `Int`. On 32-bit
    // hosts we additionally have to check against `LEAN_MIN_SMALL_INT ..=
    // LEAN_MAX_SMALL_INT`, since the scalar pointer only spares 31 bits.
    let fits = core::mem::size_of::<*mut ()>() == 8
        || (LEAN_MIN_SMALL_INT..=LEAN_MAX_SMALL_INT).contains(&i64::from(n));
    // SAFETY: extern branch is `LEAN_EXPORT`'d.
    unsafe {
        if fits {
            lean_box((n as u32) as usize)
        } else {
            lean_big_int_to_int(n)
        }
    }
}

/// Widen an `i64` to a Lean `Int` (`lean.h:1574–1579`).
///
/// # Safety
///
/// Falls back to `lean_big_int64_to_int` for out-of-range values.
#[inline(always)]
pub unsafe fn lean_int64_to_int(n: i64) -> lean_obj_res {
    // SAFETY: extern branch is `LEAN_EXPORT`'d.
    unsafe {
        if (LEAN_MIN_SMALL_INT..=LEAN_MAX_SMALL_INT).contains(&n) {
            lean_box((n as i32 as u32) as usize)
        } else {
            lean_big_int64_to_int(n)
        }
    }
}

/// Promote a Lean `Nat` to `Int` (`lean.h:1597–1607`).
///
/// # Safety
///
/// Consumes `a` per the standard calling convention; falls back to
/// `lean_big_size_t_to_int` if the boxed value exceeds `LEAN_MAX_SMALL_INT`.
#[inline(always)]
pub unsafe fn lean_nat_to_int(a: lean_obj_res) -> lean_obj_res {
    // SAFETY: scalar branch is pointer math; extern is `LEAN_EXPORT`'d.
    unsafe {
        if !lean_is_scalar(a) {
            return a;
        }
        let v = lean_unbox(a);
        if i64::try_from(v).is_ok_and(|n| n <= LEAN_MAX_SMALL_INT) {
            a
        } else {
            lean_big_size_t_to_int(v)
        }
    }
}

/// Recover a signed integer from a scalar-tagged Lean `Int` (`lean.h:1581–1587`).
///
/// # Safety
///
/// `a` must be scalar-tagged; otherwise the result is meaningless.
#[inline(always)]
pub unsafe fn lean_scalar_to_int64(a: b_lean_obj_arg) -> i64 {
    // SAFETY: precondition above; pointer arithmetic only.
    unsafe {
        if core::mem::size_of::<*mut ()>() == 8 {
            i64::from((lean_unbox(a) as u32) as i32)
        } else {
            ((a as usize) as isize >> 1) as i64
        }
    }
}

/// Recover a signed integer from a scalar-tagged Lean `Int` (`lean.h:1589–1595`).
///
/// # Safety
///
/// Same as [`lean_scalar_to_int64`].
#[inline(always)]
pub unsafe fn lean_scalar_to_int(a: b_lean_obj_arg) -> i32 {
    // SAFETY: precondition above; pointer arithmetic only.
    unsafe { lean_scalar_to_int64(a) as i32 }
}
