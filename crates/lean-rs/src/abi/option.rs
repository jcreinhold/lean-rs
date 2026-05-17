//! `IntoLean` / `TryFromLean` for `Option<T>`.
//!
//! Lean's `Option α` is the two-constructor inductive
//!
//! ```text
//! inductive Option (α : Type u) where
//!   | none : Option α
//!   | some (val : α) : Option α
//! ```
//!
//! and Lean's compiler applies the standard mixed-inductive ABI rule:
//! **nullary constructors are scalar-tagged** (`lean_box(tag)`),
//! **constructors with fields are heap-allocated ctors** with the tag in
//! the header. Concretely for `Option`:
//!
//! - `none` → `lean_box(0)` — scalar-tagged, no allocation.
//! - `some x` → heap ctor with `tag = 1`, `num_objs = 1`, slot 0 = `x`.
//!
//! Matching Lean's encoding bit-for-bit is what lets Rust-built `Option`
//! values cross into Lean functions that pattern-match (the heap-ctor
//! encoding for None passes through `optionNatIdentity` undetected but
//! corrupts `match x with | none => …` in any real consumer).

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment naming the invariant; the blanket allow keeps the
// unsafe surface inside the smallest scope that compiles.
#![allow(unsafe_code)]

use lean_rs_sys::object::{lean_box, lean_is_ctor, lean_is_scalar, lean_obj_tag, lean_unbox};

use crate::abi::structure::{alloc_ctor_with_objects, take_ctor_objects};
use crate::abi::traits::{IntoLean, TryFromLean, conversion_error};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

impl<'lean, T> IntoLean<'lean> for Option<T>
where
    T: IntoLean<'lean>,
{
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            // SAFETY: `lean_box(0)` is pure pointer arithmetic; the
            // returned scalar-tagged pointer is non-null and the refcount
            // helpers short-circuit on it. Matches Lean's compiled
            // encoding of `Option.none`.
            None => unsafe { Obj::from_owned_raw(runtime, lean_box(0)) },
            Some(value) => alloc_ctor_with_objects(runtime, 1, [value.into_lean(runtime)]),
        }
    }
}

impl<'lean, T> TryFromLean<'lean> for Option<T>
where
    T: TryFromLean<'lean>,
{
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let ptr = obj.as_raw_borrowed();
        // SAFETY: `lean_is_scalar` reads pointer bits only.
        if unsafe { lean_is_scalar(ptr) } {
            // SAFETY: scalar branch — `lean_unbox` returns the encoded
            // tag (`Option.none` is `lean_box(0)`).
            let payload = unsafe { lean_unbox(ptr) };
            return match payload {
                0 => Ok(None),
                other => Err(conversion_error(format!(
                    "expected Lean Option.none (scalar tag 0), found scalar payload {other}"
                ))),
            };
        }
        // SAFETY: non-scalar branch; `lean_is_ctor` inspects the object
        // header only.
        if !unsafe { lean_is_ctor(ptr) } {
            // SAFETY: same branch.
            let found_tag = unsafe { lean_obj_tag(ptr) };
            return Err(conversion_error(format!(
                "expected Lean Option ctor or scalar tag, found object with tag {found_tag}"
            )));
        }
        // SAFETY: ctor object; tag is in `m_tag`.
        let tag = unsafe { lean_obj_tag(ptr) };
        if tag == 1 {
            let [inner] = take_ctor_objects::<1>(obj, 1, "Option::some")?;
            Ok(Some(T::try_from_lean(inner)?))
        } else {
            Err(conversion_error(format!(
                "expected Lean Option.some ctor (tag 1), found heap ctor with tag {tag}"
            )))
        }
    }
}

// -- LeanAbi: Option<T> is boxed at the Lake C ABI -------------------

impl<T> crate::abi::traits::sealed::SealedAbi for Option<T> {}
impl<'lean, T> crate::abi::traits::LeanAbi<'lean> for Option<T>
where
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
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Self::try_from_lean(obj)
    }
}
