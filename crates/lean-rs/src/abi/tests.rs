//! Round-trip tests for the `pub(crate) abi` conversions.
//!
//! Each test follows the pattern:
//! 1. Bring the Lean runtime + `LeanRsFixture` library up via the
//!    throwaway loader in [`lean_rs_test_support::fixture::init`].
//! 2. Build a Rust value, marshal it through [`IntoLean`] (or a `from_*`
//!    helper) into a Lean object.
//! 3. Hand the Lean object to a fixture export declared as
//!    `extern "C"` below, taking ownership of the result.
//! 4. Decode the returned `Obj` via [`TryFromLean`] (or a `try_to_*`
//!    helper) and assert equality with the input.
//!
//! Fixture exports take and return *unboxed* values for fixed-width
//! scalars (`u8`, `u16`, `u32`, `u64`, `usize`, `char`, `f64`) and
//! boxed `lean_object*` for everything else (`Nat`, `Int`, `Bool`,
//! `Unit`, `String`, `ByteArray`).

// SAFETY DOC: every `unsafe { ... }` block carries a per-block `// SAFETY:`
// comment naming the invariant.
#![allow(unsafe_code)]
#![allow(clippy::expect_used, clippy::float_cmp, clippy::panic)]

use core::ffi::c_char;

use lean_rs_sys::nat_int::LEAN_MAX_SMALL_NAT;
use lean_rs_sys::object::{lean_box, lean_unbox};
use lean_rs_sys::types::lean_object;
use lean_rs_test_support::fixture;

use crate::LeanRuntime;

/// Bring the Lean runtime + `LeanRsFixture` library up. Idempotent.
fn init() -> &'static LeanRuntime {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    fixture::init_fixture();
    runtime
}

use crate::abi::except::Except;
use crate::abi::structure::{alloc_ctor_with_objects, ctor_tag, take_ctor_objects};
use crate::abi::traits::{IntoLean, TryFromLean};
use crate::abi::{bytearray, int, nat, string};
use crate::error::{HostStage, LeanError};
use crate::runtime::obj::Obj;

// -- fixture extern declarations -----------------------------------------
//
// These mirror the `@[export ...]` attributes in
// `fixtures/lean/LeanRsFixture/{Scalars,Strings}.lean`. Linkage to the
// fixture dylib is emitted by `crates/lean-rs-test-support/build.rs`.

