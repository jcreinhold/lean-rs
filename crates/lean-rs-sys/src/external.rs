//! External objects—externs and inline accessors from `lean.h:295–1332`.

#![allow(clippy::inline_always)]

use core::ffi::c_void;

use crate::consts::LEAN_EXTERNAL;
use crate::repr::LeanExternalObjectRepr;
use crate::types::{lean_obj_res, lean_object};

/// Finalizer signature stored in a `lean_external_class` (`lean.h:295`).
pub type lean_external_finalize_proc = unsafe extern "C" fn(*mut c_void);

/// Foreach signature stored in a `lean_external_class` (`lean.h:296`).
pub type lean_external_foreach_proc = unsafe extern "C" fn(*mut c_void, *mut lean_object);

unsafe extern "C" {
    /// Register an external-object class. The returned pointer outlives the
    /// process; the runtime does not free registered classes
    /// (`lean.h:303`).
    pub fn lean_register_external_class(
        finalize: lean_external_finalize_proc,
        foreach: lean_external_foreach_proc,
    ) -> *mut c_void;
}

/// Allocate an external-data wrapper (`lean.h:1307–1313`).
///
/// # Safety
///
/// `cls` must come from [`lean_register_external_class`] and remain valid
/// for the lifetime of the returned object; `data` will be passed verbatim
/// to the class's finalize and foreach callbacks.
#[inline(always)]
pub unsafe fn lean_alloc_external(cls: *mut c_void, data: *mut c_void) -> lean_obj_res {
    // SAFETY: `lean_alloc_object` returns a fresh small-object allocation
    // sized for `LeanExternalObjectRepr`; we initialize the header, class,
    // and data fields before returning.
    unsafe {
        let raw = crate::object::lean_alloc_object(core::mem::size_of::<LeanExternalObjectRepr>());
        let repr = raw.cast::<LeanExternalObjectRepr>();
        (*repr).header.m_rc = 1;
        (*repr).header.m_tag = LEAN_EXTERNAL;
        (*repr).header.m_other = 0;
        (*repr).header.m_cs_sz = 0;
        (*repr).class = cls;
        (*repr).data = data;
        raw
    }
}

/// Read the external object's class pointer (`lean.h:1315–1317`).
///
/// # Safety
///
/// `o` must be a borrowed Lean external object.
#[inline(always)]
pub unsafe fn lean_get_external_class(o: *mut lean_object) -> *mut c_void {
    // SAFETY: precondition above.
    unsafe { (*o.cast::<LeanExternalObjectRepr>()).class }
}

/// Read the external object's data pointer (`lean.h:1319–1321`).
///
/// # Safety
///
/// Same as [`lean_get_external_class`]. The interpretation of the pointer
/// is up to the class's finalize / foreach implementation.
#[inline(always)]
pub unsafe fn lean_get_external_data(o: *mut lean_object) -> *mut c_void {
    // SAFETY: precondition above.
    unsafe { (*o.cast::<LeanExternalObjectRepr>()).data }
}
