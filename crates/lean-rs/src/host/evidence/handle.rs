//! Opaque handle for kernel-checked Lean evidence.

use core::fmt;

use lean_rs_sys::lean_object;

use crate::abi::traits::{LeanAbi, TryFromLean, sealed};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Opaque receipt for a piece of Lean evidence the kernel accepted.
///
/// Constructed only on the
/// [`crate::host::evidence::LeanKernelOutcome::Checked`] branch returned
/// by [`crate::LeanSession::kernel_check`]. The Rust handle wraps the
/// Lean-side `Evidence` value (currently a one-field structure carrying
/// a `Lean.Declaration`) opaquely — Rust does not inspect the contained
/// proof term or declaration. Prompt 17 layers `ProofSummary` and the
/// `check_evidence` re-validation method on top; for prompt 15 the
/// handle is intentionally accessor-free.
///
/// [`Clone`] bumps the underlying refcount; [`Drop`] releases it.
/// Neither [`Send`] nor [`Sync`]: inherited from the crate-internal
/// owned-object handle.
pub struct LeanEvidence<'lean> {
    obj: Obj<'lean>,
}

impl Clone for LeanEvidence<'_> {
    fn clone(&self) -> Self {
        Self { obj: self.obj.clone() }
    }
}

impl fmt::Debug for LeanEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Opaque on purpose: the Rust side has no claim on Lean's
        // structural identity. Route through a Lean-authored
        // pretty-printer export when a diagnostic string is needed.
        f.debug_struct("LeanEvidence").finish_non_exhaustive()
    }
}

impl sealed::SealedAbi for LeanEvidence<'_> {}

impl<'lean> LeanAbi<'lean> for LeanEvidence<'lean> {
    type CRepr = *mut lean_object;

    fn into_c(self, _runtime: &'lean LeanRuntime) -> *mut lean_object {
        self.obj.into_raw()
    }

    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
    )]
    fn from_c(c: *mut lean_object, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` is an owned `lean_object*` produced by the Lean
        // kernel-check capability returning an `Evidence` value; per
        // Lake's `lean_obj_res` contract it carries one reference count.
        // `runtime` witnesses `'lean`.
        #[allow(unsafe_code)]
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Ok(Self { obj })
    }
}

impl<'lean> TryFromLean<'lean> for LeanEvidence<'lean> {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        Ok(Self { obj })
    }
}