unsafe extern "C" {
    fn lean_rs_fixture_u8_identity(x: u8) -> u8;
    fn lean_rs_fixture_u16_identity(x: u16) -> u16;
    fn lean_rs_fixture_u32_identity(x: u32) -> u32;
    fn lean_rs_fixture_u64_identity(x: u64) -> u64;
    fn lean_rs_fixture_usize_identity(x: usize) -> usize;
    fn lean_rs_fixture_u32_add(a: u32, b: u32) -> u32;
    fn lean_rs_fixture_u64_mul(a: u64, b: u64) -> u64;

    fn lean_rs_fixture_nat_identity(n: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_nat_succ(n: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_int_identity(n: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_int_neg(n: *mut lean_object) -> *mut lean_object;

    fn lean_rs_fixture_bool_not(b: u8) -> u8;
    fn lean_rs_fixture_unit_id(u: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_char_identity(c: u32) -> u32;
    fn lean_rs_fixture_float_identity(x: f64) -> f64;
    fn lean_rs_fixture_float_add(a: f64, b: f64) -> f64;

    fn lean_rs_fixture_string_identity(s: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_string_length(s: *mut lean_object) -> *mut lean_object;

    fn lean_rs_fixture_bytearray_identity(b: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_bytearray_size(b: *mut lean_object) -> *mut lean_object;

    fn lean_rs_fixture_array_string_identity(xs: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_array_string_push(xs: *mut lean_object, x: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_option_nat_identity(x: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_option_nat_some(n: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_except_string_nat_ok(n: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_except_string_nat_err(s: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_pair_make(n: *mut lean_object, s: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_bundle_make(name: *mut lean_object, items: *mut lean_object) -> *mut lean_object;
    fn lean_rs_fixture_bundle_identity(b: *mut lean_object) -> *mut lean_object;
}

// -- scalar (unboxed) round trips ----------------------------------------

#[test]
fn unboxed_scalar_identity_round_trips() {
    let _runtime = init();
    // SAFETY: every fixture call below is a `extern "C"` Lean export with
    // no Lean-side preconditions beyond a valid argument value.
    unsafe {
        assert_eq!(lean_rs_fixture_u8_identity(0), 0);
        assert_eq!(lean_rs_fixture_u8_identity(u8::MAX), u8::MAX);
        assert_eq!(lean_rs_fixture_u16_identity(u16::MAX), u16::MAX);
        assert_eq!(lean_rs_fixture_u32_identity(u32::MAX), u32::MAX);
        assert_eq!(lean_rs_fixture_u64_identity(u64::MAX), u64::MAX);
        assert_eq!(lean_rs_fixture_usize_identity(usize::MAX), usize::MAX);
    }
}

#[test]
fn unboxed_scalar_multi_argument_calls() {
    let _runtime = init();
    // SAFETY: pure unboxed-arg/return Lean exports.
    unsafe {
        assert_eq!(lean_rs_fixture_u32_add(7, 35), 42);
        assert_eq!(lean_rs_fixture_u64_mul(u64::from(u32::MAX), 2), u64::from(u32::MAX) * 2);
    }
}

#[test]
fn float_identity_round_trip_includes_nan() {
    let _runtime = init();
    // SAFETY: pure unboxed-`f64` Lean export.
    unsafe {
        assert_eq!(lean_rs_fixture_float_identity(0.0), 0.0);
        assert_eq!(lean_rs_fixture_float_identity(-1.5), -1.5);
        assert_eq!(
            lean_rs_fixture_float_identity(core::f64::consts::PI),
            core::f64::consts::PI
        );
        assert!(lean_rs_fixture_float_identity(f64::NAN).is_nan());
        assert_eq!(lean_rs_fixture_float_add(0.1, 0.2), 0.1 + 0.2);
    }
}

#[test]
fn char_identity_round_trip_preserves_non_ascii() {
    let _runtime = init();
    // SAFETY: Lean's `Char` is an unboxed `uint32_t`; pass and receive via
    // the same C representation Rust uses for `char as u32`.
    unsafe {
        for c in ['a', '🦀', '\0', char::MAX] {
            let echoed = lean_rs_fixture_char_identity(u32::from(c));
            assert_eq!(echoed, u32::from(c));
        }
    }
}

// -- bool / unit (boxed scalar) ------------------------------------------

#[test]
fn bool_round_trip_via_fixture() {
    let _runtime = init();
    // SAFETY: Lean's `Bool` ABI is an unboxed `uint8_t` carrying `0`/`1`;
    // pass the boolean directly through the C boundary.
    unsafe {
        assert_eq!(lean_rs_fixture_bool_not(0), 1);
        assert_eq!(lean_rs_fixture_bool_not(1), 0);
    }
}

#[test]
fn unit_round_trip_via_fixture() {
    let runtime = init();
    // Lean's `Unit` is the zero-tag constructor, scalar-boxed as
    // `lean_box(0)` in argument positions. We construct it through the
    // `IntoLean` impl and hand the raw pointer to the fixture.
    let unit_obj: Obj<'_> = ().into_lean(runtime);
    // SAFETY: `unit_id` borrows then re-returns the input; ownership of
    // the resulting pointer transfers back to the new `Obj`.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_unit_id(unit_obj.into_raw())) };
    <()>::try_from_lean(echoed).expect("Unit decodes");
}

// -- Nat / Int (scalar fast path + bignum diagnostic) --------------------

#[test]
fn nat_identity_round_trips_small_values() {
    let runtime = init();
    for &n in &[0_u64, 1, 42, 1_000, u64::from(u32::MAX)] {
        let input: Obj<'_> = nat::from_u64(runtime, n);
        // SAFETY: fixture call transfers ownership in and out.
        let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_nat_identity(input.into_raw())) };
        assert_eq!(nat::try_to_u64(echoed).expect("scalar Nat decodes"), n);
    }
}

#[test]
fn nat_succ_of_zero_round_trips_through_u64() {
    let runtime = init();
    let input: Obj<'_> = nat::from_u64(runtime, 0);
    // SAFETY: standard ownership-transfer pattern.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_nat_succ(input.into_raw())) };
    assert_eq!(nat::try_to_u64(echoed).expect("scalar Nat decodes"), 1);
}

#[test]
fn nat_succ_of_max_small_returns_bignum_that_does_not_fit_u64() {
    let runtime = init();
    // `LEAN_MAX_SMALL_NAT == usize::MAX >> 1`; `succ` of that overflows to
    // a bignum. Our `try_to_u64` deliberately refuses the bignum read.
    let input: Obj<'_> = nat::from_usize(runtime, LEAN_MAX_SMALL_NAT);
    // SAFETY: standard ownership-transfer pattern.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_nat_succ(input.into_raw())) };
    match nat::try_to_u64(echoed) {
        Err(LeanError::Host(host)) => {
            assert_eq!(host.stage(), HostStage::Conversion);
            assert!(
                host.message().contains("Nat (scalar-fitting)"),
                "unexpected message: {:?}",
                host.message()
            );
        }
        other => panic!("expected Host(Conversion) for bignum, got {other:?}"),
    }
}

#[test]
fn int_identity_round_trips_signed_values() {
    let runtime = init();
    for &n in &[0_i64, 1, -1, 42, -42, i64::from(i32::MAX), i64::from(i32::MIN)] {
        let input: Obj<'_> = int::from_i64(runtime, n);
        // SAFETY: standard ownership-transfer pattern.
        let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_int_identity(input.into_raw())) };
        assert_eq!(int::try_to_i64(echoed).expect("scalar Int decodes"), n);
    }
}

#[test]
fn isize_and_usize_helpers_round_trip_through_int_and_nat_fixtures() {
    let runtime = init();
    // usize: positive values; reuse the `Nat` fixture.
    let n: usize = 12345;
    let input: Obj<'_> = nat::from_usize(runtime, n);
    // SAFETY: standard ownership-transfer pattern.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_nat_identity(input.into_raw())) };
    assert_eq!(nat::try_to_usize(echoed).expect("scalar Nat decodes"), n);

    // isize: signed values via the `Int` fixture.
    for &v in &[0_isize, 1, -1, 42, -42] {
        let input: Obj<'_> = int::from_isize(runtime, v);
        // SAFETY: standard ownership-transfer pattern.
        let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_int_identity(input.into_raw())) };
        assert_eq!(int::try_to_isize(echoed).expect("scalar Int decodes"), v);
    }
}

#[test]
fn int_neg_round_trips_through_fixture() {
    let runtime = init();
    for &n in &[0_i64, 1, -1, 42, -42, i64::from(i32::MAX)] {
        let input: Obj<'_> = int::from_i64(runtime, n);
        // SAFETY: standard ownership-transfer pattern.
        let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_int_neg(input.into_raw())) };
        let decoded = int::try_to_i64(echoed).expect("scalar Int decodes");
        // `i32::MAX.wrapping_neg()` is still scalar-fitting; everything else
        // negates trivially.
        assert_eq!(decoded, n.wrapping_neg());
    }
}

// -- String --------------------------------------------------------------

fn string_identity_helper(s: &str) {
    let runtime = init();
    let input: Obj<'_> = string::from_str(runtime, s);
    // SAFETY: standard ownership-transfer pattern.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_string_identity(input.into_raw())) };
    // Borrowed view: returns a `&str` view of the Lean payload without
    // any Rust-side allocation beyond what `Obj` already holds.
    let view = echoed.borrow();
    let borrowed = string::borrow_str(&view).expect("Lean String bytes are valid UTF-8");
    assert_eq!(borrowed, s);
    // Owned decode: allocates one `String` on the Rust side.
    let owned = String::try_from_lean(echoed).expect("Lean String round-trips");
    assert_eq!(owned, s);
}

#[test]
fn string_round_trips_empty() {
    string_identity_helper("");
}

#[test]
fn string_round_trips_ascii() {
    string_identity_helper("hello, world");
}

#[test]
fn string_round_trips_non_ascii_utf8() {
    string_identity_helper("héllo 🦀 — Lean 4 says hi");
}

#[test]
fn string_round_trips_large_payload() {
    let large = "a".repeat(10 * 1024);
    string_identity_helper(&large);
}

#[test]
fn string_round_trips_embedded_nul_bytes() {
    // `lean_mk_string_from_bytes_unchecked` takes an explicit length, so
    // the trailing-NUL convention does not truncate the payload.
    string_identity_helper("a\0b\0c");
}

#[test]
fn string_length_returns_utf8_codepoint_count_as_nat() {
    let runtime = init();
    let s = "héllo 🦀";
    let input: Obj<'_> = string::from_str(runtime, s);
    // SAFETY: standard ownership-transfer pattern.
    let len_obj = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_string_length(input.into_raw())) };
    let len = nat::try_to_u64(len_obj).expect("string length fits scalar");
    assert_eq!(len, s.chars().count() as u64);
}

// -- ByteArray -----------------------------------------------------------

fn bytearray_identity_helper(bytes: &[u8]) {
    let runtime = init();
    let input: Obj<'_> = bytearray::from_bytes(runtime, bytes);
    // SAFETY: standard ownership-transfer pattern.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_bytearray_identity(input.into_raw())) };
    let view = echoed.borrow();
    let borrowed = bytearray::borrow_bytes(&view).expect("ByteArray view");
    assert_eq!(borrowed, bytes);
    let owned = bytearray::to_vec(echoed).expect("ByteArray round-trips");
    assert_eq!(owned.as_slice(), bytes);
}

#[test]
fn bytearray_round_trips_empty() {
    bytearray_identity_helper(&[]);
}

#[test]
fn bytearray_round_trips_single_byte() {
    bytearray_identity_helper(&[0x42]);
}

#[test]
fn bytearray_round_trips_all_byte_values() {
    let all: Vec<u8> = (0_u8..=255).collect();
    bytearray_identity_helper(&all);
}

#[test]
fn bytearray_round_trips_embedded_nul_bytes() {
    bytearray_identity_helper(b"a\0b\0c\0\0\0d");
}

#[test]
fn bytearray_round_trips_large_payload() {
    let large = vec![0xAB_u8; 10 * 1024];
    bytearray_identity_helper(&large);
}

#[test]
fn bytearray_size_returns_byte_count_as_nat() {
    let runtime = init();
    let bytes: &[u8] = b"hello\0world";
    let input: Obj<'_> = bytearray::from_bytes(runtime, bytes);
    // SAFETY: standard ownership-transfer pattern.
    let size_obj = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_bytearray_size(input.into_raw())) };
    let size = nat::try_to_u64(size_obj).expect("byte-count fits scalar");
    assert_eq!(size, bytes.len() as u64);
}

// -- IntoLean / TryFromLean trait round trips (no fixture call) ----------
//
// These exercise the trait surface without invoking a fixture, confirming
// that polymorphic-boxed `u64`/`usize`/`f64`/`bool`/`Unit` decode back to
// the same value.

#[test]
fn trait_round_trip_u64_via_polymorphic_boxing() {
    let runtime = init();
    for &v in &[0_u64, 1, u64::from(u32::MAX), u64::MAX] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(u64::try_from_lean(obj).expect("u64 decodes"), v);
    }
}

