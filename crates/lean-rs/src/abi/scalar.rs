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

use crate::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error, sealed};
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

// -- helpers --------------------------------------------------------------

/// Build a "expected Lean X, found …" conversion error for kind mismatches.
/// The single place this module formats wrong-kind messages, so the wording
/// stays uniform and grep-stable.
fn wrong_kind(expected: &str, found_tag: u32) -> LeanError {
    conversion_error(format!("expected Lean {expected}, found object with tag {found_tag}"))
}

fn wrong_kind_scalar(expected: &str) -> LeanError {
    conversion_error(format!("expected Lean {expected}, found scalar-tagged object"))
}

fn out_of_range(expected: &str) -> LeanError {
    conversion_error(format!("Lean value does not fit Rust {expected}"))
}

/// Require that `obj` is scalar-tagged; build a wrong-kind error otherwise.
/// `expected` is the Rust type name embedded in the diagnostic.
#[inline]
fn require_scalar(obj: &Obj<'_>, expected: &str) -> LeanResult<()> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` inspects pointer bits only and is sound for
    // any pointer value (`lean.h:312`).
    if unsafe { lean_is_scalar(ptr) } {
        Ok(())
    } else {
        // SAFETY: non-scalar branch — `obj_tag` reads `m_tag`, which is valid
        // for any non-scalar Lean heap object we hold (`Obj` ownership).
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(wrong_kind(expected, found_tag))
    }
}

/// Require that `obj` is a ctor-boxed value; build a wrong-kind error
/// otherwise. Used by the polymorphic-boxed wide scalars (`u64`, `usize`,
/// `f64`).
#[inline]
fn require_ctor(obj: &Obj<'_>, expected: &str) -> LeanResult<()> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: pure pointer-bit math.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(wrong_kind_scalar(expected));
    }
    // SAFETY: non-scalar branch; tag read on the owned object.
    if unsafe { lean_rs_sys::object::lean_is_ctor(ptr) } {
        Ok(())
    } else {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(wrong_kind(expected, found_tag))
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_scalar(&obj, "Bool")?;
        // SAFETY: scalar branch verified above; `lean_unbox` returns the
        // payload `usize`.
        let payload = unsafe { lean_unbox(obj.as_raw_borrowed()) };
        match payload {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(out_of_range("bool")),
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
/// `$name` is the diagnostic label embedded in the conversion error;
/// `$unsigned` is the bit-pattern type used for the encoding
/// (`u8` / `u16` / `u32`).
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
                fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
                    require_scalar(&obj, $name)?;
                    // SAFETY: scalar branch verified above.
                    let raw = unsafe { lean_unbox(obj.as_raw_borrowed()) };
                    // Decode via the unsigned counterpart so a negative
                    // `i8`/`i16`/`i32` whose bit pattern is in the low bits
                    // round-trips cleanly. Reject anything that doesn't fit
                    // the unsigned counterpart's range.
                    let unsigned = <$unsigned>::try_from(raw)
                        .map_err(|_| out_of_range($name))?;
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        // Polymorphic `UInt64` is always ctor-boxed, never scalar-tagged.
        require_ctor(&obj, "u64")?;
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_ctor(&obj, "usize")?;
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_ctor(&obj, "i64")?;
        // SAFETY: kind check above; same single-`u64` ctor as `UInt64`,
        // reinterpreted as `i64` for the caller.
        let bits = unsafe { lean_unbox_uint64(obj.as_raw_borrowed()) };
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_ctor(&obj, "isize")?;
        // SAFETY: kind check above; same single-`usize` ctor as `USize`.
        let bits = unsafe { lean_unbox_usize(obj.as_raw_borrowed()) };
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_scalar(&obj, "char")?;
        // SAFETY: scalar branch verified above.
        let raw = unsafe { lean_unbox(obj.as_raw_borrowed()) };
        let code_point = u32::try_from(raw).map_err(|_| out_of_range("char"))?;
        Self::from_u32(code_point)
            .ok_or_else(|| conversion_error(format!("Lean char {code_point:#x} is not a Unicode scalar value")))
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
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_ctor(&obj, "f64")?;
        // SAFETY: kind check above; the ctor's first scalar payload is the
        // installed `f64` from `lean_box_float`.
        Ok(unsafe { lean_unbox_float(obj.as_raw_borrowed()) })
    }
}

