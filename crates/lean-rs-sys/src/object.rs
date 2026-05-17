//! Object inspection and allocation helpers — mirrors `lean.h:312–630`.
//!
//! Covers scalar-pointer encoding ([`lean_is_scalar`]), tag reads, the
//! `lean_is_*` predicates, the `lean_to_*` casts (returned as raw `*mut
//! lean_object` for opacity), and the runtime-mode reads ([`lean_is_st`],
//! [`lean_is_mt`], [`lean_is_persistent`], [`lean_is_exclusive`],
//! [`lean_is_shared`]). Allocation primitives exported by `libleanshared`
//! (`lean_alloc_object`, `lean_free_object`) are declared here; the
//! higher-level inline allocators (`lean_alloc_ctor`, `lean_alloc_closure`,
//! `lean_alloc_array`, …) live in their category modules.

#![allow(clippy::inline_always)]

use core::sync::atomic::{AtomicI32, Ordering};

use crate::consts::{
    LEAN_ARRAY, LEAN_CLOSURE, LEAN_EXTERNAL, LEAN_MAX_CTOR_TAG, LEAN_MPZ, LEAN_PROMISE, LEAN_REF, LEAN_SCALAR_ARRAY,
    LEAN_STRING, LEAN_TASK, LEAN_THUNK,
};
use crate::repr::LeanObjectRepr;
use crate::types::lean_object;

unsafe extern "C" {
    /// Allocate an uninitialized Lean heap object of size `sz` bytes
    /// (`lean.h:490`). The caller is responsible for initializing the
    /// header via `lean_set_st_header`-equivalent writes, which only this
    /// crate's helpers should perform.
    pub fn lean_alloc_object(sz: usize) -> *mut lean_object;

    /// Free a Lean heap object previously allocated with
    /// `lean_alloc_object` (`lean.h:491`).
    pub fn lean_free_object(o: *mut lean_object);

    /// Total byte size of `o`'s allocation (`lean.h:506`).
    pub fn lean_object_byte_size(o: *mut lean_object) -> usize;

    /// Byte size of `o`'s salient (initialized) storage
    /// (`lean.h:513`).
    pub fn lean_object_data_byte_size(o: *mut lean_object) -> usize;
}

/// Scalar-pointer test (`lean.h:312`). Scalar values are tagged with the
/// low bit set; the pointer never aliases a real allocation.
///
/// # Safety
///
/// No precondition: this only inspects the pointer bits. It is safe to call
/// on any value that the runtime might hand us, including null and
/// uninitialized values.
#[inline(always)]
pub unsafe fn lean_is_scalar(o: *mut lean_object) -> bool {
    (o as usize) & 1 == 1
}

/// Box an unsigned integer into a scalar Lean object (`lean.h:313`).
///
/// # Safety
///
/// Pointer-bit arithmetic only; no memory access. The caller is responsible
/// for ensuring `n` fits inside `usize >> 1` if the value is intended to be
/// recoverable via [`lean_unbox`].
#[inline(always)]
pub unsafe fn lean_box(n: usize) -> *mut lean_object {
    ((n << 1) | 1) as *mut lean_object
}

/// Unbox a scalar Lean object (`lean.h:314`).
///
/// # Safety
///
/// `o` must be scalar-tagged (low bit set). Otherwise the returned `usize`
/// is the raw pointer right-shifted by one and meaningless.
#[inline(always)]
pub unsafe fn lean_unbox(o: *mut lean_object) -> usize {
    (o as usize) >> 1
}

#[inline(always)]
unsafe fn header<'a>(o: *mut lean_object) -> &'a LeanObjectRepr {
    // SAFETY: caller guarantees `o` is a valid non-scalar heap pointer;
    // layout pinned by build digest.
    unsafe { &*o.cast::<LeanObjectRepr>() }
}

#[inline(always)]
unsafe fn load_rc(o: *mut lean_object) -> i32 {
    // SAFETY: caller guarantees `o` is a valid non-scalar heap pointer.
    // We materialize a safe `&AtomicI32` for the `Relaxed` load even on the
    // single-threaded path so reads stay consistent with concurrent
    // mutations from `lean_inc*` / `lean_dec*` on MT objects.
    unsafe {
        let repr = o.cast::<LeanObjectRepr>();
        AtomicI32::from_ptr(&raw mut (*repr).m_rc).load(Ordering::Relaxed)
    }
}

/// Read the object's tag byte (`lean.h:493–495`).
///
/// # Safety
///
/// `o` must be a valid non-scalar heap object pointer.
#[inline(always)]
pub unsafe fn lean_ptr_tag(o: *mut lean_object) -> u8 {
    // SAFETY: precondition above.
    unsafe { header(o).m_tag }
}