#[test]
fn trait_round_trip_usize_via_polymorphic_boxing() {
    let runtime = init();
    for &v in &[0_usize, 1, usize::MAX] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(usize::try_from_lean(obj).expect("usize decodes"), v);
    }
}

#[test]
fn trait_round_trip_f64_via_polymorphic_boxing() {
    let runtime = init();
    for &v in &[0.0_f64, -1.5, core::f64::consts::PI, f64::INFINITY] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(f64::try_from_lean(obj).expect("f64 decodes"), v);
    }
}

#[test]
fn trait_round_trip_bool_uses_scalar_encoding() {
    let runtime = init();
    let obj_true: Obj<'_> = true.into_lean(runtime);
    let obj_false: Obj<'_> = false.into_lean(runtime);
    assert!(bool::try_from_lean(obj_true).expect("true decodes"));
    assert!(!bool::try_from_lean(obj_false).expect("false decodes"));
}

#[test]
fn trait_round_trip_small_int_macro_stamped_impls() {
    let runtime = init();
    // u8: scalar-boxed via `lean_box(n as usize)`.
    for v in [0_u8, 1, 0x7F, u8::MAX] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(u8::try_from_lean(obj).expect("u8 decodes"), v);
    }
    // u16 / u32 / i8 / i16 / i32 — same path, signed variants share the
    // unsigned encoding.
    for v in [0_i32, 1, -1, i32::MAX, i32::MIN] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(i32::try_from_lean(obj).expect("i32 decodes"), v);
    }
}