// -- LeanAbi: per-type C-ABI representation matching Lake's emitted ----
//
// Lake compiles a Lean parameter of type `T` to a C parameter whose
// representation depends on `T`:
//
//   * `Unit`                       → `lean_object*` (scalar `lean_box(0)`)
//   * `Bool`                       → `uint8_t`     (unboxed)
//   * `UInt8/16/32/64`, `USize`    → matching `uintN_t` / `size_t` (unboxed)
//   * `Int8/16/32/64`, `ISize`     → matching `uintN_t` (unboxed, sign-extended via cast)
//   * `Char`                       → `uint32_t`     (unboxed Unicode scalar)
//   * `Float`                      → `double`       (unboxed)
//
// The macro-stamped `LeanExported::call` casts the function pointer to
// match these per-arg CRepr types, then dispatches.
//
// `Unit` uses `*mut lean_object` (boxed) because Lake encodes `Unit`
// arguments as `lean_box(0)` at the C boundary — verified against
// `fixtures/lean/.lake/build/ir/LeanRsFixture/Scalars.c`
// (`lean_rs_fixture_unit_id(lean_object*)`).

impl sealed::SealedAbi for () {}
impl<'lean> LeanAbi<'lean> for () {
    type CRepr = *mut lean_rs_sys::lean_object;
    fn into_c(self, _runtime: &'lean LeanRuntime) -> Self::CRepr {
        // SAFETY: `lean_box(0)` is the scalar sentinel; no refcount.
        unsafe { lean_box(0) }
    }
    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
    )]
    fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: take ownership for Drop; consumes the returned object.
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        <()>::try_from_lean(obj)
    }
}

impl sealed::SealedAbi for bool {}
impl<'lean> LeanAbi<'lean> for bool {
    type CRepr = u8;
    fn into_c(self, _runtime: &'lean LeanRuntime) -> u8 {
        u8::from(self)
    }
    fn from_c(c: u8, _runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        match c {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(out_of_range("bool")),
        }
    }
}

/// Stamp `LeanAbi` for an unboxed-scalar Lean type whose Rust mirror is
/// directly the C representation Lake emits.
macro_rules! impl_scalar_abi_passthrough {
    ($($ty:ty),* $(,)?) => {
        $(
            impl sealed::SealedAbi for $ty {}
            impl<'lean> LeanAbi<'lean> for $ty {
                type CRepr = $ty;
                fn into_c(self, _runtime: &'lean LeanRuntime) -> $ty { self }
                fn from_c(c: $ty, _runtime: &'lean LeanRuntime) -> LeanResult<$ty> { Ok(c) }
            }
        )*
    };
}

impl_scalar_abi_passthrough!(u8, u16, u32, u64, usize, f64);

// Signed scalars share the unsigned bit pattern at Lake's C ABI; the
// CRepr is the signed type itself (rustc casts to/from the same bit
// pattern at the C boundary).
impl_scalar_abi_passthrough!(i8, i16, i32, i64, isize);

// Char is a UInt32 at the Lake C ABI. The encode/decode pair uses the
// `u32::CRepr` shape but validates the Unicode invariant on decode.
impl sealed::SealedAbi for char {}
impl<'lean> LeanAbi<'lean> for char {
    type CRepr = u32;
    fn into_c(self, _runtime: &'lean LeanRuntime) -> u32 {
        u32::from(self)
    }
    fn from_c(c: u32, _runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        Self::from_u32(c).ok_or_else(|| conversion_error(format!("Lean char {c:#x} is not a Unicode scalar value")))
    }
}
