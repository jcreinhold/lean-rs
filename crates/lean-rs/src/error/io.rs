//! Decode Lean `IO α` results.
//!
//! Compiled Lean `IO α` actions return a `lean_object*` that is a
//! `Lean.EStateM.Result`-shaped constructor: tag 0 (`ok`) carries the
//! value, tag 1 (`error`) carries an `IO.Error`. [`decode_io`] inspects
//! the tag via the helpers in [`lean_rs_sys::io`] and returns either
//! the owned success [`Obj`] or a classified
//! [`LeanError::LeanException`]. Application-level ABI conversion of
//! the success value (`TryFromLean`, scalar / `Nat` / `String` decoders)
//! is the caller's job — separating the two responsibilities keeps each
//! decode site readable and avoids tying `IO α` to a single Rust type.
//!
//! Classification is anchored to Lean 4.29.1's
//! `src/lean/Init/System/IOError.lean` declaration order (see
//! [`KIND_TABLE`]). A unit test pins the `userError` mapping against
//! the live runtime via the existing `lean_rs_fixture_io_throw`
//! fixture — that test fails if the table drifts and the fix is to
//! update `KIND_TABLE` to match the new declaration order.
//!
//! Message extraction is best-effort. Most `IO.Error` constructors
//! carry the human-readable detail as their last object field (a
//! Lean `String`); we read that field and bound it. Constructors
//! without an object field (today: `unexpectedEof`) and unknown tags
//! collapse to a generic placeholder. Faithful per-variant rendering
//! is out of scope for prompt 10 (the prompt excludes
//! "pretty-printing arbitrary Lean exceptions through `MetaM`").

#![allow(unsafe_code)]
#![allow(
    dead_code,
    reason = "first non-test caller lands in prompts 11–12 (LeanModule + LeanExported{N})"
)]

use core::slice;

use lean_rs_sys::ctor::{lean_ctor_num_objs, lean_ctor_obj_cptr};
use lean_rs_sys::io::{lean_io_result_get_error, lean_io_result_is_ok, lean_io_result_take_value};
use lean_rs_sys::object::{lean_is_scalar, lean_is_string, lean_obj_tag};
use lean_rs_sys::string::{lean_string_cstr, lean_string_size};

