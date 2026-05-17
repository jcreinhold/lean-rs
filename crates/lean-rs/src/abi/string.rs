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

use crate::abi::traits::{IntoLean, TryFromLean};
use crate::error::ConversionError;
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
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
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
        Self::from_utf8(owned).map_err(|_| ConversionError::InvalidUtf8)
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
/// Returns [`ConversionError::WrongObjectKind`] if `obj` is not a Lean
/// `String`, or [`ConversionError::InvalidUtf8`] if the bytes are not
/// valid UTF-8 (defensive — Lean enforces the invariant).
pub(crate) fn borrow_str<'a>(obj: &'a ObjRef<'_, '_>) -> Result<&'a str, ConversionError> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(ConversionError::WrongObjectKind {
            expected: "String",
            found_tag: u32::MAX,
        });
    }
    // SAFETY: non-scalar branch; tag read on owned-by-source object.
    if !unsafe { lean_is_string(ptr) } {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        return Err(ConversionError::WrongObjectKind {
            expected: "String",
            found_tag,
        });
    }
    // SAFETY: kind verified; the slice borrows the Lean payload for `'a`,
    // which is bounded by the lifetime of the `ObjRef` we received.
    let bytes = unsafe {
        let size_with_nul = lean_string_size(ptr);
        let len = size_with_nul.saturating_sub(1);
        let data = lean_string_cstr(ptr).cast::<u8>();
        slice::from_raw_parts(data, len)
    };
    core::str::from_utf8(bytes).map_err(|_| ConversionError::InvalidUtf8)
}

fn require_string(obj: &Obj<'_>) -> Result<(), ConversionError> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` reads pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(ConversionError::WrongObjectKind {
            expected: "String",
            found_tag: u32::MAX,
        });
    }
    // SAFETY: non-scalar branch; tag read on the owned object.
    if unsafe { lean_is_string(ptr) } {
        Ok(())
    } else {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(ConversionError::WrongObjectKind {
            expected: "String",
            found_tag,
        })
    }
}
