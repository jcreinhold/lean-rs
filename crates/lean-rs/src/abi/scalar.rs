//! `IntoLean` / `TryFromLean` for `()`, `bool`, fixed-width integers,
//! `usize`/`isize`, `char`, and `f64`.
//!
//! Lean's polymorphic boxing rules (`lean.h:2811–2873`):
//!
//! - `UInt8`, `UInt16`, `UInt32` (on 64-bit) and the matching `IntN` always
//!   fit in a scalar-tagged pointer via [`lean_rs_sys::object::lean_box`].
//! - `UInt64`, `USize`, `Float` are too wide for a scalar pointer; they are
//!   wrapped in a single-field constructor by the polymorphic-boxing
//!   helpers added in prompt 08 ([`lean_rs_sys::ctor::lean_box_uint64`]
//!   and friends).
//! - `Bool` is the two-constructor inductive `false → lean_box(0)`,
//!   `true → lean_box(1)`.
//! - `Unit` is the zero-arity constructor `Unit.unit → lean_box(0)`.
//! - `Char` is a `uint32_t` Unicode scalar value, polymorphic-boxed as
//!   `UInt32` (always ctor-boxed because constructors carrying chars
//!   land in the polymorphic-position layout).
//!
//! The unboxed direct-call form used by `LeanExported{N}` (prompt 12)
//! does not pass through this module; the trait surface here is for
//! polymorphic-position values that live inside Lean objects.

// SAFETY DOC: every `unsafe { ... }` block carries a per-block `// SAFETY:`
// comment naming the invariant; per the safety model we allow `unsafe`
// at the module scope.
#![allow(unsafe_code)]

use lean_rs_sys::ctor::{
    lean_box_float, lean_box_uint64, lean_box_usize, lean_unbox_float, lean_unbox_uint64, lean_unbox_usize,
};
use lean_rs_sys::object::{lean_box, lean_is_scalar, lean_obj_tag, lean_unbox};

use crate::abi::traits::{IntoLean, TryFromLean};
use crate::error::ConversionError;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

// -- helpers --------------------------------------------------------------

/// Re-label a [`ConversionError::WrongObjectKind`]'s `expected` string,
/// leaving every other variant untouched. Used by impls that delegate to a
/// sibling type's `TryFromLean` (e.g. `i64 → u64`, `char → u32`) so the
/// diagnostic carries the caller-visible Rust type name.
fn relabel_kind(err: ConversionError, expected: &'static str) -> ConversionError {
    if let ConversionError::WrongObjectKind { found_tag, .. } = err {
        ConversionError::WrongObjectKind { expected, found_tag }
    } else {
        err
    }
}

