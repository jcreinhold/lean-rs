//! Bounded option bundle for [`crate::LeanSession::run_meta`].
//!
//! Mirrors [`crate::host::elaboration::LeanElabOptions`] in spirit: every
//! setter saturates rather than rejecting out-of-range values, the bounds
//! exist as safety rails the call site never has to write `.map_err`
//! around. Parallel rather than shared because meta-services carry no
//! source position (no `file_label`) and do carry a reducibility setting
//! ([`LeanMetaTransparency`]) that has no analogue in elaboration.
//!
//! The heartbeat and diagnostic-byte ceilings reuse the existing
//! `LEAN_HEARTBEAT_LIMIT_*` / `LEAN_DIAGNOSTIC_BYTE_LIMIT_*` constants
//! from [`crate::host::elaboration`] — the underlying Lean machinery
//! (`Lean.maxHeartbeats`) and the failure-bytes invariant are the same.

use crate::error::bound_message;
use crate::host::elaboration::{
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX, LEAN_HEARTBEAT_LIMIT_DEFAULT,
    LEAN_HEARTBEAT_LIMIT_MAX,
};

/// Reducibility setting threaded into the bounded `MetaM` runner.
///
/// Maps 1-1 onto Lean's `Meta.TransparencyMode` at 4.29.1. Declaration
/// order doubles as the on-wire byte the Lean shim reads; the
/// [`Self::as_byte`] accessor exposes that contract for the dispatch
/// site. `#[non_exhaustive]` so toolchain refinements can extend the
/// taxonomy (e.g., a hypothetical `None`) without breaking exhaustive
/// matches downstream.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeanMetaTransparency {
    /// Lean's standard reducibility — non-reducible / non-irreducible
    /// definitions unfold on demand.
    #[default]
    Default,
    /// Only `@[reducible]` definitions unfold. Useful when you want
    /// [`crate::host::meta::whnf`] to expose the surface structure of a
    /// term without diving into expensive bodies.
    Reducible,
    /// `Default` plus the bodies of instance bindings.
    Instances,
    /// Every definition unfolds. Most aggressive setting — also the
    /// most likely to blow the heartbeat budget on non-trivial terms.
    All,
}

impl LeanMetaTransparency {
    /// On-wire byte the Lean shim's `transparencyOfByte` reads.
    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::Reducible => 1,
            Self::Instances => 2,
            Self::All => 3,
        }
    }
}

/// Bounded options threaded into [`crate::LeanSession::run_meta`].
///
/// Construct through [`Self::new`] or [`Default::default`] and chain
/// the per-field builder methods. Each setter saturates at the same
/// ceiling [`crate::LeanElabOptions`] uses; the namespace context is
/// bounded at [`crate::LEAN_ERROR_MESSAGE_LIMIT`].
///
/// ```ignore
/// let opts = LeanMetaOptions::new()
///     .heartbeat_limit(50_000)
///     .transparency(LeanMetaTransparency::Reducible);
/// ```
#[derive(Clone, Debug)]
pub struct LeanMetaOptions {
    namespace_context: String,
    heartbeat_limit: u64,
    diagnostic_byte_limit: usize,
    transparency: LeanMetaTransparency,
}

impl LeanMetaOptions {
    /// Construct an options bundle with the documented defaults: empty
    /// namespace context, [`LEAN_HEARTBEAT_LIMIT_DEFAULT`] heartbeats,
    /// [`LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT`] bytes of diagnostics, and
    /// [`LeanMetaTransparency::Default`] reducibility.
    #[must_use]
    pub fn new() -> Self {
        Self {
            namespace_context: String::new(),
            heartbeat_limit: LEAN_HEARTBEAT_LIMIT_DEFAULT,
            diagnostic_byte_limit: LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT,
            transparency: LeanMetaTransparency::Default,
        }
    }

