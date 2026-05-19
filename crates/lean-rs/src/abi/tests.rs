//! Round-trip tests for the `pub(crate) abi` conversions, dispatched
//! through the typed [`crate::module::LeanExported`] handles landed by
//! prompt 12.
//!
//! Each test follows the pattern:
//! 1. Bring up the Lean runtime + open the fixture library via
//!    [`fixture_library`] and initialize the root module inline.
//! 2. Build a Rust value, marshal it through [`IntoLean`] (or a `from_*`
//!    helper) into a Lean object — done implicitly by `LeanExported::call`
//!    for typed-handle round trips.
//! 3. Look up the fixture export by name through `module.exported::<Args, R>(...)`
//!    and call it.
//! 4. Compare the decoded Rust value against the input.
//!
//! Compared to the prompt-08/09 tests, the per-test surface shrinks: no
//! `unsafe { Obj::from_owned_raw(runtime, fixture(arg.into_raw())) }`
//! incantation per call site, no `extern "C"` declaration block, no
//! reliance on the throwaway `lean_rs_test_support::fixture` loader.

#![allow(
    clippy::expect_used,
    clippy::float_cmp,
    clippy::panic,
    clippy::wildcard_enum_match_arm
)]

use std::path::PathBuf;

use crate::abi::except::Except;
use crate::abi::structure::{alloc_ctor_with_objects, ctor_tag, take_ctor_objects};
use crate::abi::traits::{IntoLean, TryFromLean};
use crate::abi::{bytearray, int, nat, string};
use crate::error::{HostStage, LeanError, LeanExceptionKind};
use crate::module::{LeanIo, LeanLibrary};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

// -- fixture setup -------------------------------------------------------
//
// `LeanLibrary` is `!Sync` (it borrows `&'lean LeanRuntime` and
// `LeanRuntime` is `!Sync`), so it cannot live in a `static OnceLock`.
// Each test opens its own [`LeanLibrary`] against the shared runtime
// singleton; symbol-table walking + dlopen are both backed by the OS page
// cache for the second-and-later open of the same file, so the overhead
// across the test suite is negligible.

fn fixture_dylib_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = workspace
        .join("fixtures")
        .join("lean")
        .join(".lake")
        .join("build")
        .join("lib");
    let new_style = lib_dir.join(format!("liblean__rs__fixture_LeanRsFixture.{dylib_extension}"));
    let old_style = lib_dir.join(format!("libLeanRsFixture.{dylib_extension}"));
    if old_style.is_file() && !new_style.is_file() {
        old_style
    } else {
        new_style
    }
}

/// `LeanRuntime` reference for tests that need to build Lean values
/// directly (e.g. via `string::from_str`).
fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

/// Open the fixture library against the shared runtime singleton. Each
/// test that needs the typed handle then calls `library.initialize_module(...)`
/// inline; `LeanModule<'lean, 'lib>` borrows from `library`, so the two
/// bindings live together at the start of every test.
fn fixture_library() -> LeanLibrary<'static> {
    let path = fixture_dylib_path();
    assert!(
        path.exists(),
        "fixture dylib not found at {} — run `cd fixtures/lean && lake build`",
        path.display(),
    );
    LeanLibrary::open(runtime(), &path).expect("fixture dylib opens cleanly")
}

// -- scalar (unboxed) round trips ----------------------------------------

