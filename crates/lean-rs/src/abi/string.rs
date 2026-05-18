//! `String` ↔ Rust `String` / `&str` conversions.
//!
//! Lean's `String` is a heap-allocated `lean_string_object` carrying a
//! UTF-8 byte buffer with a trailing NUL terminator
//! (`m_size = bytes + 1`). Round-trips preserve embedded NUL bytes
//! because the conversions use the size field rather than C-string
//! semantics.
//!
//! The [`IntoLean`] impl on `String` always allocates inside Lean (no
//! way to avoid the copy when crossing the FFI boundary). The
//! [`from_str`] helper does the same starting from a borrowed `&str`.
//!
//! Reading is split:
//! - [`TryFromLean`] for `String` copies the bytes into a fresh Rust
//!   allocation (one heap copy on the Rust side).
//! - [`borrow_str`] returns a `&str` view into the Lean buffer, avoiding
//!   the Rust-side allocation. The view is tied to the source [`ObjRef`]
//!   lifetime so it cannot outlive the underlying Lean object.

#![allow(unsafe_code)]

use core::ffi::c_char;
use core::slice;

use lean_rs_sys::object::{lean_is_scalar, lean_is_string, lean_obj_tag};
use lean_rs_sys::string::{lean_mk_string_from_bytes_unchecked, lean_string_cstr, lean_string_size};

use crate::abi::traits::{IntoLean, TryFromLean, conversion_error};
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::{Obj, ObjRef};

impl<'lean> IntoLean<'lean> for String {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        from_str(runtime, self.as_str())
    }
}

/// Build a Lean `String` from a borrowed Rust `&str`.
///
/// One Lean-side allocation; no Rust-side copies beyond the temporary
/// `c_char` pointer cast. Handles embedded NUL bytes correctly because the
/// underlying call uses the explicit byte length instead of `strlen`.
#[must_use]
pub(crate) fn from_str<'lean>(runtime: &'lean LeanRuntime, s: &str) -> Obj<'lean> {
    let bytes = s.as_bytes();
    // SAFETY: `bytes` is a `&[u8]` slice of valid UTF-8 (Rust's `str`
    // invariant), so `lean_mk_string_from_bytes_unchecked`'s precondition
    // is met. The returned object owns one refcount.
    unsafe {
        let raw = lean_mk_string_from_bytes_unchecked(bytes.as_ptr().cast::<c_char>(), bytes.len());
        Obj::from_owned_raw(runtime, raw)
    }
}

impl<'lean> TryFromLean<'lean> for String {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_string(&obj)?;
        // SAFETY: kind verified; `lean_string_size` returns the byte length
        // including the trailing NUL, `lean_string_cstr` returns a borrowed
        // pointer to the UTF-8 bytes. We materialise an owned Rust copy.
        let owned = unsafe {
            let ptr = obj.as_raw_borrowed();
            let size_with_nul = lean_string_size(ptr);
            let len = size_with_nul.saturating_sub(1);
            let data = lean_string_cstr(ptr).cast::<u8>();
            let slice = slice::from_raw_parts(data, len);
            slice.to_vec()
        };
        tracing::trace!(
            target: "lean_rs",
            shape = "string",
            len = owned.len(),
            "lean_rs.abi.decode",
        );
        Self::from_utf8(owned).map_err(|_| invalid_utf8())
    }
}

/// Borrow a `&str` view of a Lean `String` without copying.
///
/// The returned slice is tied to `obj`'s lifetime; it must not outlive
/// the borrowed `ObjRef`. This is the only zero-allocation path for
/// reading a Lean `String` from Rust.
///
/// # Errors
///
/// Returns `LeanError::Host { stage: Conversion, .. }` if `obj` is not a
/// Lean `String`, or if its bytes are not valid UTF-8 (defensive — Lean
/// enforces the invariant, but we honour it rather than relying on
/// `from_utf8_unchecked`).
pub(crate) fn borrow_str<'a>(obj: &'a ObjRef<'_, '_>) -> LeanResult<&'a str> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(wrong_kind_scalar());
    }
    // SAFETY: non-scalar branch; tag read on owned-by-source object.
    if !unsafe { lean_is_string(ptr) } {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        return Err(wrong_kind_heap(found_tag));
    }
    // SAFETY: kind verified; the slice borrows the Lean payload for `'a`,
    // which is bounded by the lifetime of the `ObjRef` we received.
    let bytes = unsafe {
        let size_with_nul = lean_string_size(ptr);
        let len = size_with_nul.saturating_sub(1);
        let data = lean_string_cstr(ptr).cast::<u8>();
        slice::from_raw_parts(data, len)
    };
    core::str::from_utf8(bytes).map_err(|_| invalid_utf8())
}

fn require_string(obj: &Obj<'_>) -> LeanResult<()> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(wrong_kind_scalar());
    }
    // SAFETY: non-scalar branch; tag read on the owned object.
    if unsafe { lean_is_string(ptr) } {
        Ok(())
    } else {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(wrong_kind_heap(found_tag))
    }
}

fn wrong_kind_scalar() -> LeanError {
    conversion_error("expected Lean String, found scalar-tagged object")
}

fn wrong_kind_heap(found_tag: u32) -> LeanError {
    conversion_error(format!("expected Lean String, found object with tag {found_tag}"))
}

fn invalid_utf8() -> LeanError {
    conversion_error("Lean string bytes were not valid UTF-8")
}

// -- LeanAbi: String is boxed at the Lake C ABI (`lean_object*`) ------

impl crate::abi::traits::sealed::SealedAbi for String {}
impl<'lean> crate::abi::traits::LeanAbi<'lean> for String {
    type CRepr = *mut lean_rs_sys::lean_object;
    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }
    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
    )]
    fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` is a `lean_obj_res` owning one reference per
        // Lake's contract; wrap in `Obj` then decode through the
        // polymorphic-boxing path.
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Self::try_from_lean(obj)
    }
}

// -- LeanAbi: &str is encode-only at the Lake C ABI -------------------
//
// Lake's C ABI for borrowed-string arguments uses the same boxed
// `lean_object*` representation as owned `String`. A borrowed-encode
// path avoids the per-call `String::to_owned()` that callers would
// otherwise perform to satisfy `LeanAbi<'lean> for String`. The
// elaborate / kernel_check / make_name shims on `LeanSession` each take
// `&str` from the caller and previously paid an extra `to_owned()`
// solely to reach the `String` `LeanAbi` impl; with this impl those
// shims pass the slice straight through.
//
// `from_c` is unreachable through any caller-constructible flow: a
// borrowed-string return position has no lifetime to borrow from, since
// `LeanAbi::from_c`'s signature does not bind the input pointer to any
// `'a`. The body releases the inbound owned reference and returns a
// conversion error so the impl is honest about the limitation rather
// than panicking. Code that needs to *read* a Lean `String` uses
// `String` (owned decode) or `borrow_str(&ObjRef)` (zero-copy view).
impl crate::abi::traits::sealed::SealedAbi for &str {}
impl<'lean> crate::abi::traits::LeanAbi<'lean> for &str {
    type CRepr = *mut lean_rs_sys::lean_object;
    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        from_str(runtime, self).into_raw()
    }
    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
    )]
    fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` owns one Lean reference per Lake's `lean_obj_res`
        // contract; wrap-and-drop releases the count so we do not leak
        // when this unreachable branch is reached.
        drop(unsafe { Obj::from_owned_raw(runtime, c) });
        Err(conversion_error(
            "&str cannot decode a Lean call result; use `String` for an owned copy \
             or `borrow_str(&ObjRef)` for a zero-copy view",
        ))
    }
}
