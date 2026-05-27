//! Opaque [`lean_object`] plus the calling-convention typedefs from
//! `lean.h:131–168`.

use core::marker::{PhantomData, PhantomPinned};

/// Opaque handle to a Lean 4 heap object.
///
/// `lean_object` is **only ever held by pointer**. The published type is
/// zero-sized + `!Send + !Sync + !Unpin`, so downstream code cannot read or
/// write the underlying header (`m_rc`, `m_tag`, …) directly. Reach object
/// state through this crate's `pub unsafe fn` helpers.
///
/// The C struct in `lean.h:131–136` is `{ int m_rc; unsigned m_cs_sz:16;
/// unsigned m_other:8; unsigned m_tag:8; }` for 8 total bytes. A crate-
/// private `LeanObjectRepr` mirrors that layout; the digest pinned at
/// build time guarantees the C and Rust pictures match.
#[repr(C)]
pub struct lean_object {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// "Standard" object argument—caller transfers ownership of one refcount.
pub type lean_obj_arg = *mut lean_object;

/// "Borrowed" object argument—caller retains the refcount.
pub type b_lean_obj_arg = *mut lean_object;

/// "Unique" object argument—caller asserts the object is non-shared.
pub type u_lean_obj_arg = *mut lean_object;

/// "Standard" object result—callee returns a fresh owned refcount.
pub type lean_obj_res = *mut lean_object;

/// "Borrowed" object result—refcount belongs to a longer-lived owner.
pub type b_lean_obj_res = *mut lean_object;
