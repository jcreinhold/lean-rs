//! Opaque handle for kernel-checked Lean evidence.

use core::fmt;

use lean_rs::LeanRuntime;
use lean_rs::Obj;
use lean_rs::abi::traits::{LeanAbi, TryFromLean, sealed};
use lean_rs::error::LeanResult;

/// Opaque receipt for a piece of Lean evidence the kernel accepted.
///
/// Constructed only on the
/// [`crate::host::evidence::LeanKernelOutcome::Checked`] branch returned
/// by [`crate::LeanSession::kernel_check`]. The Rust handle wraps the
/// Lean-side `Evidence` value (currently a one-field structure carrying
/// a `Lean.Declaration`) opaquely — Rust does not inspect the contained
/// proof term or declaration. The two operations callers do with a
/// handle are [`crate::LeanSession::check_evidence`] (re-run the kernel
/// against it) and [`crate::LeanSession::summarize_evidence`] (project
/// it to a bounded [`crate::host::evidence::ProofSummary`]).
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
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, _runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.obj.into_raw()
    }

    fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        evidence_from_c(c, runtime)
    }
}

fn evidence_from_c<'lean>(
    c: <Obj<'lean> as LeanAbi<'lean>>::CRepr,
    runtime: &'lean LeanRuntime,
) -> LeanResult<LeanEvidence<'lean>> {
    Obj::from_c(c, runtime).map(|obj| LeanEvidence { obj })
}

impl<'lean> TryFromLean<'lean> for LeanEvidence<'lean> {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        Ok(Self { obj })
    }
}
