//! `IntoLean` / `TryFromLean` for `()`, `bool`, fixed-width integers,
//! `usize`/`isize`, `char`, and `f64`.
//!
//! Lean's polymorphic boxing rules (`lean.h:2811–2873`):
//!
//! - `UInt8`, `UInt16`, `UInt32` (on 64-bit) and the matching `IntN` always
//!   fit in a scalar-tagged pointer via [`lean_rs_sys::object::lean_box`].
//! - `UInt64`, `USize`, `Float` are too wide for a scalar pointer; they
//!   are wrapped in a single-field constructor by the polymorphic-boxing
//!   helpers in [`lean_rs_sys::ctor`] ([`lean_rs_sys::ctor::lean_box_uint64`]
//!   and friends).
//! - `Bool` is the two-constructor inductive `false → lean_box(0)`,
//!   `true → lean_box(1)`.
//! - `Unit` is the zero-arity constructor `Unit.unit → lean_box(0)`.
//! - `Char` is a `uint32_t` Unicode scalar value, polymorphic-boxed as
//!   `UInt32` (always ctor-boxed because constructors carrying chars
//!   land in the polymorphic-position layout).
//!
//! The unboxed direct-call form used by [`crate::module::LeanExported`]
//! does not pass through this module; the trait surface here is for
//! polymorphic-position values that live inside Lean objects.

// SAFETY DOC: every `unsafe { ... }` block carries a per-block `// SAFETY:`
// comment naming the invariant; per the safety model we allow `unsafe`
// at the module scope.
#![allow(unsafe_code)]

use lean_rs_sys::ctor::{lean_box_float, lean_box_uint64, lean_box_usize};
use lean_rs_sys::object::{lean_box, lean_is_scalar, lean_obj_tag, lean_unbox};

use crate::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error};
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
        // SAFETY: non-scalar branch—`obj_tag` reads `m_tag`, which is valid
        // for any non-scalar Lean heap object we hold (`Obj` ownership).
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(wrong_kind(expected, found_tag))
    }
}

/// Check the constructor and initialized payload bounds before reading a wide
/// scalar. A constructor tag alone does not establish any scalar storage.
fn boxed_word(obj: &Obj<'_>, expected: &str) -> LeanResult<u64> {
    crate::abi::structure::view(obj)
        .ctor_shape(0, 0, expected)?
        .uint64(0, expected)
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
        // silently fail at this layer—the ctor decoder catches true
        // mismatches at the surrounding constructor boundary.
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
        boxed_word(&obj, "u64")
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
        Self::try_from(boxed_word(&obj, "usize")?).map_err(|_| out_of_range("usize"))
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
        let bits = boxed_word(&obj, "i64")?;
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
        let bits = usize::try_from(boxed_word(&obj, "isize")?).map_err(|_| out_of_range("isize"))?;
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
        //—keep the impl simple and unified with `u32`.
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
        boxed_word(&obj, "f64").map(Self::from_bits)
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
// arguments as `lean_box(0)` at the C boundary—verified against
// `fixtures/lean/.lake/build/ir/LeanRsFixture/Scalars.c`
// (`lean_rs_fixture_unit_id(lean_object*)`).

// SAFETY: Unit uses the scalar sentinel; its decoder consumes the owned boxed result.
unsafe impl<'lean> LeanAbi<'lean> for () {
    type CRepr = *mut lean_rs_sys::lean_object;
    fn into_c(self, _runtime: &'lean LeanRuntime) -> Self::CRepr {
        // SAFETY: `lean_box(0)` is the scalar sentinel; no refcount.
        unsafe { lean_box(0) }
    }
    unsafe fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: take ownership for Drop; consumes the returned object.
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        <()>::try_from_lean(obj)
    }
}

// SAFETY: Bool uses u8 values 0/1; decoding rejects other bytes and owns no references.
unsafe impl<'lean> LeanAbi<'lean> for bool {
    type CRepr = u8;
    fn into_c(self, _runtime: &'lean LeanRuntime) -> u8 {
        u8::from(self)
    }
    unsafe fn from_c(c: u8, _runtime: &'lean LeanRuntime) -> LeanResult<Self> {
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

            // SAFETY: this Rust scalar is the exact C slot type; identity conversion owns no references.
            unsafe impl<'lean> LeanAbi<'lean> for $ty {
                type CRepr = $ty;
                fn into_c(self, _runtime: &'lean LeanRuntime) -> $ty { self }
                unsafe fn from_c(c: $ty, _runtime: &'lean LeanRuntime) -> LeanResult<$ty> { Ok(c) }
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

// SAFETY: Char uses a u32 slot and validates the Unicode scalar invariant on decode.
unsafe impl<'lean> LeanAbi<'lean> for char {
    type CRepr = u32;
    fn into_c(self, _runtime: &'lean LeanRuntime) -> u32 {
        u32::from(self)
    }
    unsafe fn from_c(c: u32, _runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        Self::from_u32(c).ok_or_else(|| conversion_error(format!("Lean char {c:#x} is not a Unicode scalar value")))
    }
}