#[test]
fn unboxed_scalar_identity_round_trips() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    assert_eq!(
        module
            .exported::<(u8,), u8>("lean_rs_fixture_u8_identity")
            .expect("lookup")
            .call(0)
            .expect("call"),
        0,
    );
    assert_eq!(
        module
            .exported::<(u8,), u8>("lean_rs_fixture_u8_identity")
            .expect("lookup")
            .call(u8::MAX)
            .expect("call"),
        u8::MAX,
    );
    assert_eq!(
        module
            .exported::<(u16,), u16>("lean_rs_fixture_u16_identity")
            .expect("lookup")
            .call(u16::MAX)
            .expect("call"),
        u16::MAX,
    );
    assert_eq!(
        module
            .exported::<(u32,), u32>("lean_rs_fixture_u32_identity")
            .expect("lookup")
            .call(u32::MAX)
            .expect("call"),
        u32::MAX,
    );
    assert_eq!(
        module
            .exported::<(u64,), u64>("lean_rs_fixture_u64_identity")
            .expect("lookup")
            .call(u64::MAX)
            .expect("call"),
        u64::MAX,
    );
    assert_eq!(
        module
            .exported::<(usize,), usize>("lean_rs_fixture_usize_identity")
            .expect("lookup")
            .call(usize::MAX)
            .expect("call"),
        usize::MAX,
    );
}

#[test]
fn unboxed_scalar_multi_argument_calls() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let add = module
        .exported::<(u32, u32), u32>("lean_rs_fixture_u32_add")
        .expect("lookup");
    assert_eq!(add.call(7, 35).expect("call"), 42);
    let mul = module
        .exported::<(u64, u64), u64>("lean_rs_fixture_u64_mul")
        .expect("lookup");
    assert_eq!(mul.call(u64::from(u32::MAX), 2).expect("call"), u64::from(u32::MAX) * 2);
}

#[test]
fn float_identity_round_trip_includes_nan() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(f64,), f64>("lean_rs_fixture_float_identity")
        .expect("lookup");
    assert_eq!(identity.call(0.0).expect("call"), 0.0);
    assert_eq!(identity.call(-1.5).expect("call"), -1.5);
    assert_eq!(
        identity.call(core::f64::consts::PI).expect("call"),
        core::f64::consts::PI,
    );
    assert!(identity.call(f64::NAN).expect("call").is_nan());
    let add = module
        .exported::<(f64, f64), f64>("lean_rs_fixture_float_add")
        .expect("lookup");
    assert_eq!(add.call(0.1, 0.2).expect("call"), 0.1 + 0.2);
}

#[test]
fn char_identity_round_trip_preserves_non_ascii() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(char,), char>("lean_rs_fixture_char_identity")
        .expect("lookup");
    for c in ['a', '🦀', '\0', char::MAX] {
        assert_eq!(identity.call(c).expect("call"), c);
    }
}

// -- bool / unit (boxed scalar) ------------------------------------------

#[test]
fn bool_round_trip_via_fixture() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let bool_not = module
        .exported::<(bool,), bool>("lean_rs_fixture_bool_not")
        .expect("lookup");
    assert!(bool_not.call(false).expect("call"));
    assert!(!bool_not.call(true).expect("call"));
}

#[test]
fn unit_round_trip_via_fixture() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let unit_id = module.exported::<((),), ()>("lean_rs_fixture_unit_id").expect("lookup");
    unit_id.call(()).expect("Unit round-trips");
}

// -- Nat / Int (scalar fast path + bignum diagnostic) --------------------
//
// The `Nat`/`Int` Lean encoding uses scalar-tagged pointers for values
// that fit in `LEAN_MAX_SMALL_NAT`; bignums are heap-allocated. Our
// `nat::*` / `int::*` helpers handle the scalar path; bignum decodes
// surface as `HostStage::Conversion`. Since these helpers do not match
// any `IntoLean`/`TryFromLean` blanket impl for `u64` (which would route
// through the polymorphic-`UInt64` boxing instead), the tests build the
// Lean argument by hand and decode the response with the helper.

#[test]
fn nat_identity_round_trips_small_values() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_nat_identity")
        .expect("lookup");
    for &n in &[0_u64, 1, 42, 1_000, u64::from(u32::MAX)] {
        let input = nat::from_u64(runtime, n);
        let echoed = identity.call(input).expect("call");
        assert_eq!(nat::try_to_u64(echoed).expect("scalar Nat decodes"), n);
    }
}

