//! Fuzzing entry points (feature `fuzzing`, not semver-stable).
//!
//! The in-tree `crates/lean-rs/fuzz/` cargo-fuzz crate drives the
//! `pub(crate) abi` decoders with `Arbitrary`-generated Lean-shaped
//! inputs constructed via `lean-rs-sys` public helpers. Those decoders
//! are `pub(crate)`, so this module is the *only* place that exposes
//! them for an external test harness—and only when the `fuzzing`
//! feature is enabled.
//!
//! Per `docs/architecture/01-safety-model.md`'s "fuzz arbitrary raw
//! pointers" non-goal, every entry point takes a `*mut lean_object`
//! that the caller must have produced from a real `lean-rs-sys`
//! allocator. The wrappers do not validate provenance; they only
//! observe that decoders return either `Ok(_)` or
//! `Err(LeanError::Host(stage = Conversion))`.
//!
//! The `Drop` of every wrapped `Obj<'lean>` runs `lean_dec`, so the
//! caller transfers exactly one refcount per call.

#![allow(unsafe_code)]

use lean_rs_sys::lean_object;

use crate::abi::traits::TryFromLean;
use crate::abi::{bytearray, nat, structure};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Wrap a raw owned Lean pointer (one refcount transferred from the
/// caller) and decode it as a Rust `String`.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the input is
/// not a Lean `String` (wrong tag, scalar, malformed UTF-8).
///
/// # Safety
///
/// `raw` must be a non-null pointer produced by a `lean-rs-sys`
/// allocator, owning exactly one Lean reference count.
pub unsafe fn decode_string(runtime: &LeanRuntime, raw: *mut lean_object) -> LeanResult<String> {
    // SAFETY: caller's contract.
    let obj = unsafe { Obj::from_owned_raw(runtime, raw) };
    String::try_from_lean(obj)
}

/// Decode a raw owned pointer as a `Vec<u8>` interpreted as a
/// `ByteArray`.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the input is
/// not a packed-byte scalar array (wrong tag, wrong elem size).
///
/// # Safety
///
/// Same as [`decode_string`].
pub unsafe fn decode_bytearray(runtime: &LeanRuntime, raw: *mut lean_object) -> LeanResult<Vec<u8>> {
    // SAFETY: caller's contract.
    let obj = unsafe { Obj::from_owned_raw(runtime, raw) };
    bytearray::to_vec(obj)
}

/// Decode a raw owned pointer as a `Vec<u64>` interpreted as
/// `Array UInt64`.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the input is
/// not an array of polymorphic-boxed `u64` values.
///
/// # Safety
///
/// Same as [`decode_string`]; the array's element type
/// must be polymorphic-boxed `u64` (Lake's `Array UInt64` encoding).
pub unsafe fn decode_array_u64(runtime: &LeanRuntime, raw: *mut lean_object) -> LeanResult<Vec<u64>> {
    // SAFETY: caller's contract.
    let obj = unsafe { Obj::from_owned_raw(runtime, raw) };
    Vec::<u64>::try_from_lean(obj)
}

/// Decode a raw owned pointer as an `Option<u64>` interpreted as
/// `Option UInt64`.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the input
/// does not match Lean's mixed-arity `Option` encoding (`None` =
/// `lean_box(0)`, `Some x` = ctor tag 1 with one field).
///
/// # Safety
///
/// Same as [`decode_string`].
pub unsafe fn decode_option_u64(runtime: &LeanRuntime, raw: *mut lean_object) -> LeanResult<Option<u64>> {
    // SAFETY: caller's contract.
    let obj = unsafe { Obj::from_owned_raw(runtime, raw) };
    Option::<u64>::try_from_lean(obj)
}

/// Decode a raw owned pointer as a `Result<u64, String>` interpreted
/// as `Except String UInt64`.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the input
/// is not an `Except` ctor (tag 0 = `error`, tag 1 = `ok`), or if
/// either branch's payload fails its own decode.
///
/// # Safety
///
/// Same as [`decode_string`].
pub unsafe fn decode_except(runtime: &LeanRuntime, raw: *mut lean_object) -> LeanResult<Result<u64, String>> {
    // SAFETY: caller's contract.
    let obj = unsafe { Obj::from_owned_raw(runtime, raw) };
    Result::<u64, String>::try_from_lean(obj)
}

/// Decode a raw owned pointer as a `u64` interpreted as a Lean `Nat`
/// (scalar fast path; bignum branch surfaces a typed `Conversion`
/// error).
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the input
/// is not a scalar-tagged `Nat` that fits a `u64` (bignum / wrong tag).
///
/// # Safety
///
/// Same as [`decode_string`].
pub unsafe fn decode_nat_u64(runtime: &LeanRuntime, raw: *mut lean_object) -> LeanResult<u64> {
    // SAFETY: caller's contract.
    let obj = unsafe { Obj::from_owned_raw(runtime, raw) };
    nat::try_to_u64(obj)
}

/// Read the tag of a constructor, surfacing a typed `Conversion` error
/// when the input is not a constructor at all.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if the input is
/// a scalar or non-constructor heap object.
///
/// # Safety
///
/// Same as [`decode_string`].
pub unsafe fn decode_ctor_tag(runtime: &LeanRuntime, raw: *mut lean_object) -> LeanResult<u8> {
    // SAFETY: caller's contract.
    let obj = unsafe { Obj::from_owned_raw(runtime, raw) };
    structure::ctor_tag(&obj)
}

// Re-export the public error type so fuzz harnesses can pattern-match
// on `LeanError::Host` without crossing through `lean_rs::error`.
#[doc(hidden)]
pub use crate::error::LeanError;
