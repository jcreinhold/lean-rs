//! Opaque handle for `Lean.Declaration`.
//!
//! [`LeanDeclaration`] is a receipt for an owned Lean declaration value
//! (axiom, definition, theorem, opaque, inductive, …) produced on the
//! Lean side. The Rust API is intentionally minimal: it carries the
//! handle through the FFI boundary and nothing else. Construction and
//! inspection — selecting the constructor, reading the declaration name
//! or type, rendering a summary — live in Lean exports the caller
//! reaches through [`crate::module::LeanModule::exported_unchecked`]. Rust offers
//! no constructor: building a `Declaration` outside Lean would either be
//! wrong (lacking universe and type machinery) or duplicate Lean's
//! responsibility for environment extension.
//!
//! Display text obtained from a Lean export is diagnostic, not a
//! semantic key (see the module docs on [`crate::handle`]).

use core::fmt;

use lean_rs_sys::lean_object;

use crate::abi::traits::{LeanAbi, TryFromLean, sealed};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Opaque, lifetime-bound handle to a `Lean.Declaration`.
///
/// `'lean` cascades from the [`crate::LeanRuntime`] borrow that produced
/// the value, so a handle cannot outlive the runtime. Neither [`Send`]
/// nor [`Sync`]: inherited from the crate-internal owned-object handle
/// the type wraps.
///
/// [`Clone`] bumps the underlying refcount; [`Drop`] releases it. There
/// are no public inherent methods: the handle is a pass-through that
/// reaches Lean-authored operations through
/// [`crate::module::LeanModule::exported_unchecked`].
pub struct LeanDeclaration<'lean> {
    obj: Obj<'lean>,
}

impl Clone for LeanDeclaration<'_> {
    fn clone(&self) -> Self {
        Self { obj: self.obj.clone() }
    }
}

impl fmt::Debug for LeanDeclaration<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Opaque on purpose: the Rust side has no claim on the
        // declaration's contents. For a diagnostic string, route
        // through a Lean-authored summariser export.
        f.debug_struct("LeanDeclaration").finish_non_exhaustive()
    }
}

impl sealed::SealedAbi for LeanDeclaration<'_> {}

impl<'lean> LeanAbi<'lean> for LeanDeclaration<'lean> {
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
        // export returning `Lean.Declaration`; per Lake's `lean_obj_res`
        // contract it carries one reference count. `runtime` witnesses
        // `'lean`.
        #[allow(unsafe_code)]
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Ok(Self { obj })
    }
}

impl<'lean> TryFromLean<'lean> for LeanDeclaration<'lean> {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        Ok(Self { obj })
    }
}
