//! [`EvidenceStatus`] tag enum and the [`LeanKernelOutcome`] sum type
//! returned by [`crate::LeanSession::kernel_check`].
//!
//! The Lean side encodes the outcome as a four-constructor inductive
//! (`checked | rejected | unavailable | unsupported`); each constructor
//! carries one object payload — the [`crate::LeanEvidence`] handle for
//! `Checked`, a [`LeanElabFailure`] for the other three. The constructor
//! tags are 0..=3 in declaration order; the [`TryFromLean`] impl below
//! does the dispatch.

use lean_rs::Obj;
use lean_rs::abi::structure::{take_ctor_objects, view};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
use lean_rs::error::LeanResult;

use crate::host::elaboration::LeanElabFailure;
use crate::host::evidence::handle::LeanEvidence;

/// What the kernel-check capability concluded about a piece of Lean
/// source.
///
/// The tag accessor on [`LeanKernelOutcome::status`] returns values of
/// this type, as does [`crate::LeanSession::check_evidence`] when
/// re-validating a captured [`crate::LeanEvidence`] handle.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EvidenceStatus {
    /// The kernel accepted the declaration. A [`crate::LeanEvidence`]
    /// handle is available on the [`LeanKernelOutcome::Checked`] branch.
    Checked,
    /// The declaration was well-formed enough to reach kernel checking,
    /// and the kernel (or the elaborator's type-check pass) refused it.
    Rejected,
    /// The capability ran but could not produce evidence — typically a
    /// parse failure that aborted before the kernel could see the
    /// declaration.
    Unavailable,
    /// The source did not name a kind of declaration this capability
    /// produces evidence for (for example a `#check` command, a
    /// non-theorem definition, or a comment-only fragment).
    Unsupported,
}

/// Outcome of [`crate::LeanSession::kernel_check`].
///
/// Carries either a [`crate::LeanEvidence`] handle (on `Checked`) or a
/// [`LeanElabFailure`] (on every other status) so callers can both
/// branch on the typed status tag via [`Self::status`] and read the
/// structured diagnostics in the failure cases.
#[derive(Debug)]
pub enum LeanKernelOutcome<'lean> {
    /// The kernel accepted the declaration.
    Checked(LeanEvidence<'lean>),
    /// The declaration reached kernel checking and was refused. The
    /// failure carries the diagnostics the elaborator and kernel
    /// produced.
    Rejected(LeanElabFailure),
    /// The capability could not produce evidence (typically a parse
    /// failure). The failure carries the diagnostics Lean produced
    /// before aborting.
    Unavailable(LeanElabFailure),
    /// The source did not name a supported kind of declaration. The
    /// failure carries any diagnostics Lean produced while classifying.
    Unsupported(LeanElabFailure),
}

impl LeanKernelOutcome<'_> {
    /// Project the variant tag without consuming the payload.
    ///
    /// Useful when the caller only needs to branch on `Checked` vs. one
    /// of the failure tags before deciding whether to read the contained
    /// [`LeanElabFailure`] diagnostics.
    #[must_use]
    pub fn status(&self) -> EvidenceStatus {
        match self {
            Self::Checked(_) => EvidenceStatus::Checked,
            Self::Rejected(_) => EvidenceStatus::Rejected,
            Self::Unavailable(_) => EvidenceStatus::Unavailable,
            Self::Unsupported(_) => EvidenceStatus::Unsupported,
        }
    }
}

impl<'lean> TryFromLean<'lean> for EvidenceStatus {
    /// Decode a nullary-only Lean `EvidenceStatus` inductive.
    ///
    /// Tag order is `Checked = 0`, `Rejected = 1`, `Unavailable = 2`,
    /// `Unsupported = 3`, matching the declaration order in
    /// `fixtures/lean/LeanRsFixture/Elaboration.lean`'s `EvidenceStatus`
    /// inductive.
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let obj_view = view(&obj);
        let tag = if obj_view.is_scalar() {
            obj_view.scalar_payload("EvidenceStatus")?
        } else {
            let heap_tag = obj_view.ctor()?.tag();
            let _ = take_ctor_objects::<0>(obj, heap_tag, "EvidenceStatus")?;
            usize::from(heap_tag)
        };
        match tag {
            0 => Ok(Self::Checked),
            1 => Ok(Self::Rejected),
            2 => Ok(Self::Unavailable),
            3 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean EvidenceStatus tag 0..=3, found {other}"
            ))),
        }
    }
}

impl<'lean> TryFromLean<'lean> for LeanKernelOutcome<'lean> {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let tag = view(&obj).ctor()?.tag();
        match tag {
            0 => {
                let [payload] = take_ctor_objects::<1>(obj, 0, "KernelOutcome.checked")?;
                Ok(Self::Checked(LeanEvidence::try_from_lean(payload)?))
            }
            1 => {
                let [payload] = take_ctor_objects::<1>(obj, 1, "KernelOutcome.rejected")?;
                Ok(Self::Rejected(LeanElabFailure::try_from_lean(payload)?))
            }
            2 => {
                let [payload] = take_ctor_objects::<1>(obj, 2, "KernelOutcome.unavailable")?;
                Ok(Self::Unavailable(LeanElabFailure::try_from_lean(payload)?))
            }
            3 => {
                let [payload] = take_ctor_objects::<1>(obj, 3, "KernelOutcome.unsupported")?;
                Ok(Self::Unsupported(LeanElabFailure::try_from_lean(payload)?))
            }
            other => Err(conversion_error(format!(
                "expected Lean KernelOutcome ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}