#[test]
fn nat_succ_of_zero_round_trips_through_u64() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let succ = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_nat_succ")
        .expect("lookup");
    let echoed = succ.call(nat::from_u64(runtime, 0)).expect("call");
    assert_eq!(nat::try_to_u64(echoed).expect("scalar Nat decodes"), 1);
}

#[test]
fn nat_succ_of_max_small_returns_bignum_that_does_not_fit_u64() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let succ = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_nat_succ")
        .expect("lookup");
    let input = nat::from_usize(runtime, lean_rs_sys::nat_int::LEAN_MAX_SMALL_NAT);
    let echoed = succ.call(input).expect("call");
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
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_int_identity")
        .expect("lookup");
    for &n in &[0_i64, 1, -1, 42, -42, i64::from(i32::MAX), i64::from(i32::MIN)] {
        let input = int::from_i64(runtime, n);
        let echoed = identity.call(input).expect("call");
        assert_eq!(int::try_to_i64(echoed).expect("scalar Int decodes"), n);
    }
}

#[test]
fn isize_and_usize_helpers_round_trip_through_int_and_nat_fixtures() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let nat_identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_nat_identity")
        .expect("lookup");
    let n: usize = 12345;
    let echoed = nat_identity.call(nat::from_usize(runtime, n)).expect("call");
    assert_eq!(nat::try_to_usize(echoed).expect("scalar Nat decodes"), n);

    let int_identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_int_identity")
        .expect("lookup");
    for &v in &[0_isize, 1, -1, 42, -42] {
        let echoed = int_identity.call(int::from_isize(runtime, v)).expect("call");
        assert_eq!(int::try_to_isize(echoed).expect("scalar Int decodes"), v);
    }
}

#[test]
fn int_neg_round_trips_through_fixture() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let neg = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_int_neg")
        .expect("lookup");
    for &n in &[0_i64, 1, -1, 42, -42, i64::from(i32::MAX)] {
        let echoed = neg.call(int::from_i64(runtime, n)).expect("call");
        let decoded = int::try_to_i64(echoed).expect("scalar Int decodes");
        assert_eq!(decoded, n.wrapping_neg());
    }
}

// -- String --------------------------------------------------------------

fn string_identity_helper(s: &str) {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(String,), String>("lean_rs_fixture_string_identity")
        .expect("lookup");
    assert_eq!(identity.call(s.to_owned()).expect("call"), s);
}

/// `LeanAbi for &str` round-trips through the same fixture as the
/// owned-`String` path. Covers the borrowed-encode entry point used by
/// `LeanSession::elaborate`, `kernel_check`, `elaborate_bulk`, and
/// `make_name`.
#[test]
fn borrowed_str_arg_round_trips_through_string_identity() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(&str,), String>("lean_rs_fixture_string_identity")
        .expect("lookup");
    for &s in &["", "hello, world", "héllo 🦀", "a\0b\0c"] {
        assert_eq!(identity.call(s).expect("call"), s);
    }
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
    string_identity_helper("a\0b\0c");
}

#[test]
fn string_length_returns_utf8_codepoint_count_as_nat() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let length = module
        .exported::<(String,), Obj<'_>>("lean_rs_fixture_string_length")
        .expect("lookup");
    let s = "héllo 🦀";
    let len_obj = length.call(s.to_owned()).expect("call");
    let len = nat::try_to_u64(len_obj).expect("string length fits scalar");
    assert_eq!(len, s.chars().count() as u64);
}

// -- ByteArray -----------------------------------------------------------

/// `ByteArray` fixtures take/return `Obj<'_>` here because `Vec<u8>` has
/// no `IntoLean` / `TryFromLean` impl (overload ambiguity with
/// `Array UInt8`). Round trips build the input through the `bytearray`
/// free helpers and decode the output the same way.
fn bytearray_identity_helper(bytes: &[u8]) {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_bytearray_identity")
        .expect("lookup");
    let echoed = identity.call(bytearray::from_bytes(runtime, bytes)).expect("call");
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
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let size_fn = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_bytearray_size")
        .expect("lookup");
    let bytes: &[u8] = b"hello\0world";
    let size_obj = size_fn.call(bytearray::from_bytes(runtime, bytes)).expect("call");
    let size = nat::try_to_u64(size_obj).expect("byte-count fits scalar");
    assert_eq!(size, bytes.len() as u64);
}

