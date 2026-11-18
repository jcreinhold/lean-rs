//! Valid owned-object laws. These tests require the linked Lean runtime.

#![allow(
    unsafe_code,
    clippy::expect_used,
    reason = "valid native roundtrips establish the exact decoder input"
)]

use lean_rs::abi::structure::alloc_ctor_with_objects;
use lean_rs::abi::traits::{IntoLean, LeanBoxedAbi, TryFromLean};
use lean_rs::{LeanDiagnosticCode, LeanRuntime};

#[test]
fn nested_native_containers_preserve_values_and_owned_references() {
    type Nested = Vec<Option<Result<(String, u64), bool>>>;
    let runtime = LeanRuntime::init().expect("runtime initialization");
    let expected: Nested = vec![None, Some(Ok(("native".to_owned(), u64::MAX))), Some(Err(true))];
    let boxed = expected.clone().into_boxed(runtime);
    let retained = boxed.clone();
    // SAFETY: both owned objects were encoded as this exact nested native
    // type above; cloning acquired an independent reference for each decoder.
    let first = unsafe { Nested::from_boxed(boxed) }.expect("first native decode");
    // SAFETY: retained is the independent reference to the same initialized
    // native value. Decoding first released only its own reference.
    let second = unsafe { Nested::from_boxed(retained) }.expect("retained native decode");
    assert_eq!(first, expected);
    assert_eq!(second, expected);
}

#[test]
fn scalar_decoders_reject_a_valid_constructor_without_scalar_storage() {
    let runtime = LeanRuntime::init().expect("runtime initialization");
    let empty = alloc_ctor_with_objects(runtime, 0, []);
    for error in [
        u64::try_from_lean(empty.clone()).expect_err("empty ctor has no UInt64 storage"),
        usize::try_from_lean(empty.clone()).expect_err("empty ctor has no USize storage"),
        i64::try_from_lean(empty.clone()).expect_err("empty ctor has no Int64 storage"),
        isize::try_from_lean(empty.clone()).expect_err("empty ctor has no ISize storage"),
        f64::try_from_lean(empty).expect_err("empty ctor has no Float storage"),
    ] {
        assert_eq!(error.code(), LeanDiagnosticCode::AbiConversion);
    }
}

#[test]
fn boxed_float_decoding_preserves_initialized_bit_patterns() {
    let runtime = LeanRuntime::init().expect("runtime initialization");
    for bits in [
        0,
        1 << 63,
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        0x7ff8_0000_0000_0042,
    ] {
        let value = f64::from_bits(bits);
        let decoded = f64::try_from_lean(value.into_lean(runtime)).expect("boxed Float");
        assert_eq!(decoded.to_bits(), bits);
    }
}