#[test]
fn trait_round_trip_char_rejects_non_unicode_scalar() {
    let runtime = init();
    // Round-trip a valid scalar value first.
    let obj: Obj<'_> = '🦀'.into_lean(runtime);
    assert_eq!(char::try_from_lean(obj).expect("char decodes"), '🦀');

    // A surrogate code point round-tripped through a `u32`-shaped Lean
    // value cannot decode back to `char`.
    let surrogate_u32: u32 = 0xD800;
    let obj: Obj<'_> = surrogate_u32.into_lean(runtime);
    match char::try_from_lean(obj) {
        Err(LeanError::Host(host)) => {
            assert_eq!(host.stage(), HostStage::Conversion);
            assert!(
                host.message().contains("Unicode scalar value"),
                "unexpected message: {:?}",
                host.message()
            );
            assert!(
                host.message().contains(&format!("{surrogate_u32:#x}")),
                "unexpected message: {:?}",
                host.message()
            );
        }
        other => panic!("expected Host(Conversion) for surrogate, got {other:?}"),
    }
}

// -- TryFromLean kind-mismatch diagnostics -------------------------------

#[test]
fn try_from_lean_returns_wrong_kind_for_mismatched_object() {
    let runtime = init();
    // Build a String, then try to decode it as a ByteArray. Expect a
    // conversion-stage host failure naming `ByteArray`.
    let s: Obj<'_> = string::from_str(runtime, "not a byte array");
    match bytearray::to_vec(s) {
        Err(LeanError::Host(host)) => {
            assert_eq!(host.stage(), HostStage::Conversion);
            assert!(
                host.message().contains("ByteArray"),
                "unexpected message: {:?}",
                host.message()
            );
        }
        other => panic!("expected Host(Conversion) for kind mismatch, got {other:?}"),
    }
}

