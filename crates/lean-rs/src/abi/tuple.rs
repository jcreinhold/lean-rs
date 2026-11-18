//! Lean `Prod` ABI support for small Rust tuples.
//!
//! Rust argument tuples normally describe a function's arity for
//! [`crate::LeanExported`]. These impls are different: they encode a
//! tuple *as one Lean value* when a Lean export itself takes a product
//! argument, e.g. `Expr × Expr × UInt8`.

#![allow(unsafe_code)]

use lean_rs_sys::lean_object;

use crate::abi::structure::{alloc_ctor_with_objects, take_ctor_objects};
use crate::abi::traits::{IntoLean, LeanAbi, TryFromLean};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

impl<'lean, A, B> IntoLean<'lean> for (A, B)
where
    A: IntoLean<'lean>,
    B: IntoLean<'lean>,
{
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        let (a, b) = self;
        alloc_ctor_with_objects(runtime, 0, [a.into_lean(runtime), b.into_lean(runtime)])
    }
}

impl<'lean, A, B> TryFromLean<'lean> for (A, B)
where
    A: TryFromLean<'lean>,
    B: TryFromLean<'lean>,
{
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [a, b] = take_ctor_objects::<2>(obj, 0, "Prod.mk")?;
        Ok((A::try_from_lean(a)?, B::try_from_lean(b)?))
    }
}

// SAFETY: Prod uses an owned object slot and native codecs for its two fields.
unsafe impl<'lean, A, B> LeanAbi<'lean> for (A, B)
where
    A: crate::abi::traits::LeanBoxedAbi<'lean>,
    B: crate::abi::traits::LeanBoxedAbi<'lean>,
{
    type CRepr = *mut lean_object;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        <Self as crate::abi::traits::LeanBoxedAbi<'lean>>::into_boxed(self, runtime).into_raw()
    }

    unsafe fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` carries one owned refcount returned from a Lean
        // export whose result type is a product.
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        // SAFETY: the native result has the complete type established at binding.
        unsafe { <Self as crate::abi::traits::LeanBoxedAbi<'lean>>::from_boxed(obj) }
    }
}

impl<'lean, A, B, C> IntoLean<'lean> for (A, B, C)
where
    A: IntoLean<'lean>,
    B: IntoLean<'lean>,
    C: IntoLean<'lean>,
{
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        let (a, b, c) = self;
        (a, (b, c)).into_lean(runtime)
    }
}

impl<'lean, A, B, C> TryFromLean<'lean> for (A, B, C)
where
    A: TryFromLean<'lean>,
    B: TryFromLean<'lean>,
    C: TryFromLean<'lean>,
{
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let (a, (b, c)) = <(A, (B, C))>::try_from_lean(obj)?;
        Ok((a, b, c))
    }
}

// SAFETY: the triple uses right-associated Prod layout with native codecs for every field.
unsafe impl<'lean, A, B, C> LeanAbi<'lean> for (A, B, C)
where
    A: crate::abi::traits::LeanBoxedAbi<'lean>,
    B: crate::abi::traits::LeanBoxedAbi<'lean>,
    C: crate::abi::traits::LeanBoxedAbi<'lean>,
{
    type CRepr = *mut lean_object;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        <Self as crate::abi::traits::LeanBoxedAbi<'lean>>::into_boxed(self, runtime).into_raw()
    }

    unsafe fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` carries one owned refcount returned from a Lean
        // export whose result type is a product.
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        // SAFETY: the native result has the complete type established at binding.
        unsafe { <Self as crate::abi::traits::LeanBoxedAbi<'lean>>::from_boxed(obj) }
    }
}
