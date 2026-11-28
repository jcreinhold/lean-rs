//! Conversion traits for first-order Lean values.
//!
//! Owned-data conversions and explicitly unsafe native ABI conversions.
//!
//! - [`IntoLean`] / [`TryFromLean`]—convert between Rust
//!   values and polymorphic-boxed [`Obj`]. Used for container elements,
//!   structure fields, and any Lean position where the value lives behind
//!   a `lean_object*`. The classic encoding/decoding direction.
//! - [`LeanAbi`]—convert between Rust values and the
//!   *C-ABI representation* Lake emits for a top-level Lean export
//!   parameter or return. The C representation varies: `uint8_t` for
//!   `Bool`, `uint32_t` for `Char`, `double` for `Float`, scalar primitive
//!   for `UIntN`/`UIntN`, and `lean_object*` for everything boxed. This
//!   trait drives [`crate::module::LeanExported`]'s typed function-pointer
//!   cast.
//!
//! Native ABI conversions coexist with safe owned-data conversions because
//! they encode different
//! conventions for the same Rust type: `u8 as IntoLean` produces a
//! polymorphic-boxed `lean_box(u8 as usize)`, while `u8 as LeanAbi`
//! produces an unboxed `uint8_t` matching Lake's emitted signature.
//!
//! Borrowed conversions do not introduce a new trait. Where a Rust
//! borrowed type appears in a Lean export's argument tuple, the per-type
//! module adds an `impl LeanAbi for &T` rather than a new
//! `BorrowedLeanAbi` trait. The `&str` impl in `super::string` is the
//! earned case: `LeanSession::elaborate`, `kernel_check`, `elaborate_bulk`,
//! and `make_name` each accepted `&str` from callers and previously paid
//! a `String::to_owned()` only to reach `LeanAbi<'lean> for String`.
//! A borrowed-only read, such as the test-only `bytearray::borrow_bytes`,
//! stays a free function because it is a zero-copy *return*-direction
//! helper and never needs to satisfy the `LeanAbi` arg-tuple bound.

use lean_rs_sys::lean_object;
use lean_toolchain::LeanExportAbiRepr;

#[cfg(doc)]
use crate::error::{HostStage, LeanDiagnosticCode};
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// C representation types supported by manifest-backed export signatures.
///
/// This is deliberately narrower than Rust FFI in general: Lean exports use
/// either `lean_object*` or the scalar shapes Lake emits for first-order Lean
/// values.
pub trait LeanCReprAbi: Copy + 'static + c_repr_sealed::Sealed {
    /// Manifest ABI representation for this C slot.
    const EXPORT_ABI_REPR: LeanExportAbiRepr;
}

mod c_repr_sealed {
    #[allow(unreachable_pub, reason = "private supertrait seals the supported C representations")]
    pub trait Sealed {}
}

impl c_repr_sealed::Sealed for *mut lean_object {}
impl LeanCReprAbi for *mut lean_object {
    const EXPORT_ABI_REPR: LeanExportAbiRepr = LeanExportAbiRepr::LeanObject;
}

macro_rules! impl_c_repr_abi {
    ($($ty:ty => $repr:ident),* $(,)?) => {
        $(
            impl c_repr_sealed::Sealed for $ty {}
            impl LeanCReprAbi for $ty {
                const EXPORT_ABI_REPR: LeanExportAbiRepr = LeanExportAbiRepr::$repr;
            }
        )*
    };
}

impl_c_repr_abi! {
    u8 => U8,
    u16 => U16,
    u32 => U32,
    u64 => U64,
    usize => USize,
    i8 => I8,
    i16 => I16,
    i32 => I32,
    i64 => I64,
    isize => ISize,
    f64 => F64,
}

/// Move a Rust value into a freshly owned Lean object.
///
/// The returned [`Obj`] carries exactly one Lean reference count and is
/// anchored to the `&'lean LeanRuntime` borrow that witnessed the call.
pub trait IntoLean<'lean>: Sized {
    /// Allocate (or scalar-box) a Lean representation of `self` and return
    /// the owned handle.
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean>;
}

/// Decode an owned Lean object into a Rust value.
///
/// Consumes the [`Obj`]—even on failure, the refcount is released by
/// `obj`'s `Drop`. The function signature returns the error type without
/// the Obj because the cases where the caller wants to recover the
/// original `Obj` are rare; if they arise, we will add a `try_from_lean_ref`
/// variant against an `ObjRef` rather than complicating this trait.
pub trait TryFromLean<'lean>: Sized {
    /// Decode `obj` into `Self`, returning a
    /// [`LeanError::Host`](LeanError) with stage
    /// [`HostStage::Conversion`] if the object's kind or payload is
    /// outside the type's representable range.
    ///
    /// # Errors
    ///
    /// Per-impl behaviours are documented at the impl site. Helpers in
    /// the per-type modules use the [`conversion_error`] free
    /// function to build the bounded diagnostic.
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self>;
}