// -- Borrowed views suppress the Rust-side allocation --------------------
//
// Note rather than counter-based assertion (per the prompt's "tests or
// notes" allowance): the `string::borrow_str` and `bytearray::borrow_bytes`
// helpers return slice views into the Lean payload. The only Rust-side
// allocation along the borrowed-read path is the `Obj` itself (a single
// `NonNull<lean_object>`); reading is a `slice::from_raw_parts` over the
// payload. The owned-read variants (`String::try_from_lean` and
// `bytearray::to_vec`) additionally allocate one Rust buffer of the
// payload's size.

#[test]
fn borrowed_view_aliases_lean_payload() {
    let runtime = init();
    let bytes: &[u8] = b"alias me";
    let obj: Obj<'_> = bytearray::from_bytes(runtime, bytes);
    let view = obj.borrow();
    let borrowed = bytearray::borrow_bytes(&view).expect("borrow succeeds");

    // The slice's data pointer is the Lean payload pointer — no copy.
    // SAFETY: pure pointer comparison; both pointers are valid because
    // `obj` lives until end-of-scope and `view` borrows it.
    unsafe {
        let payload_ptr = lean_rs_sys::array::lean_sarray_cptr(obj.as_raw_borrowed());
        assert_eq!(borrowed.as_ptr(), payload_ptr.cast_const());
    }
    assert_eq!(borrowed, bytes);
}

// -- helper: silence unused-imports lint on platforms where the c_char /
// lean_box / lean_unbox imports are not directly named.
#[allow(dead_code)]
fn _silence_unused() {
    let _: *const c_char = core::ptr::null();
    // SAFETY: pointer arithmetic only.
    unsafe {
        let _ = lean_box(0);
        let _ = lean_unbox(lean_box(0));
    }
}

// -- Array String round trips (prompt 09) --------------------------------

/// Ship `xs` through the Lean `arrayStringIdentity` fixture and assert
/// the round-tripped Rust `Vec<String>` matches.
fn array_string_round_trip(xs: Vec<String>) {
    let runtime = init();
    let expected = xs.clone();
    let input: Obj<'_> = xs.into_lean(runtime);
    // SAFETY: standard ownership-transfer pattern through the fixture.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_array_string_identity(input.into_raw())) };
    let decoded = Vec::<String>::try_from_lean(echoed).expect("Array String round-trips");
    assert_eq!(decoded, expected);
}

#[test]
fn array_string_empty_round_trips() {
    array_string_round_trip(Vec::new());
}

#[test]
fn array_string_single_element_round_trips() {
    array_string_round_trip(vec!["solo".to_owned()]);
}

#[test]
fn array_string_multi_element_round_trips() {
    array_string_round_trip(vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()]);
}

#[test]
fn array_string_with_empty_and_non_ascii_elements() {
    array_string_round_trip(vec![String::new(), "🦀".to_owned(), "naïve".to_owned()]);
}