/// Read the object's `m_other` byte (`lean.h:497–499`).
///
/// # Safety
///
/// Same as [`lean_ptr_tag`].
#[inline(always)]
pub unsafe fn lean_ptr_other(o: *mut lean_object) -> u8 {
    // SAFETY: precondition above.
    unsafe { header(o).m_other }
}

macro_rules! is_tag {
    ($name:ident, $tag:expr, $doc:expr) => {
        #[doc = $doc]
        ///
        /// # Safety
        ///
        /// `o` must be a valid non-scalar Lean heap object pointer.
        #[inline(always)]
        pub unsafe fn $name(o: *mut lean_object) -> bool {
            // SAFETY: precondition above.
            unsafe { lean_ptr_tag(o) == $tag }
        }
    };
}

/// Constructor objects share tags `0..=LEAN_MAX_CTOR_TAG`; this is the
/// `lean_is_ctor` predicate from `lean.h:565`.
///
/// # Safety
///
/// `o` must be a valid non-scalar Lean heap object pointer.
#[inline(always)]
pub unsafe fn lean_is_ctor(o: *mut lean_object) -> bool {
    // SAFETY: precondition above.
    unsafe { lean_ptr_tag(o) <= LEAN_MAX_CTOR_TAG }
}

is_tag!(
    lean_is_closure,
    LEAN_CLOSURE,
    "True if `o` is a closure object (`lean.h:566`)."
);
is_tag!(
    lean_is_array,
    LEAN_ARRAY,
    "True if `o` is an object array (`lean.h:567`)."
);
is_tag!(
    lean_is_sarray,
    LEAN_SCALAR_ARRAY,
    "True if `o` is a scalar array (`lean.h:568`)."
);
is_tag!(lean_is_string, LEAN_STRING, "True if `o` is a string (`lean.h:569`).");
is_tag!(
    lean_is_mpz,
    LEAN_MPZ,
    "True if `o` is an MPZ (big integer) object (`lean.h:570`)."
);
is_tag!(
    lean_is_thunk,
    LEAN_THUNK,
    "True if `o` is a thunk object (`lean.h:571`)."
);
is_tag!(lean_is_task, LEAN_TASK, "True if `o` is a task object (`lean.h:572`).");
is_tag!(
    lean_is_promise,
    LEAN_PROMISE,
    "True if `o` is a promise object (`lean.h:573`)."
);
is_tag!(
    lean_is_external,
    LEAN_EXTERNAL,
    "True if `o` is an external object (`lean.h:574`)."
);
is_tag!(
    lean_is_ref,
    LEAN_REF,
    "True if `o` is a Lean `Ref` object (`lean.h:575`)."
);

/// Read the object's "logical" tag (`lean.h:577–579`): for scalar values
/// this is the unboxed payload; otherwise it is the heap tag.
///
/// # Safety
///
/// `o` must be either a scalar-tagged pointer or a valid non-scalar Lean
/// heap object pointer.
#[inline(always)]
pub unsafe fn lean_obj_tag(o: *mut lean_object) -> u32 {
    // SAFETY: scalar check first; heap branch inherits lean_ptr_tag's contract.
    unsafe {
        if lean_is_scalar(o) {
            lean_unbox(o) as u32
        } else {
            u32::from(lean_ptr_tag(o))
        }
    }
}

/// Single-threaded test (`lean.h:519–521`).
///
/// # Safety
///
/// `o` must be a valid non-scalar Lean heap object pointer.
#[inline(always)]
pub unsafe fn lean_is_st(o: *mut lean_object) -> bool {
    // SAFETY: precondition above.
    unsafe { load_rc(o) > 0 }
}

/// Multi-threaded test (`lean.h:515–517`).
///
/// # Safety
///
/// Same as [`lean_is_st`].
#[inline(always)]
pub unsafe fn lean_is_mt(o: *mut lean_object) -> bool {
    // SAFETY: precondition above.
    unsafe { load_rc(o) < 0 }
}

/// Persistent test (`lean.h:524–526`).
///
/// # Safety
///
/// Same as [`lean_is_st`].
#[inline(always)]
pub unsafe fn lean_is_persistent(o: *mut lean_object) -> bool {
    // SAFETY: precondition above.
    unsafe { load_rc(o) == 0 }
}

/// True if `o` is single-threaded with refcount exactly one
/// (`lean.h:592–598`).
///
/// # Safety
///
/// Same as [`lean_is_st`].
#[inline(always)]
pub unsafe fn lean_is_exclusive(o: *mut lean_object) -> bool {
    // SAFETY: precondition above.
    unsafe { load_rc(o) == 1 }
}

/// True if `o` is single-threaded with refcount > 1 (`lean.h:604–610`).
///
/// # Safety
///
/// Same as [`lean_is_st`].
#[inline(always)]
pub unsafe fn lean_is_shared(o: *mut lean_object) -> bool {
    // SAFETY: precondition above.
    unsafe { load_rc(o) > 1 }
}