/// Require that `obj` is scalar-tagged; return [`ConversionError::WrongObjectKind`]
/// otherwise. Reads only pointer bits.
#[inline]
fn require_scalar(obj: &Obj<'_>, expected: &'static str) -> Result<(), ConversionError> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` inspects pointer bits only and is sound for
    // any pointer value (`lean.h:312`).
    if unsafe { lean_is_scalar(ptr) } {
        Ok(())
    } else {
        // SAFETY: non-scalar branch — `obj_tag` reads `m_tag`, which is valid
        // for any non-scalar Lean heap object we hold (`Obj` ownership).
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(ConversionError::WrongObjectKind { expected, found_tag })
    }
}

// -- () -------------------------------------------------------------------

impl<'lean> IntoLean<'lean> for () {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // SAFETY: `lean_box(0)` is pointer arithmetic only; the result is a
        // scalar-tagged pointer with a transferred (no-op) refcount.
        unsafe { Obj::from_owned_raw(runtime, lean_box(0)) }
    }
}

impl<'lean> TryFromLean<'lean> for () {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        require_scalar(&obj, "Unit")?;
        // Unit has a single inhabitant; the payload value is `0` by
        // construction. We do not assert on the unbox result because a
        // future Lean encoding change (e.g. tag != 0 for Unit) should not
        // silently fail at this layer — the prompt 09 ctor decoder will
        // catch true mismatches.
        Ok(())
    }
}

// -- bool -----------------------------------------------------------------

impl<'lean> IntoLean<'lean> for bool {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // SAFETY: scalar-box of `0` or `1`; pure pointer arithmetic.
        unsafe { Obj::from_owned_raw(runtime, lean_box(usize::from(self))) }
    }
}

impl<'lean> TryFromLean<'lean> for bool {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        require_scalar(&obj, "Bool")?;
        // SAFETY: scalar branch verified above; `lean_unbox` returns the
        // payload `usize`.
        let payload = unsafe { lean_unbox(obj.as_raw_borrowed()) };
        match payload {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ConversionError::OutOfRange { expected: "bool" }),
        }
    }
}

// -- macro-stamped: u8, u16, u32, i8, i16, i32 ---------------------------

/// Stamp `IntoLean` + `TryFromLean` for an integer type at most 32 bits
/// wide. Lean's `IntN` and `UIntN` share the same `uintN_t` bit pattern,
/// so signed values are encoded via their unsigned counterpart's bit
/// pattern (e.g. `i32::MIN` becomes `lean_box(0x8000_0000)` rather than
/// `lean_box(0xFFFF_FFFF_8000_0000)` which sign-extension would produce).
///
/// `$name` is the diagnostic label embedded in
/// [`ConversionError::OutOfRange`]; `$unsigned` is the bit-pattern
/// type used for the encoding (`u8` / `u16` / `u32`).
macro_rules! impl_scalar_abi_small_int {
    ($($ty:ty as $unsigned:ty => $name:literal),* $(,)?) => {
        $(
            impl<'lean> IntoLean<'lean> for $ty {
                fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
                    // SAFETY: cast through the unsigned counterpart preserves
                    // the bit pattern (matching Lean's `IntN`/`UIntN` shared
                    // representation), then widens to `usize` for
                    // scalar-tagged boxing. `lean_box` is pure pointer
                    // arithmetic.
                    let payload = self as $unsigned as usize;
                    unsafe { Obj::from_owned_raw(runtime, lean_box(payload)) }
                }
            }

            impl<'lean> TryFromLean<'lean> for $ty {
                fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
                    require_scalar(&obj, $name)?;
                    // SAFETY: scalar branch verified above.
                    let raw = unsafe { lean_unbox(obj.as_raw_borrowed()) };
                    // Decode via the unsigned counterpart so a negative
                    // `i8`/`i16`/`i32` whose bit pattern is in the low bits
                    // round-trips cleanly. Reject anything that doesn't fit
                    // the unsigned counterpart's range.
                    let unsigned = <$unsigned>::try_from(raw)
                        .map_err(|_| ConversionError::OutOfRange { expected: $name })?;
                    Ok(unsigned as $ty)
                }
            }
        )*
    };
}

impl_scalar_abi_small_int! {
    u8 as u8 => "u8",
    u16 as u16 => "u16",
    u32 as u32 => "u32",
    i8 as u8 => "i8",
    i16 as u16 => "i16",
    i32 as u32 => "i32",
}

// -- u64, usize, i64, isize -----------------------------------------------

impl<'lean> IntoLean<'lean> for u64 {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // SAFETY: `lean_box_uint64` allocates a single-field ctor with the
        // payload installed; refcount = 1 on return.
        unsafe { Obj::from_owned_raw(runtime, lean_box_uint64(self)) }
    }
}

impl<'lean> TryFromLean<'lean> for u64 {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        // Polymorphic `UInt64` is always ctor-boxed, never scalar-tagged.
        if !is_ctor(&obj) {
            return Err(ConversionError::WrongObjectKind {
                expected: "u64",
                // SAFETY: non-ctor branch; read the heap tag for the diagnostic.
                found_tag: unsafe { lean_obj_tag(obj.as_raw_borrowed()) },
            });
        }
        // SAFETY: kind check above gives us a ctor with the expected
        // single-`u64` payload at offset 0.
        Ok(unsafe { lean_unbox_uint64(obj.as_raw_borrowed()) })
    }
}

impl<'lean> IntoLean<'lean> for usize {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // SAFETY: see `u64`'s impl.
        unsafe { Obj::from_owned_raw(runtime, lean_box_usize(self)) }
    }
}

impl<'lean> TryFromLean<'lean> for usize {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        if !is_ctor(&obj) {
            return Err(ConversionError::WrongObjectKind {
                expected: "usize",
                // SAFETY: see `u64`'s impl.
                found_tag: unsafe { lean_obj_tag(obj.as_raw_borrowed()) },
            });
        }
        // SAFETY: see `u64`'s impl.
        Ok(unsafe { lean_unbox_usize(obj.as_raw_borrowed()) })
    }
}

impl<'lean> IntoLean<'lean> for i64 {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        #[allow(clippy::cast_sign_loss, reason = "Int64 reuses UInt64's bit pattern")]
        let bits = self as u64;
        bits.into_lean(runtime)
    }
}

impl<'lean> TryFromLean<'lean> for i64 {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        let bits = u64::try_from_lean(obj).map_err(|e| relabel_kind(e, "i64"))?;
        #[allow(clippy::cast_possible_wrap, reason = "Int64 reuses UInt64's bit pattern")]
        Ok(bits as Self)
    }
}

impl<'lean> IntoLean<'lean> for isize {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        #[allow(clippy::cast_sign_loss, reason = "ISize reuses USize's bit pattern")]
        let bits = self as usize;
        bits.into_lean(runtime)
    }
}

impl<'lean> TryFromLean<'lean> for isize {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        let bits = usize::try_from_lean(obj).map_err(|e| relabel_kind(e, "isize"))?;
        #[allow(clippy::cast_possible_wrap, reason = "ISize reuses USize's bit pattern")]
        Ok(bits as Self)
    }
}

// -- char -----------------------------------------------------------------

impl<'lean> IntoLean<'lean> for char {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // Lean's `Char` is a `uint32_t` Unicode scalar value. In a
        // polymorphic position Lean's `UInt32` is scalar-tagged on 64-bit
        // hosts (the encoding `lean_unbox_uint32` falls back to on 64-bit)
        // — keep the impl simple and unified with `u32`.
        u32::from(self).into_lean(runtime)
    }
}

impl<'lean> TryFromLean<'lean> for char {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        let code_point = u32::try_from_lean(obj).map_err(|e| relabel_kind(e, "char"))?;
        Self::from_u32(code_point).ok_or(ConversionError::InvalidChar { code_point })
    }
}

// -- f64 ------------------------------------------------------------------

impl<'lean> IntoLean<'lean> for f64 {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // SAFETY: `lean_box_float` allocates a single-field ctor; refcount
        // = 1 on return.
        unsafe { Obj::from_owned_raw(runtime, lean_box_float(self)) }
    }
}

impl<'lean> TryFromLean<'lean> for f64 {
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError> {
        if !is_ctor(&obj) {
            return Err(ConversionError::WrongObjectKind {
                expected: "f64",
                // SAFETY: see `u64`'s impl.
                found_tag: unsafe { lean_obj_tag(obj.as_raw_borrowed()) },
            });
        }
        // SAFETY: kind check above; the ctor's first scalar payload is the
        // installed `f64` from `lean_box_float`.
        Ok(unsafe { lean_unbox_float(obj.as_raw_borrowed()) })
    }
}

/// True if `obj` is a non-scalar heap object whose tag falls in the
/// constructor range (`tag <= LEAN_MAX_CTOR_TAG`).
#[inline]
fn is_ctor(obj: &Obj<'_>) -> bool {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` is pure pointer-bit math; the non-scalar
    // branch reads the header tag of an object we own.
    unsafe {
        if lean_is_scalar(ptr) {
            return false;
        }
        lean_rs_sys::object::lean_is_ctor(ptr)
    }
}