/// Convert boxed native values whose Lean type is established by the caller.
///
/// Unlike [`TryFromLean`], this contract may rely on the object's Lean type.
/// It supports opaque semantic handles and containers containing those handles
/// without offering a safe cast from an arbitrary [`Obj`].
///
/// # Safety
///
/// Implementations must decode every supported value of their documented Lean
/// type without violating object ownership, runtime lifetime, or thread rules.
/// Errors must release owned references. A decoder must not manufacture a
/// semantic handle for a different Lean type.
/// Encoders must produce a fully initialized owned value of that same Lean type.
#[allow(
    unsafe_code,
    reason = "native decoding requires a caller-established Lean value type"
)]
pub unsafe trait LeanBoxedAbi<'lean>: Sized {
    /// Encode a value using its documented Lean polymorphic representation.
    fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean>;

    /// Consume an owned native value of the Lean type represented by `Self`.
    ///
    /// # Safety
    ///
    /// `obj` and its reachable fields must have the complete Lean value layout
    /// expected by this implementation. Pointer ownership alone is insufficient.
    /// The object must belong to its lifetime's initialized runtime and be used
    /// on an attached thread. The caller transfers the owned reference.
    ///
    /// # Errors
    ///
    /// Returns a conversion error when a valid Lean value cannot be represented
    /// by `Self`.
    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self>;
}

/// Build a `LeanError::Host { stage: Conversion, .. }` carrying a uniform
/// diagnostic.
///
/// Centralised so per-type ABI impls share the wording and so a future
/// log/sink can hook one site instead of N.
pub fn conversion_error(message: impl Into<String>) -> LeanError {
    LeanError::abi_conversion(message)
}

/// Per-type C-ABI representation used by [`crate::module::LeanExported`].
///
/// Lake emits unboxed C primitives for `UIntN`/`IntN`/`USize`/`ISize`/
/// `Bool`/`Char`/`Float` exports; boxed `lean_object*` for everything else
/// (`String`, `ByteArray`, `Nat`, `Int`, structures, IO results, …). The
/// per-type [`CRepr`](LeanAbi::CRepr) records which convention applies.
///
/// `into_c` / `from_c` are paired: a type's `CRepr` is invariant between
/// the encode and decode directions, so they share one contract.
///
/// # Safety
///
/// Implementations must use the C representation and ownership convention of
/// their documented Lean type. Encoders must produce initialized owned values
/// of that type for every accepted Rust value. Decoders must preserve reference
/// ownership and runtime lifetimes for valid native results of that type.
/// Generic export binding additionally requires the caller to establish that
/// the actual native Lean types match these conversions; matching C slots does
/// not establish boxed layouts or scalar refinements.
#[allow(unsafe_code, reason = "implementing a native conversion establishes FFI invariants")]
pub unsafe trait LeanAbi<'lean>: Sized {
    /// The C-ABI type Lake emits for this Lean type at function
    /// signatures.
    type CRepr: LeanCReprAbi;

    /// Encode `self` into the C-ABI representation. The returned value
    /// is suitable for passing as a function argument; ownership of any
    /// allocated Lean object is transferred to the receiver.
    #[doc(hidden)]
    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr;

    /// Decode an owned C-ABI value into [`Self`].
    ///
    /// For boxed `CRepr = *mut lean_object`, the pointer carries one
    /// owned reference count (per Lake's `lean_obj_res` ownership
    /// contract); `from_c` consumes it.
    ///
    /// # Safety
    ///
    /// `c` must be a valid initialized value of this implementation's complete
    /// Lean type and C representation. A boxed pointer must be non-null, carry
    /// one transferred reference, and belong to `runtime`. Its reachable fields
    /// must have the expected types and layouts. Use it only on an attached
    /// runtime thread. A scalar must satisfy its Lean type's refinements.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Conversion`]
    /// if the value cannot be decoded into `Self` (kind mismatch,
    /// out-of-range bignum, malformed UTF-8, non-Unicode `char` payload).
    #[doc(hidden)]
    unsafe fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self>;
}

// -- LeanAbi for Obj<'lean> -------------------------------------------
//
// The identity impl: `Obj<'lean>` already IS the boxed C ABI shape.
// Lets `LeanExported<(Obj,), Obj>` work for tests that pass Lean values
// constructed via per-type helpers (`nat::from_u64`, `string::from_str`,
// …) directly without re-typing.

// SAFETY: Obj transfers the same owned reference in both directions and does
// not claim a more specific Lean value type than the caller established.
#[allow(unsafe_code, reason = "identity conversion for an owned native object")]
unsafe impl<'lean> LeanAbi<'lean> for Obj<'lean> {
    type CRepr = *mut lean_object;
    fn into_c(self, _runtime: &'lean LeanRuntime) -> *mut lean_object {
        self.into_raw()
    }
    unsafe fn from_c(c: *mut lean_object, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` carries one owned reference count returned from
        // an extern Lean function (per Lake's `lean_obj_res` contract).
        // `runtime` is the witness for `'lean`.
        #[allow(unsafe_code)]
        Ok(unsafe { Obj::from_owned_raw(runtime, c) })
    }
}

// `Obj<'lean>: TryFromLean<'lean>` is the identity decoder. It lets a
// caller write `LeanIo<Obj<'lean>>` as the typed handle's return type to
// get the raw IO payload back as an `Obj`, then decode through a
// per-type helper (`nat::try_to_u64`, `ctor_tag`, …) when the value
// shape doesn't fit a polymorphic-boxing `TryFromLean` impl.
//
// `Obj<'lean>` deliberately does NOT implement `IntoLean<'lean>`—
// passing an `Obj` as an argument goes through `LeanAbi::into_c`
// (identity), not through the polymorphic-boxing path.

impl<'lean> TryFromLean<'lean> for Obj<'lean> {
    fn try_from_lean(obj: Self) -> LeanResult<Self> {
        Ok(obj)
    }
}
