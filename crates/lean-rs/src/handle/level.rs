//! Opaque handle for `Lean.Level`.
//!
//! [`LeanLevel`] is a receipt for an owned universe-level value produced
//! on the Lean side. The Rust API is intentionally minimal: it carries
//! the handle through the FFI boundary and nothing else. Construction
//! and inspection — `.zero`, `.succ`, `.max`, `toString`, `==` — live in
//! Lean exports the caller reaches through
//! [`crate::module::LeanModule::exported`].
//!
//! Display text obtained from a Lean export is diagnostic, not a
//! semantic key; use a Lean-authored equality export when semantics
//! matter (see the module docs on [`crate::handle`]).

use core::fmt;

use lean_rs_sys::lean_object;

use crate::abi::traits::{LeanAbi, TryFromLean, sealed};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Opaque, lifetime-bound handle to a `Lean.Level`.
///
/// `'lean` cascades from the [`crate::LeanRuntime`] borrow that produced
/// the value, so a handle cannot outlive the runtime. Neither [`Send`]
/// nor [`Sync`]: inherited from the crate-internal owned-object handle
/// the type wraps.
///
/// [`Clone`] bumps the underlying refcount; [`Drop`] releases it. There
/// are no public inherent methods: the handle is a pass-through that
/// reaches Lean-authored operations through
/// [`crate::module::LeanModule::exported`].
pub struct LeanLevel<'lean> {
    obj: Obj<'lean>,
}

impl Clone for LeanLevel<'_> {
    fn clone(&self) -> Self {
        Self { obj: self.obj.clone() }
    }
}

impl fmt::Debug for LeanLevel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Opaque on purpose: the Rust side has no claim on Lean's
        // structural identity. For a diagnostic string, route through a
        // Lean-authored `Level` pretty-printer export.
        f.debug_struct("LeanLevel").finish_non_exhaustive()
    }
}

impl sealed::SealedAbi for LeanLevel<'_> {}

impl<'lean> LeanAbi<'lean> for LeanLevel<'lean> {
    type CRepr = *mut lean_object;

    fn into_c(self, _runtime: &'lean LeanRuntime) -> *mut lean_object {
        self.obj.into_raw()
    }

    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
    )]
    fn from_c(c: *mut lean_object, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` is an owned `lean_object*` produced by a Lean
        // export returning `Lean.Level`; per Lake's `lean_obj_res`
        // contract it carries one reference count. `runtime` witnesses
        // `'lean`.
        #[allow(unsafe_code)]
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Ok(Self { obj })
    }
}

impl<'lean> TryFromLean<'lean> for LeanLevel<'lean> {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        Ok(Self { obj })
    }
}
