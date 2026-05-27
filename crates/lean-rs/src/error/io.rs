//! Decode Lean `IO α` results.
//!
//! Compiled Lean `IO α` actions return a `lean_object*` that is a
//! `Lean.EStateM.Result`-shaped constructor: tag 0 (`ok`) carries the
//! value, tag 1 (`error`) carries an `IO.Error`. [`decode_io`] inspects
//! the tag via the helpers in [`lean_rs_sys::io`] and returns either
//! the owned success [`Obj`] or a classified
//! [`LeanError::LeanException`]. Application-level ABI conversion of
//! the success value (`TryFromLean`, scalar / `Nat` / `String` decoders)
//! is the caller's job—separating the two responsibilities keeps each
//! decode site readable and avoids tying `IO α` to a single Rust type.
//!
//! Classification is anchored to Lean 4.29.1's
//! `src/lean/Init/System/IOError.lean` declaration order (see
//! [`KIND_TABLE`]). A unit test pins the `userError` mapping against
//! the live runtime via the existing `lean_rs_fixture_io_throw`
//! fixture—that test fails if the table drifts and the fix is to
//! update `KIND_TABLE` to match the new declaration order.
//!
//! Message extraction is best-effort. Most `IO.Error` constructors
//! carry the human-readable detail as their last object field (a
//! Lean `String`); we read that field and bound it. Constructors
//! without an object field (today: `unexpectedEof`) and unknown tags
//! collapse to a generic placeholder. Faithful per-variant rendering
//! is out of scope here—it would require pretty-printing arbitrary
//! Lean exceptions through `MetaM`, which the bounded `host::meta`
//! surface does not currently expose.

#![allow(unsafe_code)]
#![allow(
    dead_code,
    reason = "decoder helpers reached through generic dispatch; lib-only build cannot prove reachability"
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
    //! Direct `decode_io` round trips. The fixture IO exports are
    //! reachable through the typed
    //! [`crate::module::LeanExported`] handle; these tests strip the
    //! handle and exercise `decode_io` on the raw result `Obj` to keep
    //! the decoder under direct test (independent of the typed-handle
    //! composition).
    //!
    //! The `userError` test pins the Lean-4.29.1 constructor index for
    //! `userError` (`18`) against the live runtime, guarding
    //! [`super::KIND_TABLE`].

    #![allow(unsafe_code, clippy::expect_used, clippy::panic)]

    use std::path::PathBuf;

    use super::decode_io;
    use crate::LeanRuntime;
    use crate::abi::nat;
    use crate::error::{LeanError, LeanExceptionKind};
    use crate::module::LeanLibrary;
    use crate::runtime::obj::Obj;

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

    /// Open the fixture library and initialize its root module so the
    /// per-test `decode_io` calls can resolve `Effects` exports.
    fn open_fixture(runtime: &LeanRuntime) -> LeanLibrary<'_> {
        let path = fixture_dylib_path();
        assert!(path.exists(), "fixture dylib not found at {}", path.display());
        let library = LeanLibrary::open(runtime, &path).expect("fixture dylib opens cleanly");
        // Initializing the root cascades into `Effects` (where the IO
        // fixtures live); drop the typed handle—these tests reach the
        // raw entry points via the library directly.
        drop(
            library
                .initialize_module("lean_rs_fixture", "LeanRsFixture")
                .expect("fixture root module initializes"),
        );
        library
    }

    /// Resolve the raw entry point for a fixture function and call it
    /// with the IO world token, returning the owned result `Obj`.
    fn call_io_raw<'lean>(library: &LeanLibrary<'lean>, runtime: &'lean LeanRuntime, symbol: &str) -> Obj<'lean> {
        let addr = library.resolve_function_symbol(symbol).expect("symbol resolves");
        type FnPtr = unsafe extern "C" fn(*mut lean_rs_sys::lean_object) -> *mut lean_rs_sys::lean_object;
        // SAFETY: every fixture IO export has Lake-emitted signature
        // `fn(world: *mut lean_object) -> *mut lean_object`.
        let f: FnPtr = unsafe { core::mem::transmute::<*mut core::ffi::c_void, FnPtr>(addr) };
        // SAFETY: `lean_box(0)` is the conventional scalar world token.
        let world = unsafe { lean_rs_sys::object::lean_box(0) };
        // SAFETY: the function takes ownership of `world` (a scalar—no
        // refcount transfer) and returns an owned IO result pointer.
        let raw = unsafe { f(world) };
        // SAFETY: `raw` is the owned IO result returned by Lake's IO export.
        unsafe { Obj::from_owned_raw(runtime, raw) }
    }

    #[test]
    fn decode_io_ok_returns_value() {
        let runtime = LeanRuntime::init().expect("runtime init");
        let library = open_fixture(runtime);
        let result = call_io_raw(&library, runtime, "lean_rs_fixture_io_success_nat");
        let value_obj = decode_io(runtime, result).expect("ioSuccessNat decodes");
        // `pure 7 : IO Nat` returns a Lean `Nat`, decoded by the Nat
        // helper rather than by a `TryFromLean` impl for u64 (which
        // expects the polymorphic UInt64 boxing).
        let value = nat::try_to_u64(value_obj).expect("scalar Nat decodes");
        assert_eq!(value, 7);
    }

    #[test]
    fn decode_io_error_returns_lean_exception() {
        let runtime = LeanRuntime::init().expect("runtime init");
        let library = open_fixture(runtime);
        let result = call_io_raw(&library, runtime, "lean_rs_fixture_io_throw");
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
