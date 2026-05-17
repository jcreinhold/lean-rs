//! Conversion traits for first-order Lean values.
//!
//! [`IntoLean`] and [`TryFromLean`] are the universal currency of the
//! `pub(crate) abi` module: every per-type implementation in
//! [`crate::abi::scalar`], [`crate::abi::string`], and
//! [`crate::abi::bytearray`] implements one or both, and the typed
//! [`crate::module::LeanExported{N}`](crate) machinery (prompt 12) drives
//! its argument marshalling and return decoding through them.
//!
//! Both traits are `pub(crate)` per `RD-2026-05-17-004`; they never appear
//! in public docs and only stamp internal call sites.
//!
//! `IntoLean::into_lean` is infallible for the first-order types in scope
//! (a Rust `u64` always boxes; a Rust `String` always allocates). Failures
//! arrive on the read side: [`TryFromLean::try_from_lean`] returns
//! [`ConversionError`] for kind mismatches, bignum overflow, malformed
//! UTF-8, or non-scalar `char` payloads.
//!
//! Borrowed conversions (`&str`, `&[u8]`) live as free functions on the
//! per-type modules rather than as additional traits — keeping the trait
//! surface minimal until a real second caller earns the abstraction (per
//! the CLAUDE.md "no speculative traits with one implementor" rule).

use crate::error::ConversionError;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Move a Rust value into a freshly owned Lean object.
///
/// The returned [`Obj`] carries exactly one Lean reference count and is
/// anchored to the `&'lean LeanRuntime` borrow that witnessed the call.
pub(crate) trait IntoLean<'lean>: Sized {
    /// Allocate (or scalar-box) a Lean representation of `self` and return
    /// the owned handle.
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean>;
}

/// Decode an owned Lean object into a Rust value.
///
/// Consumes the [`Obj`] — even on failure, the refcount is released by
/// `obj`'s `Drop`. The function signature returns the error type without
/// the Obj because the cases where the caller wants to recover the
/// original `Obj` are rare; if they arise, we will add a `try_from_lean_ref`
/// variant against an `ObjRef` rather than complicating this trait.
pub(crate) trait TryFromLean<'lean>: Sized {
    /// Decode `obj` into `Self`, returning a [`ConversionError`] if the
    /// object's kind or payload is outside the type's representable range.
    ///
    /// # Errors
    ///
    /// See the [`ConversionError`] variants. Per-impl behaviours are
    /// documented at the impl site.
    fn try_from_lean(obj: Obj<'lean>) -> Result<Self, ConversionError>;
}