// -- IntoLean / TryFromLean trait round trips (no fixture call) ----------

#[test]
fn trait_round_trip_u64_via_polymorphic_boxing() {
    let runtime = runtime();
    for &v in &[0_u64, 1, u64::from(u32::MAX), u64::MAX] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(u64::try_from_lean(obj).expect("u64 decodes"), v);
    }
}

#[test]
fn trait_round_trip_usize_via_polymorphic_boxing() {
    let runtime = runtime();
    for &v in &[0_usize, 1, usize::MAX] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(usize::try_from_lean(obj).expect("usize decodes"), v);
    }
}

#[test]
fn trait_round_trip_f64_via_polymorphic_boxing() {
    let runtime = runtime();
    for &v in &[0.0_f64, -1.5, core::f64::consts::PI, f64::INFINITY] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(f64::try_from_lean(obj).expect("f64 decodes"), v);
    }
}

#[test]
fn trait_round_trip_bool_uses_scalar_encoding() {
    let runtime = runtime();
    let obj_true: Obj<'_> = true.into_lean(runtime);
    let obj_false: Obj<'_> = false.into_lean(runtime);
    assert!(bool::try_from_lean(obj_true).expect("true decodes"));
    assert!(!bool::try_from_lean(obj_false).expect("false decodes"));
}

#[test]
fn trait_round_trip_small_int_macro_stamped_impls() {
    let runtime = runtime();
    for v in [0_u8, 1, 0x7F, u8::MAX] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(u8::try_from_lean(obj).expect("u8 decodes"), v);
    }
    for v in [0_i32, 1, -1, i32::MAX, i32::MIN] {
        let obj: Obj<'_> = v.into_lean(runtime);
        assert_eq!(i32::try_from_lean(obj).expect("i32 decodes"), v);
    }
}

#[test]
fn trait_round_trip_char_rejects_non_unicode_scalar() {
    let runtime = runtime();
    let obj: Obj<'_> = '🦀'.into_lean(runtime);
    assert_eq!(char::try_from_lean(obj).expect("char decodes"), '🦀');

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
    let runtime = runtime();
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

#[test]
fn borrowed_view_aliases_lean_payload() {
    let runtime = runtime();
    let bytes: &[u8] = b"alias me";
    let obj: Obj<'_> = bytearray::from_bytes(runtime, bytes);
    let view = obj.borrow();
    let borrowed = bytearray::borrow_bytes(&view).expect("borrow succeeds");

    // SAFETY: pure pointer comparison; both pointers are valid because
    // `obj` lives until end-of-scope and `view` borrows it.
    #[allow(unsafe_code)]
    unsafe {
        let payload_ptr = lean_rs_sys::array::lean_sarray_cptr(obj.as_raw_borrowed());
        assert_eq!(borrowed.as_ptr(), payload_ptr.cast_const());
    }
    assert_eq!(borrowed, bytes);
}

// -- Array String round trips --------------------------------------------

fn array_string_round_trip(xs: Vec<String>) {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(Vec<String>,), Vec<String>>("lean_rs_fixture_array_string_identity")
        .expect("lookup");
    let expected = xs.clone();
    let decoded = identity.call(xs).expect("call");
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
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let push = module
        .exported::<(Vec<String>, String), Vec<String>>("lean_rs_fixture_array_string_push")
        .expect("lookup");
    let decoded = push
        .call(vec!["one".to_owned(), "two".to_owned()], "three".to_owned())
        .expect("call");
    assert_eq!(decoded, vec!["one".to_owned(), "two".to_owned(), "three".to_owned()]);
}

#[test]
fn array_returns_wrong_kind_for_non_array() {
    let runtime = runtime();
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
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_option_nat_identity")
        .expect("lookup");
    let input_obj = alloc_ctor_with_objects::<0>(runtime, 0, []);
    let echoed = identity.call(input_obj).expect("call");
    let tag = ctor_tag(&echoed).expect("Option ctor");
    assert_eq!(tag, 0, "expected None");
    let [] = take_ctor_objects::<0>(echoed, 0, "Option::none").expect("none decodes");
}

#[test]
fn option_nat_identity_round_trips_some() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_option_nat_identity")
        .expect("lookup");
    let n: u64 = 42;
    let input_obj = alloc_ctor_with_objects(runtime, 1, [nat::from_u64(runtime, n)]);
    let echoed = identity.call(input_obj).expect("call");
    let tag = ctor_tag(&echoed).expect("Option ctor");
    assert_eq!(tag, 1, "expected Some");
    let [field] = take_ctor_objects::<1>(echoed, 1, "Option::some").expect("some decodes");
    assert_eq!(nat::try_to_u64(field).expect("Nat decodes"), n);
}