use super::{LeanError, LeanExceptionKind, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Decode an owned Lean `IO α` result, returning the owned success
/// value as an [`Obj`] or a classified [`LeanError::LeanException`].
///
/// On `IO.ok`, the carried value is detached from the result (its
/// refcount transferred into the returned [`Obj`]) and the result
/// object is dropped. On `IO.error`, the `IO.Error` constructor is
/// classified into a [`LeanExceptionKind`] and a bounded message is
/// extracted before the result is dropped.
///
/// The caller decodes the success [`Obj`] through whichever ABI helper
/// matches the Lean type of `α` (a `TryFromLean` impl for polymorphic
/// scalars, `nat::try_to_u64` for `Nat`, `string::borrow_str` for
/// `String`, …). Splitting the two concerns avoids tying every IO
/// decode to one Rust type and keeps each call site honest about the
/// `IO α`-to-Rust mapping it expects.
pub(crate) fn decode_io<'lean>(runtime: &'lean LeanRuntime, result: Obj<'lean>) -> LeanResult<Obj<'lean>> {
    let ptr = result.as_raw_borrowed();
    // SAFETY: `result` is an owned Lean `IO α` result tagged 0 or 1 by
    // Lean's codegen; `lean_io_result_is_ok` only reads the header tag.
    if unsafe { lean_io_result_is_ok(ptr) } {
        // SAFETY: tag == 0 → `take_value` bumps the value's refcount
        // and decrements the result. Consuming `result.into_raw()`
        // transfers our one reference to `take_value`, which is the
        // ownership contract of `lean_io_result_take_value`.
        let value_ptr = unsafe { lean_io_result_take_value(result.into_raw()) };
        // SAFETY: `value_ptr` is non-null and we own the single
        // reference count `take_value` produced. Wrap it in an `Obj`
        // tied to `runtime`.
        let value_obj = unsafe { Obj::from_owned_raw(runtime, value_ptr) };
        Ok(value_obj)
    } else {
        // SAFETY: tag == 1 → `get_error` borrows the `IO.Error` payload
        // out of the result. The borrow is valid as long as `result`
        // (and the `Obj` that owns it) is alive; we use it before
        // dropping the `Obj` at end-of-scope.
        let err_ptr = unsafe { lean_io_result_get_error(ptr) };
        let (kind, message) = classify_io_error(err_ptr);
        drop(result);
        Err(LeanError::lean_exception(kind, message))
    }
}

/// `IO.Error` constructor tag → public kind enum.
///
/// Anchored to Lean 4.29.1:
/// `src/lean/Init/System/IOError.lean:24` defines the inductive in
/// this declaration order. If a future Lean version reorders, adds, or
/// removes constructors, the unit test
/// `io::tests::user_error_is_classified_as_user_error` fails and the
/// fix is a coordinated update to this table plus the
/// `VERSION-COMPATIBILITY` contract in `00-current-state.md`.
const KIND_TABLE: &[LeanExceptionKind] = &[
    LeanExceptionKind::AlreadyExists,          // 0
    LeanExceptionKind::OtherError,             // 1
    LeanExceptionKind::ResourceBusy,           // 2
    LeanExceptionKind::ResourceVanished,       // 3
    LeanExceptionKind::UnsupportedOperation,   // 4
    LeanExceptionKind::HardwareFault,          // 5
    LeanExceptionKind::UnsatisfiedConstraints, // 6
    LeanExceptionKind::IllegalOperation,       // 7
    LeanExceptionKind::ProtocolError,          // 8
    LeanExceptionKind::TimeExpired,            // 9
    LeanExceptionKind::Interrupted,            // 10
    LeanExceptionKind::NoFileOrDirectory,      // 11
    LeanExceptionKind::InvalidArgument,        // 12
    LeanExceptionKind::PermissionDenied,       // 13
    LeanExceptionKind::ResourceExhausted,      // 14
    LeanExceptionKind::InappropriateType,      // 15
    LeanExceptionKind::NoSuchThing,            // 16
    LeanExceptionKind::UnexpectedEof,          // 17
    LeanExceptionKind::UserError,              // 18
];

/// Classify and stringify an `IO.Error` borrowed payload. Reads only
/// the header tag and (for constructors with at least one object
/// field) the last object field, which holds the human-readable
/// detail in every `IO.Error` constructor that has one.
fn classify_io_error(err: *mut lean_rs_sys::lean_object) -> (LeanExceptionKind, String) {
    // SAFETY: `err` is a borrowed Lean object pointer; `lean_is_scalar`
    // inspects only the pointer bits.
    if unsafe { lean_is_scalar(err) } {
        // `IO.Error` is an algebraic data type whose constructors all
        // contain at least one field; scalar tagging here would mean
        // Lean encoded a 0-arity constructor as a scalar. None of the
        // current constructors qualify, but treat it defensively.
        return (
            LeanExceptionKind::Other,
            "<Lean IO error: scalar-encoded constructor>".to_owned(),
        );
    }
    // SAFETY: non-scalar → tag read is layout-pinned.
    let ctor = unsafe { lean_obj_tag(err) };
    let kind = KIND_TABLE
        .get(ctor as usize)
        .copied()
        .unwrap_or(LeanExceptionKind::Other);

    let message = read_last_string_field(err).unwrap_or_else(|| format!("<Lean IO error: constructor tag {ctor}>"));

    (kind, message)
}

/// Read the last object field of a Lean constructor as an owned UTF-8
/// string, if there is one and it is a Lean `String`. Returns `None`
/// otherwise (no fields, or the last field is not a `String`).
fn read_last_string_field(ctor: *mut lean_rs_sys::lean_object) -> Option<String> {
    // SAFETY: `ctor` is a non-scalar Lean ctor; `lean_ctor_num_objs`
    // reads the header field that holds the object-field count.
    let n = unsafe { lean_ctor_num_objs(ctor) };
    if n == 0 {
        return None;
    }
    // SAFETY: header read; `obj_cptr` points at the object-field array.
    let fields = unsafe { lean_ctor_obj_cptr(ctor) };
    // `n >= 1` from the early-return above, so the subtraction is in range;
    // `saturating_sub` placates the arithmetic-side-effects lint.
    let last_index = usize::from(n).saturating_sub(1);
    // SAFETY: `n > 0`, so the last index is in bounds.
    let last = unsafe { *fields.add(last_index) };
    // SAFETY: pointer-bits only.
    if unsafe { lean_is_scalar(last) } {
        return None;
    }
    // SAFETY: non-scalar, so the tag read is well-formed.
    if !unsafe { lean_is_string(last) } {
        return None;
    }
    // SAFETY: confirmed string. Read the bytes and copy them into an
    // owned `String`; the borrow ends here, so the bound on lifetimes
    // is local.
    let bytes = unsafe {
        let size_with_nul = lean_string_size(last);
        let len = size_with_nul.saturating_sub(1);
        let data = lean_string_cstr(last).cast::<u8>();
        slice::from_raw_parts(data, len)
    };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    //! IO-result decoder round trips against the existing
    //! `Effects.lean` fixtures. The `userError` test pins the
    //! Lean-4.29.1 constructor index for `userError` (`18`) against
    //! the live runtime, guarding [`super::KIND_TABLE`].

    #![allow(unsafe_code, clippy::expect_used, clippy::panic)]

    use lean_rs_sys::types::lean_object;
    use lean_rs_test_support::fixture;

    use super::decode_io;
    use crate::LeanRuntime;
    use crate::abi::nat;
    use crate::error::{LeanError, LeanExceptionKind};
    use crate::runtime::obj::Obj;

    unsafe extern "C" {
        fn lean_rs_fixture_io_success_nat(world: *mut lean_object) -> *mut lean_object;
        fn lean_rs_fixture_io_throw(world: *mut lean_object) -> *mut lean_object;
    }

    fn init() -> &'static LeanRuntime {
        let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
        fixture::init_fixture();
        runtime
    }

    /// IO world token passed to `IO α` exports. Lean treats the world
    /// as opaque; any non-null scalar-tagged value works at the C
    /// boundary.
    fn world_token() -> *mut lean_object {
        // SAFETY: `lean_box(0)` produces a scalar-tagged sentinel; the
        // fixture functions ignore the value.
        unsafe { lean_rs_sys::object::lean_box(0) }
    }

    #[test]
    fn decode_io_ok_returns_value() {
        let runtime = init();
        // SAFETY: fixture export is `IO Nat` and returns an owned
        // result.
        let result = unsafe {
            let raw = lean_rs_fixture_io_success_nat(world_token());
            Obj::from_owned_raw(runtime, raw)
        };
        let value_obj = decode_io(runtime, result).expect("ioSuccessNat decodes");
        // `pure 7 : IO Nat` returns a Lean `Nat`, decoded by the Nat
        // helper rather than by a `TryFromLean` impl for u64 (which
        // expects the polymorphic UInt64 boxing).
        let value = nat::try_to_u64(value_obj).expect("scalar Nat decodes");
        assert_eq!(value, 7);
    }

    #[test]
    fn decode_io_error_returns_lean_exception() {
        let runtime = init();
        // SAFETY: fixture export is `IO Nat` that throws.
        let result = unsafe {
            let raw = lean_rs_fixture_io_throw(world_token());
            Obj::from_owned_raw(runtime, raw)
        };
        match decode_io(runtime, result) {
            Err(LeanError::LeanException(exc)) => {
                assert_eq!(exc.kind(), LeanExceptionKind::UserError);
                assert!(
                    exc.message().contains("deliberate IO exception"),
                    "unexpected message: {:?}",
                    exc.message()
                );
            }
            other => panic!("expected LeanException, got {other:?}"),
        }
    }

    #[test]
    fn kind_table_length_matches_known_constructor_count() {
        // 19 IO.Error constructors at Lean 4.29.1; the catch-all
        // `LeanExceptionKind::Other` is for tags outside the table.
        assert_eq!(super::KIND_TABLE.len(), 19);
    }
}