#[test]
fn array_string_push_round_trips_added_element() {
    let runtime = init();
    let xs: Vec<String> = vec!["one".to_owned(), "two".to_owned()];
    let pushed = "three".to_owned();
    let xs_obj = xs.into_lean(runtime);
    let push_obj = pushed.clone().into_lean(runtime);
    // SAFETY: fixture takes ownership of both arguments and returns an
    // owned array.
    let echoed = unsafe {
        Obj::from_owned_raw(
            runtime,
            lean_rs_fixture_array_string_push(xs_obj.into_raw(), push_obj.into_raw()),
        )
    };
    let decoded = Vec::<String>::try_from_lean(echoed).expect("Array String push round-trips");
    assert_eq!(decoded, vec!["one".to_owned(), "two".to_owned(), pushed]);
}

#[test]
fn array_returns_wrong_kind_for_non_array() {
    let runtime = init();
    // Build a String, try to decode it as an Array String.
    let s: Obj<'_> = string::from_str(runtime, "not an array");
    match Vec::<String>::try_from_lean(s) {
        Err(LeanError::Host(host)) => {
            assert_eq!(host.stage(), HostStage::Conversion);
            assert!(
                host.message().contains("Array"),
                "unexpected message: {:?}",
                host.message()
            );
        }
        other => panic!("expected Host(Conversion) for non-array, got {other:?}"),
    }
}

// -- Option round trips ---------------------------------------------------

#[test]
fn option_nat_identity_round_trips_none() {
    let runtime = init();
    let input: Option<u64> = None;
    // `Option<u64>` lands as `Option Nat` because the fixture is typed
    // `Option Nat`. We build the Lean `Option` ctor by hand: outer ctor
    // wraps a `Nat` field (none has no fields, some wraps a Nat).
    let input_obj = match input {
        None => alloc_ctor_with_objects::<0>(runtime, 0, []),
        Some(n) => alloc_ctor_with_objects(runtime, 1, [nat::from_u64(runtime, n)]),
    };
    // SAFETY: standard ownership transfer through the fixture.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_option_nat_identity(input_obj.into_raw())) };
    let tag = ctor_tag(&echoed).expect("Option ctor");
    assert_eq!(tag, 0, "expected None");
    let [] = take_ctor_objects::<0>(echoed, 0, "Option::none").expect("none decodes");
}

#[test]
fn option_nat_identity_round_trips_some() {
    let runtime = init();
    let n: u64 = 42;
    let input_obj = alloc_ctor_with_objects(runtime, 1, [nat::from_u64(runtime, n)]);
    // SAFETY: fixture takes ownership of the ctor.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_option_nat_identity(input_obj.into_raw())) };
    let tag = ctor_tag(&echoed).expect("Option ctor");
    assert_eq!(tag, 1, "expected Some");
    let [field] = take_ctor_objects::<1>(echoed, 1, "Option::some").expect("some decodes");
    assert_eq!(nat::try_to_u64(field).expect("Nat decodes"), n);
}

#[test]
fn option_nat_some_constructed_lean_side() {
    let runtime = init();
    // Lean's `optionNatSome n` builds `some n` directly: tag 1, num_objs 1.
    // The matching `optionNatNone` constant compiles to a global static
    // rather than a callable export (Lean shares the persistent value), so
    // we skip the None side here — `option_nat_identity_round_trips_none`
    // already covers a Rust-built None going through `optionNatIdentity`.
    let n: u64 = 7;
    let arg = nat::from_u64(runtime, n);
    // SAFETY: fixture consumes the Nat argument and returns `Option Nat`.
    let some_obj = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_option_nat_some(arg.into_raw())) };
    let tag = ctor_tag(&some_obj).expect("Option ctor");
    assert_eq!(tag, 1);
    let [inner] = take_ctor_objects::<1>(some_obj, 1, "Option::some").expect("some decodes");
    assert_eq!(nat::try_to_u64(inner).expect("Nat decodes"), n);
}

