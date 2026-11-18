//! `IntoLean` / `TryFromLean` for `Vec<T>` and matching free helpers for
//! preallocated construction.
//!
//! Lean's `Array α` is a [`LEAN_ARRAY`](lean_rs_sys::consts::LEAN_ARRAY)-
//! tagged object with `m_size`, `m_capacity`, and an inline payload of
//! `*mut lean_object` element slots. Every payload slot owns one
//! reference to its element; `lean_dec` on the array walks the visible
//! slots and decrements them in turn.
//!
//! The conversion path here:
//!
//! - Writes use [`lean_alloc_array`] to preallocate exactly `n` slots in
//!   a single call, then transfer each `T::into_lean(runtime)` result
//!   into its slot via [`lean_array_set_core`]. There is no
//!   `lean_array_push` / amortised-growth loop and no intermediate
//!   `Obj::clone` (which would bump `lean_inc`).
//! - Reads call [`lean_array_size`] once, allocate the Rust [`Vec`] with
//!   matching capacity, then for each slot `lean_inc` the borrowed
//!   element pointer, wrap it as an owned [`Obj`], and decode through
//!   `T::try_from_lean`. The source array's [`Drop`] balances each
//!   `lean_inc` so the parent ownership is released without leaking
//!   elements.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment naming the invariant. The blanket allow keeps the
// unsafe surface inside the smallest scope that compiles, per
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use lean_rs_sys::array::{lean_alloc_array, lean_array_cptr, lean_array_set_core, lean_array_size};
use lean_rs_sys::object::{lean_is_array, lean_is_scalar, lean_obj_tag};
use lean_rs_sys::refcount::lean_inc;

use crate::abi::traits::{IntoLean, TryFromLean, conversion_error};
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Build a Lean `Array α` from a Vec whose initialized length is known.
fn from_vec<'lean, T>(runtime: &'lean LeanRuntime, values: Vec<T>) -> Obj<'lean>
where
    T: IntoLean<'lean>,
{
    let len = values.len();
    // SAFETY: `lean_alloc_array(len, len)` returns a fresh array with
    // refcount 1 and `len` uninitialised slots; we initialise each slot
    // exactly once from the iterator's elements via `lean_array_set_core`
    // (the documented owned-write entry point) before the array escapes,
    // discharging the per-slot ownership obligation.
    unsafe {
        let raw = lean_alloc_array(len, len);
        for (i, value) in values.into_iter().enumerate() {
            lean_array_set_core(raw, i, value.into_lean(runtime).into_raw());
        }
        Obj::from_owned_raw(runtime, raw)
    }
}

impl<'lean, T> IntoLean<'lean> for Vec<T>
where
    T: IntoLean<'lean>,
{
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        from_vec(runtime, self)
    }
}

impl<'lean, T> TryFromLean<'lean> for Vec<T>
where
    T: TryFromLean<'lean>,
{
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_array(&obj)?;
        let runtime = obj.runtime();
        let ptr = obj.as_raw_borrowed();
        // SAFETY: `require_array` validated `obj` is a Lean object array,
        // so `lean_array_size` reads the header field, and
        // `lean_array_cptr` returns a pointer valid for `size` element
        // slots.
        let size = unsafe { lean_array_size(ptr) };
        let slots = unsafe { lean_array_cptr(ptr) };
        tracing::trace!(
            target: "lean_rs",
            shape = "array",
            len = size,
            "lean_rs.abi.decode",
        );
        let mut out: Self = Self::with_capacity(size);
        for i in 0..size {
            // SAFETY: `i < size` keeps the slot index in bounds; each
            // slot points to a live Lean object owned by `obj`. We bump
            // its refcount before wrapping so the resulting `Obj` owns
            // its own count; `obj`'s `Drop` releases the array (and the
            // original per-slot counts) once the loop completes.
            unsafe {
                let elem_ptr = *slots.add(i);
                lean_inc(elem_ptr);
                let elem = Obj::from_owned_raw(runtime, elem_ptr);
                out.push(T::try_from_lean(elem)?);
            }
        }
        Ok(out)
    }
}

/// Validate that `obj` is a Lean object array.
fn require_array(obj: &Obj<'_>) -> LeanResult<()> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` inspects pointer bits only.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(wrong_kind_scalar());
    }
    // SAFETY: non-scalar; tag inspection reads the object header.
    if unsafe { lean_is_array(ptr) } {
        Ok(())
    } else {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        Err(wrong_kind_heap(found_tag))
    }
}

fn wrong_kind_scalar() -> LeanError {
    conversion_error("expected Lean Array, found scalar-tagged object")
}

fn wrong_kind_heap(found_tag: u32) -> LeanError {
    conversion_error(format!("expected Lean Array, found object with tag {found_tag}"))
}

// -- LeanAbi: Vec<T> is boxed at the Lake C ABI (`lean_object*`) ------

// SAFETY: Array uses an owned object slot; LeanBoxedAbi establishes every encoded and decoded element type.
unsafe impl<'lean, T> crate::abi::traits::LeanAbi<'lean> for Vec<T>
where
    T: crate::abi::traits::LeanBoxedAbi<'lean>,
{
    type CRepr = *mut lean_rs_sys::lean_object;
    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        <Self as crate::abi::traits::LeanBoxedAbi<'lean>>::into_boxed(self, runtime).into_raw()
    }
    unsafe fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        // SAFETY: the native result has the complete type established at binding.
        unsafe { <Self as crate::abi::traits::LeanBoxedAbi<'lean>>::from_boxed(obj) }
    }
}
