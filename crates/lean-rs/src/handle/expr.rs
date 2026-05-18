//! Opaque handle for `Lean.Expr`.
//!
//! [`LeanExpr`] is a receipt for an owned Lean expression value produced
//! on the Lean side. The Rust API is intentionally minimal: it carries
//! the handle through the FFI boundary and nothing else. Construction
//! and inspection — `.bvar`, `.const`, `.app`, `toString`, `==` — live
//! in Lean exports the caller reaches through
//! [`crate::module::LeanModule::exported`]. Rust deliberately offers no
//! constructor for `Expr`: rebuilding the Lean term algebra outside the
//! kernel would either be wrong (lacking elaboration / type checking)
//! or duplicate Lean's responsibility.
//!
//! Display text obtained from a Lean export is diagnostic, not a
//! semantic key; use a Lean-authored equality export when semantics
//! matter (see the module docs on [`crate::handle`]).

use core::fmt;

use lean_rs_sys::lean_object;

use crate::abi::traits::{IntoLean, LeanAbi, TryFromLean, sealed};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Opaque, lifetime-bound handle to a `Lean.Expr`.
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
pub struct LeanExpr<'lean> {
    obj: Obj<'lean>,
}

impl Clone for LeanExpr<'_> {
    fn clone(&self) -> Self {
        Self { obj: self.obj.clone() }
    }
}

impl fmt::Debug for LeanExpr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Opaque on purpose: the Rust side has no claim on Lean's
        // structural identity. For a diagnostic string, route through a
        // Lean-authored `Expr` pretty-printer export.
        f.debug_struct("LeanExpr").finish_non_exhaustive()
    }
}

impl sealed::SealedAbi for LeanExpr<'_> {}

impl<'lean> LeanAbi<'lean> for LeanExpr<'lean> {
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
        // export returning `Lean.Expr`; per Lake's `lean_obj_res`
        // contract it carries one reference count. `runtime` witnesses
        // `'lean`.
        #[allow(unsafe_code)]
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Ok(Self { obj })
    }
}

impl<'lean> TryFromLean<'lean> for LeanExpr<'lean> {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        Ok(Self { obj })
    }
}

impl<'lean> IntoLean<'lean> for LeanExpr<'lean> {
    fn into_lean(self, _runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // The handle already wraps a fully-owned Lean object reference;
        // hand it off unchanged. Required so `Option<LeanExpr<'lean>>`
        // can satisfy [`LeanAbi`] (which is itself bounded on
        // `IntoLean + TryFromLean`) at the `LeanSession::elaborate`
        // call site that takes an optional expected type.
        self.obj
    }
}
