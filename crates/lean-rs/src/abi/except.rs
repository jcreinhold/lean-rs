//! The `Except<E, T>` value type and round-trips against Rust
//! `Result<T, E>`.
//!
//! Lean's `Except ε α` is a two-constructor inductive whose declaration
//! order — `error` then `ok` — is load-bearing for the C ABI tag:
//!
//! ```text
//! inductive Except (ε : Type u) (α : Type v) where
//!   | error : ε → Except ε α   -- tag 0, num_objs 1
//!   | ok    : α → Except ε α   -- tag 1, num_objs 1
//! ```
//!
//! Per `RD-2026-05-17-004` and `docs/architecture/03-host-api.md`
//! §"Error model", `Except<E, T>` is **a value type, not part of the
//! `LeanError` boundary**. Runtime / host failures cross as
//! `LeanError::LeanException`; application-level success-or-error
//! semantics cross as values. A Lean function returning
//! `IO (Except E T)` therefore decodes as `LeanResult<Result<T, E>>`
//! through the `decode_io` helper in [`crate::error::io`].
//!
//! Two trait surfaces are exposed:
//!
//! 1. [`Except<E, T>`] is the precise Lean-shaped mirror (variant
//!    ordering matches Lean's tag order). The `decode_io` helper and the
//!    typed [`crate::module::LeanExported`] handles use this type
//!    directly when the Lean inductive is `Except`.
//! 2. `Result<T, E>` carries the same impls so Rust call sites can stay
//!    in their native error type without an extra `.into()` hop.
//!    Internally the `Result` impls funnel through the `Except` impls via
//!    [`From`] so the tag-encoding rule lives in exactly one place.

use crate::abi::structure::{alloc_ctor_with_objects, ctor_tag, take_ctor_objects};
use crate::abi::traits::{IntoLean, TryFromLean, conversion_error};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Rust mirror of Lean's `Except ε α` inductive, in declaration order.
///
/// Used internally by [`crate::abi`] to thread Lean-shaped result values
/// through the ABI. Public callers see Rust [`Result`] via the round-trip
/// impls below.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Except<E, T> {
    /// Constructor `Except.error e` — tag 0.
    Error(E),
    /// Constructor `Except.ok a` — tag 1.
    Ok(T),
}

impl<E, T> From<Except<E, T>> for Result<T, E> {
    fn from(value: Except<E, T>) -> Self {
        match value {
            Except::Ok(t) => Self::Ok(t),
            Except::Error(e) => Self::Err(e),
        }
    }
}

impl<E, T> From<Result<T, E>> for Except<E, T> {
    fn from(value: Result<T, E>) -> Self {
        match value {
            Ok(t) => Self::Ok(t),
            Err(e) => Self::Error(e),
        }
    }
}

impl<'lean, E, T> IntoLean<'lean> for Except<E, T>
where
    E: IntoLean<'lean>,
    T: IntoLean<'lean>,
{
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Error(e) => alloc_ctor_with_objects(runtime, 0, [e.into_lean(runtime)]),
            Self::Ok(t) => alloc_ctor_with_objects(runtime, 1, [t.into_lean(runtime)]),
        }
    }
}

impl<'lean, E, T> TryFromLean<'lean> for Except<E, T>
where
    E: TryFromLean<'lean>,
    T: TryFromLean<'lean>,
{
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let tag = ctor_tag(&obj)?;
        match tag {
            0 => {
                let [field] = take_ctor_objects::<1>(obj, 0, "Except::error")?;
                Ok(Self::Error(E::try_from_lean(field)?))
            }
            1 => {
                let [field] = take_ctor_objects::<1>(obj, 1, "Except::ok")?;
                Ok(Self::Ok(T::try_from_lean(field)?))
            }
            other => Err(conversion_error(format!(
                "expected Lean Except ctor (tag 0 = error, 1 = ok), found tag {other}"
            ))),
        }
    }
}

impl<'lean, T, E> IntoLean<'lean> for Result<T, E>
where
    E: IntoLean<'lean>,
    T: IntoLean<'lean>,
{
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        Except::<E, T>::from(self).into_lean(runtime)
    }
}

impl<'lean, T, E> TryFromLean<'lean> for Result<T, E>
where
    E: TryFromLean<'lean>,
    T: TryFromLean<'lean>,
{
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        Except::<E, T>::try_from_lean(obj).map(Self::from)
    }
}

// -- LeanAbi: Except<E, T> and Result<T, E> are boxed at the Lake C ABI -

#[allow(unsafe_code, reason = "LeanAbi::from_c wraps an owned `lean_obj_res` pointer")]
mod lean_abi_impls {
    use super::{Except, IntoLean, LeanResult, LeanRuntime, Obj, TryFromLean};

    impl<E, T> crate::abi::traits::sealed::SealedAbi for Except<E, T> {}
    impl<'lean, E, T> crate::abi::traits::LeanAbi<'lean> for Except<E, T>
    where
        E: IntoLean<'lean> + TryFromLean<'lean>,
        T: IntoLean<'lean> + TryFromLean<'lean>,
    {
        type CRepr = *mut lean_rs_sys::lean_object;
        fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
            self.into_lean(runtime).into_raw()
        }
        #[allow(
            clippy::not_unsafe_ptr_arg_deref,
            reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
        )]
        fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
            // SAFETY: `c` is a `lean_obj_res` owning one refcount per
            // Lake's contract.
            let obj = unsafe { Obj::from_owned_raw(runtime, c) };
            Self::try_from_lean(obj)
        }
    }

    impl<T, E> crate::abi::traits::sealed::SealedAbi for Result<T, E> {}
    impl<'lean, T, E> crate::abi::traits::LeanAbi<'lean> for Result<T, E>
    where
        T: IntoLean<'lean> + TryFromLean<'lean>,
        E: IntoLean<'lean> + TryFromLean<'lean>,
    {
        type CRepr = *mut lean_rs_sys::lean_object;
        fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
            self.into_lean(runtime).into_raw()
        }
        #[allow(
            clippy::not_unsafe_ptr_arg_deref,
            reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
        )]
        fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
            // SAFETY: `c` is a `lean_obj_res` owning one refcount per
            // Lake's contract.
            let obj = unsafe { Obj::from_owned_raw(runtime, c) };
            Self::try_from_lean(obj)
        }
    }
}