#[test]
fn option_nat_some_constructed_lean_side() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let some_fn = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_option_nat_some")
        .expect("lookup");
    let n: u64 = 7;
    let some_obj = some_fn.call(nat::from_u64(runtime, n)).expect("call");
    let tag = ctor_tag(&some_obj).expect("Option ctor");
    assert_eq!(tag, 1);
    let [inner] = take_ctor_objects::<1>(some_obj, 1, "Option::some").expect("some decodes");
    assert_eq!(nat::try_to_u64(inner).expect("Nat decodes"), n);
}

#[test]
fn option_trait_round_trip_u64_some_and_none() {
    let runtime = runtime();
    for input in [Some(0_u64), Some(1), Some(u64::MAX), None] {
        let obj = input.into_lean(runtime);
        let out = Option::<u64>::try_from_lean(obj).expect("Option round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn option_nested_round_trips() {
    let runtime = runtime();
    let cases: Vec<Option<Option<u64>>> = vec![None, Some(None), Some(Some(0)), Some(Some(u64::MAX))];
    for input in cases {
        let obj = input.into_lean(runtime);
        let out = Option::<Option<u64>>::try_from_lean(obj).expect("nested Option round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn option_returns_wrong_tag_for_bogus_ctor() {
    let runtime = runtime();
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
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let ok_fn = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_except_string_nat_ok")
        .expect("lookup");
    let n: u64 = 99;
    let obj = ok_fn.call(nat::from_u64(runtime, n)).expect("call");
    let tag = ctor_tag(&obj).expect("Except ctor");
    assert_eq!(tag, 1, "expected ok");
    let [field] = take_ctor_objects::<1>(obj, 1, "Except::ok").expect("ok decodes");
    assert_eq!(nat::try_to_u64(field).expect("Nat decodes"), n);
}

#[test]
fn except_string_nat_err_round_trips_via_fixture() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let err_fn = module
        .exported::<(String,), Obj<'_>>("lean_rs_fixture_except_string_nat_err")
        .expect("lookup");
    let s = "boom".to_owned();
    let obj = err_fn.call(s.clone()).expect("call");
    let tag = ctor_tag(&obj).expect("Except ctor");
    assert_eq!(tag, 0, "expected error");
    let [field] = take_ctor_objects::<1>(obj, 0, "Except::error").expect("error decodes");
    assert_eq!(String::try_from_lean(field).expect("String decodes"), s);
}

#[test]
fn except_trait_round_trip_via_lean_constructed_then_rust_decoded() {
    let runtime = runtime();
    let cases: Vec<Except<String, u64>> = vec![Except::Ok(0), Except::Ok(123), Except::Error("oops".to_owned())];
    for input in cases {
        let obj = input.clone().into_lean(runtime);
        let out = Except::<String, u64>::try_from_lean(obj).expect("Except round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn result_trait_round_trip_through_pure_abi() {
    let runtime = runtime();
    let cases: Vec<Result<u64, String>> = vec![Ok(0), Ok(u64::MAX), Err("nope".to_owned())];
    for input in cases {
        let obj = input.clone().into_lean(runtime);
        let out = Result::<u64, String>::try_from_lean(obj).expect("Result round-trips");
        assert_eq!(out, input);
    }
}

#[test]
fn except_returns_wrong_tag_for_bogus_ctor() {
    let runtime = runtime();
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
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let pair_fn = module
        .exported::<(Obj<'_>, String), Obj<'_>>("lean_rs_fixture_pair_make")
        .expect("lookup");
    let n: u64 = 1234;
    let s = "pair-field".to_owned();
    let pair = pair_fn.call(nat::from_u64(runtime, n), s.clone()).expect("call");
    let [first, second] = take_ctor_objects::<2>(pair, 0, "Pair").expect("Pair decodes");
    assert_eq!(nat::try_to_u64(first).expect("first decodes"), n);
    assert_eq!(String::try_from_lean(second).expect("second decodes"), s);
}

// -- Structure pattern: Bundle (String, Array String) --------------------

#[test]
fn bundle_make_round_trips_via_fixture() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let bundle_fn = module
        .exported::<(String, Vec<String>), Obj<'_>>("lean_rs_fixture_bundle_make")
        .expect("lookup");
    let name = "release".to_owned();
    let items: Vec<String> = vec!["x86_64".to_owned(), "aarch64".to_owned()];
    let bundle = bundle_fn.call(name.clone(), items.clone()).expect("call");
    let [name_field, items_field] = take_ctor_objects::<2>(bundle, 0, "Bundle").expect("Bundle decodes");
    assert_eq!(String::try_from_lean(name_field).expect("name decodes"), name);
    assert_eq!(Vec::<String>::try_from_lean(items_field).expect("items decode"), items);
}

#[test]
fn bundle_identity_round_trips_through_lean() {
    let runtime = runtime();
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let identity = module
        .exported::<(Obj<'_>,), Obj<'_>>("lean_rs_fixture_bundle_identity")
        .expect("lookup");
    let name = "alpha".to_owned();
    let items: Vec<String> = vec!["one".to_owned(), "two".to_owned(), "three".to_owned()];
    let bundle = alloc_ctor_with_objects(
        runtime,
        0,
        [string::from_str(runtime, &name), items.clone().into_lean(runtime)],
    );
    let echoed = identity.call(bundle).expect("call");
    let [name_field, items_field] = take_ctor_objects::<2>(echoed, 0, "Bundle").expect("Bundle decodes");
    assert_eq!(String::try_from_lean(name_field).expect("name decodes"), name);
    assert_eq!(Vec::<String>::try_from_lean(items_field).expect("items decode"), items);
}

// -- Composed nested containers (no fixture) -----------------------------

#[test]
fn vec_of_option_round_trips_through_pure_abi() {
    let runtime = runtime();
    let input: Vec<Option<u64>> = vec![None, Some(0), Some(42), None, Some(u64::MAX)];
    let obj = input.clone().into_lean(runtime);
    let out = Vec::<Option<u64>>::try_from_lean(obj).expect("Vec<Option> round-trips");
    assert_eq!(out, input);
}

#[test]
fn option_of_vec_round_trips_through_pure_abi() {
    let runtime = runtime();
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

// -- IO round trips through `LeanIo<R>` ----------------------------------

#[test]
fn io_success_unit_decodes_via_lean_io_marker() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let f = module
        .exported::<(), LeanIo<()>>("lean_rs_fixture_io_success_unit")
        .expect("lookup");
    f.call().expect("io_success_unit");
}

#[test]
fn io_success_nat_decodes_payload() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let f = module
        .exported::<(), LeanIo<Obj<'_>>>("lean_rs_fixture_io_success_nat")
        .expect("lookup");
    // `IO Nat` returns a scalar-tagged Nat payload, decoded via the
    // `nat::*` helpers (not the polymorphic-UInt64 `TryFromLean` impl).
    let nat_obj = f.call().expect("io_success_nat");
    let value = nat::try_to_u64(nat_obj).expect("Nat decodes");
    assert_eq!(value, 7);
}

#[test]
fn io_failure_decodes_inner_except() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    // `IO (Except String Nat)` — outer IO succeeds, inner Except carries
    // the failure. We get the inner `Obj<'_>` then walk the ctor.
    let f = module
        .exported::<(), LeanIo<Obj<'_>>>("lean_rs_fixture_io_failure")
        .expect("lookup");
    let inner = f.call().expect("io_failure outer IO succeeds");
    let tag = ctor_tag(&inner).expect("Except ctor");
    assert_eq!(tag, 0, "expected inner error");
    let [field] = take_ctor_objects::<1>(inner, 0, "Except::error").expect("error decodes");
    let message = String::try_from_lean(field).expect("String decodes");
    assert!(message.contains("deliberate inner failure"), "unexpected: {message:?}");
}

#[test]
fn io_throw_surfaces_lean_exception_with_user_error_kind() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let f = module
        .exported::<(), LeanIo<Obj<'_>>>("lean_rs_fixture_io_throw")
        .expect("lookup");
    match f.call() {
        Err(LeanError::LeanException(exc)) => {
            assert_eq!(exc.kind(), LeanExceptionKind::UserError);
            assert!(
                exc.message().contains("deliberate IO exception"),
                "unexpected message: {:?}",
                exc.message()
            );
        }
        other => panic!("expected LeanException(UserError), got {other:?}"),
    }
}

// -- Nullary-constant global handling ------------------------------------

#[test]
fn option_nat_none_resolves_as_global_and_decodes_via_arity_zero() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    // `lean_rs_fixture_option_nat_none : Option Nat := none` compiles to a
    // persistent `lean_object*` global (`lean_box(0)`, scalar-tagged for
    // Option.none). The typed handle reads the global's stored value,
    // lean_inc's it (no-op on a scalar), and decodes through
    // `Option::<u64>::try_from_lean` which handles both none and some.
    let none_handle = module
        .exported::<(), Option<u64>>("lean_rs_fixture_option_nat_none")
        .expect("arity-0 lookup against the global succeeds");
    let decoded = none_handle.call().expect("call returns the persistent none value");
    assert_eq!(decoded, None, "Option::none decodes to Rust None");
}

#[test]
fn arity_one_lookup_against_global_rejects_with_link_diagnostic() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let err = module
        .exported::<(u64,), u64>("lean_rs_fixture_option_nat_none")
        .expect_err("arity 1 against a global must fail at lookup");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Link);
            let message = failure.message();
            assert!(
                message.contains("lean_rs_fixture_option_nat_none"),
                "diagnostic must name the symbol, got: {message:?}",
            );
            assert!(
                message.contains("nullary-constant global"),
                "diagnostic must name the kind mismatch, got: {message:?}",
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Link), got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("unexpected cancellation: {cancelled:?}"),
    }
}

#[test]
fn lean_io_against_global_rejects_at_lookup() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let err = module
        .exported::<(), LeanIo<u64>>("lean_rs_fixture_option_nat_none")
        .expect_err("LeanIo against a global must fail at lookup");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Link);
            assert!(
                failure.message().contains("LeanIo"),
                "diagnostic must mention LeanIo: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Link), got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("unexpected cancellation: {cancelled:?}"),
    }
}

#[test]
fn unknown_symbol_lookup_surfaces_host_link_diagnostic() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init");
    let err = module
        .exported::<(), u64>("lean_rs_fixture_does_not_exist")
        .expect_err("unknown symbol must surface at lookup");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Link);
            assert!(
                failure.message().contains("lean_rs_fixture_does_not_exist"),
                "diagnostic must name the symbol: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Link), got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("unexpected cancellation: {cancelled:?}"),
    }
}
