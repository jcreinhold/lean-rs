//! Constructor-object plumbing for product and sum structures.
//!
//! The "structure pattern" lives at two primitives, hand-called at each
//! struct boundary. There is no per-struct trait, no derive, no
//! procedural macro: callers compose [`alloc_ctor_with_objects`] and
//! [`take_ctor_objects`] field by field and let the per-field
//! [`super::traits::IntoLean`] / [`super::traits::TryFromLean`] impls
//! do the actual type marshalling.
//!
//! The module is the only place in `abi` that knows how
//! [`lean_alloc_ctor`], [`lean_ctor_obj_cptr`], and the constructor's
//! `tag`/`num_objs` invariants line up; container modules
//! (`crate::abi::option`, `crate::abi::except`) and downstream
//! handlers ship through these primitives instead of repeating the
//! pointer arithmetic. That keeps a single audited copy of the
//! ctor-allocation rules and centralises the `lean_inc`/`lean_dec`
//! reasoning.
//!
//! ### Lifetime and refcount invariants
//!
//! - [`alloc_ctor_with_objects`] consumes the input array of `Obj<'lean>`
//!   handles. Each handle's owned refcount is transferred — via
//!   [`Obj::into_raw`] — into the freshly allocated constructor's
//!   object-slot, so the new parent owns exactly one count per field plus
//!   its own header count. No `Obj::clone` (which would `lean_inc`) runs
//!   on the input path.
//! - [`take_ctor_objects`] reads each object slot once, calls `lean_inc`
//!   on the field, and wraps the bumped pointer in a fresh `Obj<'lean>`.
//!   The parent `Obj` is then dropped; its `lean_dec` walks back through
//!   the original per-field counts, leaving each returned handle with the
//!   same effective ownership the parent had given that field.
//! - [`ctor_tag`] is a borrow-only read; it never touches the refcount.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment naming the invariant; the blanket allow keeps the
// unsafe surface inside the smallest scope that compiles, per
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use core::mem::MaybeUninit;

use lean_rs_sys::ctor::{lean_alloc_ctor, lean_ctor_num_objs, lean_ctor_obj_cptr};
use lean_rs_sys::object::{lean_is_ctor, lean_is_scalar, lean_obj_tag};
use lean_rs_sys::refcount::lean_inc;

use crate::abi::traits::conversion_error;
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Allocate a freshly-initialised constructor with `N` object-pointer
/// fields and no scalar payload.
///
/// `tag` is the inductive constructor index (Lean's declaration order:
/// `Option.none` = 0, `Option.some` = 1, `Except.error` = 0, `Except.ok`
/// = 1, …). Each entry of `objects` is moved into its slot via
/// [`Obj::into_raw`], so the returned [`Obj`] owns the only live refcount
/// per field plus its own header count. The const-generic `N` matches the
/// number of object-pointer slots the Lean inductive declares, which
/// keeps the call site self-documenting and lets the compiler refuse
/// arity mismatches.
///
/// # Panics
///
/// Panics only via `lean_alloc_ctor`'s `strict_*` arithmetic overflow
/// guard — unreachable for the constructor shapes Lean emits
/// (`LEAN_MAX_CTOR_FIELDS` = 256).
pub fn alloc_ctor_with_objects<'lean, const N: usize>(
    runtime: &'lean LeanRuntime,
    tag: u8,
    objects: [Obj<'lean>; N],
) -> Obj<'lean> {
    // SAFETY: `lean_alloc_ctor` returns a fresh ctor with refcount 1 and
    // `N` uninitialised object slots; we write each `Obj::into_raw` into
    // its slot before the object escapes, satisfying the
    // "fully initialise every declared field" obligation.
    unsafe {
        // The `num_objs` parameter is `u8`; assert at compile time that
        // `N` fits, matching `lean.h`'s `LEAN_MAX_CTOR_FIELDS` ceiling
        // (the const block evaluates at type-checking time).
        const { assert!(N <= u8::MAX as usize, "ctor arity exceeds Lean's u8 num_objs field") };
        let raw = lean_alloc_ctor(tag, N as u8, 0);
        let slots = lean_ctor_obj_cptr(raw);
        for (i, field) in objects.into_iter().enumerate() {
            *slots.add(i) = field.into_raw();
        }
        Obj::from_owned_raw(runtime, raw)
    }
}