    /// Replace the heartbeat limit. Values above
    /// [`LEAN_HEARTBEAT_LIMIT_MAX`] saturate at the ceiling.
    #[must_use]
    pub fn heartbeat_limit(mut self, heartbeats: u64) -> Self {
        self.heartbeat_limit = heartbeats.min(LEAN_HEARTBEAT_LIMIT_MAX);
        self
    }

    /// Replace the diagnostic byte budget. Values above
    /// [`LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`] saturate at the ceiling.
    /// Threaded through the ABI; the current single-message failure
    /// branches do not actively truncate (Rust's `LeanDiagnostic`
    /// decoder already bounds at [`crate::LEAN_ERROR_MESSAGE_LIMIT`]).
    /// Multi-message services would consume the budget the same way
    /// the elaboration shim's `serializeMessages` does.
    #[must_use]
    pub fn diagnostic_byte_limit(mut self, bytes: usize) -> Self {
        self.diagnostic_byte_limit = bytes.min(LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX);
        self
    }

    /// Replace the namespace context the meta runner opens before
    /// evaluating the action (default empty, meaning the imported
    /// environment's root namespace). Long strings truncate at
    /// [`crate::LEAN_ERROR_MESSAGE_LIMIT`] on a UTF-8 char boundary.
    #[must_use]
    pub fn namespace_context(mut self, ns: &str) -> Self {
        self.namespace_context = bound_message(ns.to_owned());
        self
    }

    /// Replace the reducibility setting. Default is
    /// [`LeanMetaTransparency::Default`], matching Lean's `Meta`
    /// default.
    #[must_use]
    pub fn transparency(mut self, transparency: LeanMetaTransparency) -> Self {
        self.transparency = transparency;
        self
    }

    // -- crate-internal accessors used by the dispatch site -----------

    #[allow(
        dead_code,
        reason = "first caller lands with the run_meta dispatch in the same prompt"
    )]
    pub(crate) fn namespace_context_str(&self) -> &str {
        &self.namespace_context
    }

    pub(crate) fn heartbeats(&self) -> u64 {
        self.heartbeat_limit
    }

    pub(crate) fn diagnostic_byte_limit_usize(&self) -> usize {
        self.diagnostic_byte_limit
    }

    pub(crate) fn transparency_byte(&self) -> u8 {
        self.transparency.as_byte()
    }
}

impl Default for LeanMetaOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LEAN_ERROR_MESSAGE_LIMIT;

    #[test]
    fn defaults_match_published_constants() {
        let opts = LeanMetaOptions::new();
        assert_eq!(opts.heartbeats(), LEAN_HEARTBEAT_LIMIT_DEFAULT);
        assert_eq!(opts.diagnostic_byte_limit_usize(), LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT);
        assert_eq!(opts.namespace_context_str(), "");
        assert_eq!(opts.transparency_byte(), 0);
    }

    #[test]
    fn heartbeat_setter_saturates_at_max() {
        let opts = LeanMetaOptions::new().heartbeat_limit(u64::MAX);
        assert_eq!(opts.heartbeats(), LEAN_HEARTBEAT_LIMIT_MAX);
    }

    #[test]
    fn diagnostic_byte_limit_setter_saturates_at_max() {
        let opts = LeanMetaOptions::new().diagnostic_byte_limit(usize::MAX);
        assert_eq!(opts.diagnostic_byte_limit_usize(), LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX);
    }

    #[test]
    fn namespace_context_bounded() {
        let long = "x".repeat(LEAN_ERROR_MESSAGE_LIMIT * 2);
        let opts = LeanMetaOptions::new().namespace_context(&long);
        assert!(opts.namespace_context_str().len() <= LEAN_ERROR_MESSAGE_LIMIT);
    }

    #[test]
    fn transparency_byte_matches_lean_constructor_order() {
        assert_eq!(LeanMetaTransparency::Default.as_byte(), 0);
        assert_eq!(LeanMetaTransparency::Reducible.as_byte(), 1);
        assert_eq!(LeanMetaTransparency::Instances.as_byte(), 2);
        assert_eq!(LeanMetaTransparency::All.as_byte(), 3);
    }
}
