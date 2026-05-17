//! `Nat` ↔ Rust unsigned-integer conversions.
//!
//! Lean's `Nat` uses a scalar-tagged fast path for values `≤ LEAN_MAX_SMALL_NAT`
//! and a heap MPZ bignum for anything larger. The writers in
//! [`lean_rs_sys::scalar`] handle the dispatch; the readers in this module
//! refuse heap-MPZ values with a `HostStage::Conversion` failure because
//! the public API does not link MPZ readers that could faithfully decode
//! a value wider than `u64` / `usize`.
//!
//! The trait impls in [`crate::abi::scalar`] for `u64` / `usize` produce
//! *polymorphic-boxed* values (ctor-wrapped `UInt64` / `USize`). The
//! helpers here produce values of Lean type `Nat`, which is a different
//! object shape (scalar tag or heap MPZ). Use [`from_u64`] / [`from_usize`]
//! when the Lean signature is `Nat`; use the `IntoLean` trait when the
//! Lean signature is `UInt64` / `USize` in a polymorphic position.

#![allow(unsafe_code)]

use lean_rs_sys::object::{lean_is_scalar, lean_obj_tag, lean_unbox};
use lean_rs_sys::scalar::{lean_uint64_to_nat, lean_usize_to_nat};

use crate::abi::traits::conversion_error;
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Construct a Lean `Nat` from a Rust `u64`.
///
/// Scalar-tagged for values up to `LEAN_MAX_SMALL_NAT`; falls back to a
/// heap MPZ via `lean_big_uint64_to_nat` otherwise.
#[must_use]
pub(crate) fn from_u64(runtime: &LeanRuntime, n: u64) -> Obj<'_> {
    // SAFETY: `lean_uint64_to_nat` returns an owned `lean_obj_res` (refcount
    // = 1) — scalar-tagged or heap-allocated as appropriate.
    unsafe { Obj::from_owned_raw(runtime, lean_uint64_to_nat(n)) }
}

/// Construct a Lean `Nat` from a Rust `usize`.
#[must_use]
pub(crate) fn from_usize(runtime: &LeanRuntime, n: usize) -> Obj<'_> {
    // SAFETY: `lean_usize_to_nat` returns an owned `lean_obj_res`.
    unsafe { Obj::from_owned_raw(runtime, lean_usize_to_nat(n)) }
}

/// Decode a Lean `Nat` into a Rust `u64`.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the `Nat` is a
/// heap MPZ (which always exceeds `LEAN_MAX_SMALL_NAT` and therefore may
/// exceed `u64::MAX` on 64-bit platforms — the safe API does not attempt
/// the bignum read).
#[allow(
    clippy::needless_pass_by_value,
    reason = "Obj is consumed by Drop on return; that releases the refcount"
)]
pub(crate) fn try_to_u64(obj: Obj<'_>) -> LeanResult<u64> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        // SAFETY: scalar branch verified; payload fits `usize`.
        let raw = unsafe { lean_unbox(ptr) };
        Ok(raw as u64)
    } else {
        // SAFETY: non-scalar branch — heap MPZ. Read the tag for the diagnostic.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(bignum_nat(found_tag))
    }
}

/// Decode a Lean `Nat` into a Rust `usize`.
///
/// # Errors
///
/// See [`try_to_u64`]; on 64-bit platforms `Nat` and `usize` share the
/// scalar encoding, so a heap-MPZ value triggers the same failure.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Obj is consumed by Drop on return; that releases the refcount"
)]
pub(crate) fn try_to_usize(obj: Obj<'_>) -> LeanResult<usize> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: pure pointer-bit math.
    if unsafe { lean_is_scalar(ptr) } {
        // SAFETY: scalar branch verified.
        Ok(unsafe { lean_unbox(ptr) })
    } else {
        // SAFETY: non-scalar branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(bignum_nat(found_tag))
    }
}

fn bignum_nat(found_tag: u32) -> LeanError {
    conversion_error(format!(
        "expected Lean Nat (scalar-fitting), found object with tag {found_tag}"
    ))
}
