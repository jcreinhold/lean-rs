//! Native boxed decoding kept separate from safe, untyped object inspection.

use super::except::Except;
use super::traits::{IntoLean, LeanBoxedAbi, TryFromLean};
use crate::{LeanResult, LeanRuntime, Obj};

/// Keep native element encoding independent of safely extensible `IntoLean`.
struct NativeElement<T>(T);

impl<'lean, T: LeanBoxedAbi<'lean>> IntoLean<'lean> for NativeElement<T> {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        self.0.into_boxed(runtime)
    }
}

macro_rules! checked_leaves {
    ($($ty:ty),* $(,)?) => {$(
        // SAFETY: each leaf's checked decoder releases the object on both
        // success and failure and preserves its runtime lifetime.
        #[allow(unsafe_code, reason = "adapt a checked leaf decoder to native typed decoding")]
        unsafe impl<'lean> LeanBoxedAbi<'lean> for $ty {
            fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
                self.into_lean(runtime)
            }

            unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
                Self::try_from_lean(obj)
            }
        }
    )*};
}

checked_leaves!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    usize,
    i8,
    i16,
    i32,
    i64,
    isize,
    f64,
    char,
    String
);

// SAFETY: the identity decoder retains the original owned object and lifetime.
#[allow(unsafe_code, reason = "native identity decoder retains object ownership")]
unsafe impl<'lean> LeanBoxedAbi<'lean> for Obj<'lean> {
    fn into_boxed(self, _runtime: &'lean LeanRuntime) -> Self {
        self
    }

    unsafe fn from_boxed(obj: Self) -> LeanResult<Self> {
        Ok(obj)
    }
}

// SAFETY: the caller's Array element type establishes T's layout for every
// extracted field; the checked array decoder first acquires owned references.
#[allow(
    unsafe_code,
    reason = "propagate native Array element types through owned references"
)]
unsafe impl<'lean, T: LeanBoxedAbi<'lean>> LeanBoxedAbi<'lean> for Vec<T> {
    fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        self.into_iter()
            .map(NativeElement)
            .collect::<Vec<_>>()
            .into_lean(runtime)
    }

    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
        Vec::<Obj<'lean>>::try_from_lean(obj)?
            .into_iter()
            .map(|element| {
                // SAFETY: the native Array T contract establishes this element's T layout.
                unsafe { T::from_boxed(element) }
            })
            .collect()
    }
}

// SAFETY: the native Option T contract establishes the payload type; checked
// extraction owns that payload before delegating to its decoder.
#[allow(unsafe_code, reason = "propagate the native Option payload type")]
unsafe impl<'lean, T: LeanBoxedAbi<'lean>> LeanBoxedAbi<'lean> for Option<T> {
    fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        self.map(NativeElement).into_lean(runtime)
    }

    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
        Option::<Obj<'lean>>::try_from_lean(obj)?
            .map(|value| {
                // SAFETY: Option T's present value has the caller-established T layout.
                unsafe { T::from_boxed(value) }
            })
            .transpose()
    }
}

// SAFETY: native Except E T establishes both possible payload types; checked
// extraction transfers exactly the selected branch's reference.
#[allow(unsafe_code, reason = "propagate native Except payload types")]
unsafe impl<'lean, T, E> LeanBoxedAbi<'lean> for Result<T, E>
where
    T: LeanBoxedAbi<'lean>,
    E: LeanBoxedAbi<'lean>,
{
    fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        self.map(NativeElement).map_err(NativeElement).into_lean(runtime)
    }

    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
        match Result::<Obj<'lean>, Obj<'lean>>::try_from_lean(obj)? {
            // SAFETY: native Except E T's ok branch carries T.
            Ok(value) => unsafe { T::from_boxed(value) }.map(Ok),
            // SAFETY: native Except E T's error branch carries E.
            Err(error) => unsafe { E::from_boxed(error) }.map(Err),
        }
    }
}

// SAFETY: Except and Result share the same branch layout and ownership.
#[allow(unsafe_code, reason = "reuse the native Except decoder")]
unsafe impl<'lean, E, T> LeanBoxedAbi<'lean> for Except<E, T>
where
    T: LeanBoxedAbi<'lean>,
    E: LeanBoxedAbi<'lean>,
{
    fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        Result::<T, E>::from(self).into_boxed(runtime)
    }

    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
        // SAFETY: the caller established the same Except E T layout.
        unsafe { Result::<T, E>::from_boxed(obj) }.map(Self::from)
    }
}

// SAFETY: native Prod A B establishes each field's type; checked extraction
// yields independently owned fields before either native decoder runs.
#[allow(unsafe_code, reason = "propagate native product field types")]
unsafe impl<'lean, A, B> LeanBoxedAbi<'lean> for (A, B)
where
    A: LeanBoxedAbi<'lean>,
    B: LeanBoxedAbi<'lean>,
{
    fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        let (a, b) = self;
        (NativeElement(a), NativeElement(b)).into_lean(runtime)
    }

    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
        let (a, b) = <(Obj<'lean>, Obj<'lean>)>::try_from_lean(obj)?;
        // SAFETY: native Prod A B supplies A and B in these owned fields.
        unsafe { Ok((A::from_boxed(a)?, B::from_boxed(b)?)) }
    }
}

// SAFETY: Lean's right-associated triple is Prod A (Prod B C).
#[allow(unsafe_code, reason = "decode a native right-associated triple")]
unsafe impl<'lean, A, B, C> LeanBoxedAbi<'lean> for (A, B, C)
where
    A: LeanBoxedAbi<'lean>,
    B: LeanBoxedAbi<'lean>,
    C: LeanBoxedAbi<'lean>,
{
    fn into_boxed(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        let (a, b, c) = self;
        (a, (b, c)).into_boxed(runtime)
    }

    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
        // SAFETY: the native triple contract supplies the nested product layout.
        let (a, (b, c)) = unsafe { <(A, (B, C))>::from_boxed(obj) }?;
        Ok((a, b, c))
    }
}
