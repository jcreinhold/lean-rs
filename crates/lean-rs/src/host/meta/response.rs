//! `LeanMetaResponse<Resp>` and `MetaCallStatus` — typed outcome of a
//! bounded `MetaM` service call.
//!
//! The Lean side encodes `MetaResponse α` as a four-constructor
//! inductive (`ok / failed / timeoutOrHeartbeat / unsupported`); each
//! constructor carries a single object payload — the typed response on
//! `Ok`, a [`LeanElabFailure`] on every other branch. Tag indices are
//! 0..=3 in declaration order; the [`TryFromLean`] impl below does the
//! dispatch.
//!
//! `LeanMetaResponse<Resp>` is the value type [`crate::LeanSession::run_meta`]
//! returns; callers can both branch on the typed tag via [`Self::status`]
//! and read the typed payload on the `Ok` branch (or the structured
//! diagnostics on the other three).

use crate::abi::structure::{ctor_tag, take_ctor_objects};
use crate::abi::traits::{TryFromLean, conversion_error};
use crate::error::LeanResult;
use crate::host::elaboration::LeanElabFailure;
use crate::runtime::obj::Obj;

/// Classification tag for a meta-service call.
///
/// Returned by [`LeanMetaResponse::status`] without inspecting the
/// payload. `#[non_exhaustive]` so future capability refinements can
/// extend the taxonomy without breaking exhaustive matches downstream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MetaCallStatus {
    /// The `MetaM` action returned a typed payload.
    Ok,
    /// The `MetaM` action raised a non-resource-exhaustion exception
    /// (type error, unbound metavariable, …).
    Failed,
    /// The heartbeat ceiling tripped before the action finished.
    /// Equivalent to `Lean.Exception.isMaxHeartbeat` matching on the
    /// caught exception.
    TimeoutOrHeartbeat,
    /// The capability does not expose this service — either the Lean
    /// shim returned `unsupported` for the request shape, or the
    /// loaded capability does not export the service's C symbol.
    Unsupported,
}

/// Outcome of a bounded `MetaM` service call.
///
/// Carries either a typed `Resp` payload (on `Ok`) or a
/// [`LeanElabFailure`] (on every other status) so callers can both
/// branch on the typed status tag via [`Self::status`] and read the
/// structured diagnostics in the failure cases.
///
/// `#[non_exhaustive]` so the variant set tracks future toolchain
/// classification refinements without breaking exhaustive matches.
#[derive(Debug)]
#[non_exhaustive]
pub enum LeanMetaResponse<Resp> {
    /// The `MetaM` action returned a typed payload.
    Ok(Resp),
    /// The `MetaM` action raised a non-resource-exhaustion exception.
    /// The failure carries one error-severity diagnostic.
    Failed(LeanElabFailure),
    /// The heartbeat ceiling tripped before the action finished. The
    /// failure carries the heartbeat-exhaustion message Lean produced.
    TimeoutOrHeartbeat(LeanElabFailure),
    /// The capability did not provide this service. Either the Lean
    /// shim returned `unsupported` or the dispatcher synthesised this
    /// branch when the service's symbol was absent at capability load.
    Unsupported(LeanElabFailure),
}

impl<Resp> LeanMetaResponse<Resp> {
    /// Project the variant tag without inspecting the payload.
    #[must_use]
    pub fn status(&self) -> MetaCallStatus {
        match self {
            Self::Ok(_) => MetaCallStatus::Ok,
            Self::Failed(_) => MetaCallStatus::Failed,
            Self::TimeoutOrHeartbeat(_) => MetaCallStatus::TimeoutOrHeartbeat,
            Self::Unsupported(_) => MetaCallStatus::Unsupported,
        }
    }
}

impl<'lean, Resp> TryFromLean<'lean> for LeanMetaResponse<Resp>
where
    Resp: TryFromLean<'lean>,
{
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let tag = ctor_tag(&obj)?;
        match tag {
            0 => {
                let [payload] = take_ctor_objects::<1>(obj, 0, "MetaResponse.ok")?;
                Ok(Self::Ok(Resp::try_from_lean(payload)?))
            }
            1 => {
                let [payload] = take_ctor_objects::<1>(obj, 1, "MetaResponse.failed")?;
                Ok(Self::Failed(LeanElabFailure::try_from_lean(payload)?))
            }
            2 => {
                let [payload] = take_ctor_objects::<1>(obj, 2, "MetaResponse.timeoutOrHeartbeat")?;
                Ok(Self::TimeoutOrHeartbeat(LeanElabFailure::try_from_lean(payload)?))
            }
            3 => {
                let [payload] = take_ctor_objects::<1>(obj, 3, "MetaResponse.unsupported")?;
                Ok(Self::Unsupported(LeanElabFailure::try_from_lean(payload)?))
            }
            other => Err(conversion_error(format!(
                "expected Lean MetaResponse ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}