/// Validate that `obj` is a constructor with `expected_tag` and exactly
/// `N` object-pointer fields, then return the `N` owned field handles.
///
/// Each returned [`Obj`] carries one refcount: [`lean_inc`] is called on
/// the slot pointer before wrapping it. The parent `obj` is consumed and
/// its [`Drop`] runs the matching `lean_dec` (which decrements each field
/// once more — balancing the `lean_inc`s and leaving the returned handles
/// with the same effective ownership the parent originally held).
///
/// `label` is embedded in the diagnostic on failure so callers see
/// `"expected Lean Option::some ctor (tag 1, num_objs 1), …"` rather
/// than an anonymous "wrong ctor".
///
/// # Errors
///
/// Returns [`HostStage::Conversion`](crate::error::HostStage::Conversion)
/// if `obj` is scalar-tagged, has a non-constructor heap tag, has a
/// different tag from `expected_tag`, or carries a different
/// object-slot count from `N`.
pub fn take_ctor_objects<'lean, const N: usize>(
    obj: Obj<'lean>,
    expected_tag: u8,
    label: &str,
) -> LeanResult<[Obj<'lean>; N]> {
    require_ctor_shape(&obj, expected_tag, N, label)?;
    let runtime = obj.runtime();
    let ptr = obj.as_raw_borrowed();
    // SAFETY: shape validated above; `lean_ctor_obj_cptr` returns a
    // pointer valid for `N` slots, each holding a live owned
    // `*mut lean_object` (well-formed Lean ctor invariant).
    let slots = unsafe { lean_ctor_obj_cptr(ptr) };
    let mut out: [MaybeUninit<Obj<'lean>>; N] = [const { MaybeUninit::uninit() }; N];
    for (i, slot) in out.iter_mut().enumerate() {
        // SAFETY: index in `0..N` is in-bounds per the shape check; the
        // slot read is a borrowed view, then `lean_inc` bumps the refcount
        // so the wrapped `Obj` owns its own count independent of the
        // parent.
        unsafe {
            let field_ptr = *slots.add(i);
            lean_inc(field_ptr);
            slot.write(Obj::from_owned_raw(runtime, field_ptr));
        }
    }
    // The parent `obj` falls out of scope here; its `Drop` releases the
    // original constructor (and the per-field counts the parent held).
    drop(obj);
    // SAFETY: every element of `out` was initialised in the loop above
    // (`0..N` covers the whole array exactly once); transmute is sound
    // because `[MaybeUninit<T>; N]` and `[T; N]` share layout.
    Ok(out.map(|cell| unsafe { cell.assume_init() }))
}

/// Read the tag byte of a constructor object.
///
/// Used by sum-type decoders (`Option` and `except::Except` (sibling-module sum-type carriers))
/// that need to pick a variant before they know the arity. Borrow-only:
/// leaves the refcount untouched.
///
/// # Errors
///
/// Returns [`HostStage::Conversion`](crate::error::HostStage::Conversion)
/// if `obj` is not a heap-allocated constructor.
pub fn ctor_tag(obj: &Obj<'_>) -> LeanResult<u8> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` is pure pointer-bit math.
    if unsafe { lean_is_scalar(ptr) } {
        return Err(conversion_error(
            "expected Lean constructor, found scalar-tagged object",
        ));
    }
    // SAFETY: non-scalar; tag read on the owned object header.
    if !unsafe { lean_is_ctor(ptr) } {
        // SAFETY: same branch.
        let found_tag = unsafe { lean_obj_tag(ptr) };
        return Err(conversion_error(format!(
            "expected Lean constructor, found object with tag {found_tag}"
        )));
    }
    // SAFETY: ctor object — its tag fits a `u8` per `lean.h`'s
    // `LEAN_MAX_CTOR_TAG` ceiling.
    let tag = unsafe { lean_obj_tag(ptr) };
    // Logical ctor tags are bounded by `LEAN_MAX_CTOR_TAG` (=243), so the
    // cast is lossless inside the gate.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "ctor tag is bounded by LEAN_MAX_CTOR_TAG"
    )]
    Ok(tag as u8)
}

/// Shared validator for [`take_ctor_objects`]: ctor kind, matching tag,
/// matching `num_objs`.
fn require_ctor_shape(obj: &Obj<'_>, expected_tag: u8, expected_num_objs: usize, label: &str) -> LeanResult<()> {
    let found_tag = ctor_tag(obj)?;
    if found_tag != expected_tag {
        return Err(conversion_error(format!(
            "expected Lean {label} ctor (tag {expected_tag}), found tag {found_tag}"
        )));
    }
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `ctor_tag` already validated `obj` is a constructor; the
    // `m_other` field then holds its num_objs count.
    let found_num_objs = unsafe { lean_ctor_num_objs(ptr) } as usize;
    if found_num_objs != expected_num_objs {
        return Err(conversion_error(format!(
            "expected Lean {label} ctor with {expected_num_objs} object field(s), found {found_num_objs}"
        )));
    }
    Ok(())
}