#[test]
fn option_trait_round_trip_u64_some_and_none() {
    let runtime = init();
    // Direct IntoLean / TryFromLean round trip (no fixture call). Here
    // `u64` is the polymorphic-boxed UInt64 ctor; this exercises the
    // generic Option impl across a non-trivial inner type.
    for input in [Some(0_u64), Some(1), Some(u64::MAX), None] {
        let obj = input.into_lean(runtime);
        let out = Option::<u64>::try_from_lean(obj).expect("Option round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn option_nested_round_trips() {
    let runtime = init();
    // `Option<Option<u64>>` — exercises that the structure primitives
    // compose under nesting without ambiguity.
    let cases: Vec<Option<Option<u64>>> = vec![None, Some(None), Some(Some(0)), Some(Some(u64::MAX))];
    for input in cases {
        let obj = input.into_lean(runtime);
        let out = Option::<Option<u64>>::try_from_lean(obj).expect("nested Option round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn option_returns_wrong_tag_for_bogus_ctor() {
    let runtime = init();
    // A ctor with tag 5 is neither Option.none (0) nor Option.some (1).
    let bogus = alloc_ctor_with_objects::<0>(runtime, 5, []);
    match Option::<u64>::try_from_lean(bogus) {
        Err(LeanError::Host(host)) => {
            assert_eq!(host.stage(), HostStage::Conversion);
            assert!(host.message().contains("Option"), "unexpected: {:?}", host.message());
        }
        other => panic!("expected Host(Conversion) for bogus tag, got {other:?}"),
    }
}

// -- Except / Result round trips ------------------------------------------

#[test]
fn except_string_nat_ok_round_trips_via_fixture() {
    let runtime = init();
    let n: u64 = 99;
    let arg = nat::from_u64(runtime, n);
    // SAFETY: fixture takes ownership of `arg` and returns an
    // `Except String Nat` ctor.
    let obj = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_except_string_nat_ok(arg.into_raw())) };
    // The inner `Nat` is scalar-tagged, not polymorphic-boxed UInt64, so we
    // decode the ctor via the structure primitives and reach for
    // `nat::try_to_u64` for the field. The generic `Result<u64, String>`
    // impl would route the field through `u64::try_from_lean`, which
    // expects the polymorphic-boxed encoding instead — the same Nat-vs-
    // UInt64 distinction documented under `ERROR-BOUNDARY` and shown in
    // [`crate::abi::nat`]'s caveat list.
    let tag = ctor_tag(&obj).expect("Except ctor");
    assert_eq!(tag, 1, "expected ok");
    let [field] = take_ctor_objects::<1>(obj, 1, "Except::ok").expect("ok decodes");
    assert_eq!(nat::try_to_u64(field).expect("Nat decodes"), n);
}

#[test]
fn except_string_nat_err_round_trips_via_fixture() {
    let runtime = init();
    let s = "boom".to_owned();
    let arg = string::from_str(runtime, &s);
    // SAFETY: fixture takes ownership of `arg` and returns an
    // `Except String Nat` ctor with tag 0.
    let obj = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_except_string_nat_err(arg.into_raw())) };
    let tag = ctor_tag(&obj).expect("Except ctor");
    assert_eq!(tag, 0, "expected error");
    let [field] = take_ctor_objects::<1>(obj, 0, "Except::error").expect("error decodes");
    assert_eq!(String::try_from_lean(field).expect("String decodes"), s);
}

#[test]
fn except_trait_round_trip_via_lean_constructed_then_rust_decoded() {
    let runtime = init();
    // Build Except::Ok / Except::Error directly through IntoLean and
    // decode via Except's TryFromLean — exercises the named value type
    // (not just the Result delegate).
    let cases: Vec<Except<String, u64>> = vec![Except::Ok(0), Except::Ok(123), Except::Error("oops".to_owned())];
    for input in cases {
        let obj = input.clone().into_lean(runtime);
        let out = Except::<String, u64>::try_from_lean(obj).expect("Except round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn result_trait_round_trip_through_pure_abi() {
    let runtime = init();
    let cases: Vec<Result<u64, String>> = vec![Ok(0), Ok(u64::MAX), Err("nope".to_owned())];
    for input in cases {
        let obj = input.clone().into_lean(runtime);
        let out = Result::<u64, String>::try_from_lean(obj).expect("Result round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn except_returns_wrong_tag_for_bogus_ctor() {
    let runtime = init();
    let bogus = alloc_ctor_with_objects::<0>(runtime, 7, []);
    match Except::<String, u64>::try_from_lean(bogus) {
        Err(LeanError::Host(host)) => {
            assert_eq!(host.stage(), HostStage::Conversion);
            assert!(host.message().contains("Except"), "unexpected: {:?}", host.message());
        }
        other => panic!("expected Host(Conversion) for bogus tag, got {other:?}"),
    }
}

// -- Structure pattern: Pair (Nat, String) -------------------------------

#[test]
fn pair_make_round_trips_via_fixture() {
    let runtime = init();
    let n: u64 = 1234;
    let s = "pair-field".to_owned();
    let nat_obj = nat::from_u64(runtime, n);
    let str_obj = string::from_str(runtime, &s);
    // SAFETY: fixture consumes both arguments, returns a `Pair` ctor.
    let pair = unsafe {
        Obj::from_owned_raw(
            runtime,
            lean_rs_fixture_pair_make(nat_obj.into_raw(), str_obj.into_raw()),
        )
    };
    // Pair is a single-constructor structure: tag 0, two object fields.
    let [first, second] = take_ctor_objects::<2>(pair, 0, "Pair").expect("Pair decodes");
    assert_eq!(nat::try_to_u64(first).expect("first decodes"), n);
    assert_eq!(String::try_from_lean(second).expect("second decodes"), s);
}

// -- Structure pattern: Bundle (String, Array String) --------------------

#[test]
fn bundle_make_round_trips_via_fixture() {
    let runtime = init();
    let name = "release".to_owned();
    let items: Vec<String> = vec!["x86_64".to_owned(), "aarch64".to_owned()];
    let name_obj = string::from_str(runtime, &name);
    let items_obj = items.clone().into_lean(runtime);
    // SAFETY: fixture consumes both arguments, returns a `Bundle` ctor.
    let bundle = unsafe {
        Obj::from_owned_raw(
            runtime,
            lean_rs_fixture_bundle_make(name_obj.into_raw(), items_obj.into_raw()),
        )
    };
    let [name_field, items_field] = take_ctor_objects::<2>(bundle, 0, "Bundle").expect("Bundle decodes");
    assert_eq!(String::try_from_lean(name_field).expect("name decodes"), name);
    assert_eq!(Vec::<String>::try_from_lean(items_field).expect("items decode"), items);
}

#[test]
fn bundle_identity_round_trips_through_lean() {
    let runtime = init();
    let name = "alpha".to_owned();
    let items: Vec<String> = vec!["one".to_owned(), "two".to_owned(), "three".to_owned()];
    // Build a Bundle ctor in Rust (the structure pattern in action), ship
    // it through `bundleIdentity`, and decode the round trip.
    let bundle = alloc_ctor_with_objects(
        runtime,
        0,
        [string::from_str(runtime, &name), items.clone().into_lean(runtime)],
    );
    // SAFETY: fixture takes ownership of the ctor and returns the same
    // bundle shape.
    let echoed = unsafe { Obj::from_owned_raw(runtime, lean_rs_fixture_bundle_identity(bundle.into_raw())) };
    let [name_field, items_field] = take_ctor_objects::<2>(echoed, 0, "Bundle").expect("Bundle decodes");
    assert_eq!(String::try_from_lean(name_field).expect("name decodes"), name);
    assert_eq!(Vec::<String>::try_from_lean(items_field).expect("items decode"), items);
}

// -- Composed nested containers (no fixture) -----------------------------

#[test]
fn vec_of_option_round_trips_through_pure_abi() {
    let runtime = init();
    let input: Vec<Option<u64>> = vec![None, Some(0), Some(42), None, Some(u64::MAX)];
    let obj = input.clone().into_lean(runtime);
    let out = Vec::<Option<u64>>::try_from_lean(obj).expect("Vec<Option> round-trips");
    assert_eq!(out, input);
}

#[test]
fn option_of_vec_round_trips_through_pure_abi() {
    let runtime = init();
    let cases: Vec<Option<Vec<String>>> = vec![
        None,
        Some(Vec::new()),
        Some(vec!["only-element".to_owned()]),
        Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]),
    ];
    for input in cases {
        let obj = input.clone().into_lean(runtime);
        let out = Option::<Vec<String>>::try_from_lean(obj).expect("Option<Vec> round-trips");
        assert_eq!(out, input);
    }
}

// Silence-unused-imports anchor for the int helper, which the prompt-08
// tests pull in directly. Without this the bytearray-test migration drops
// the only reference and clippy flags the import.
#[allow(dead_code)]
fn _silence_int_helper() {
    let _ = int::from_i64;
}
