//! Bounded option bundle for [`crate::LeanSession::elaborate`] and
//! [`crate::LeanSession::kernel_check`].
//!
//! All setters are saturating: values above the declared maximum are
//! clamped rather than rejected. The bounds exist as safety rails (the
//! heartbeat ceiling keeps a runaway elaborator finite; the diagnostic
//! byte limit keeps a chatty failure path from monopolising memory) and
//! callers should not have to write `.map_err` around configuration.
//!
//! The four fields are documented per-setter on [`LeanElabOptions`]; the
//! crate-internal accessors used by the dispatch site live alongside them
//! as `pub(crate)`.

#[cfg(test)]
use crate::error::LEAN_ERROR_MESSAGE_LIMIT;
use crate::error::bound_message;

/// Default heartbeat ceiling — matches Lean's own `maxHeartbeats` default
/// at 4.29.1 (`Lean.Core.maxHeartbeats`).
pub const LEAN_HEARTBEAT_LIMIT_DEFAULT: u64 = 200_000;

/// Upper bound on the heartbeat ceiling. 1000× the default; values above
/// saturate at this ceiling so a runaway elaborator finishes in bounded
/// real time on every supported host.
pub const LEAN_HEARTBEAT_LIMIT_MAX: u64 = 200_000_000;

/// Default byte budget for the diagnostic collection returned per call
/// (64 KiB ≈ 16 default-bounded messages).
pub const LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT: usize = 64 * 1024;

/// Upper bound on the diagnostic byte budget (1 MiB).
pub const LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX: usize = 1024 * 1024;

const DEFAULT_FILE_LABEL: &str = "<elaborate>";

/// Bounded options threaded into [`crate::LeanSession::elaborate`] and
/// [`crate::LeanSession::kernel_check`].
///
/// Construct through [`Self::new`] or [`Default::default`] and chain the
/// per-field builder methods. Each setter saturates at its declared
/// ceiling and bounds string fields at [`crate::LEAN_ERROR_MESSAGE_LIMIT`].
///
/// ```ignore
/// let opts = LeanElabOptions::new()
///     .heartbeat_limit(50_000)
///     .namespace_context("Nat")
///     .file_label("examples/intro.lean");
/// ```
#[derive(Clone, Debug)]
pub struct LeanElabOptions {
    namespace_context: String,
    file_label: String,
    heartbeat_limit: u64,
    diagnostic_byte_limit: usize,
}

impl LeanElabOptions {
    /// Construct an options bundle with the documented defaults: empty
    /// namespace context, `<elaborate>` file label,
    /// [`LEAN_HEARTBEAT_LIMIT_DEFAULT`] heartbeats, and
    /// [`LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT`] bytes of diagnostics.
    #[must_use]
    pub fn new() -> Self {
        Self {
            namespace_context: String::new(),
            file_label: DEFAULT_FILE_LABEL.to_owned(),
            heartbeat_limit: LEAN_HEARTBEAT_LIMIT_DEFAULT,
            diagnostic_byte_limit: LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT,
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
    /// [`LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`] saturate at the ceiling. The
    /// Lean side stops collecting diagnostics once the running byte sum
    /// would exceed this value and marks the failure as
    /// [`crate::host::elaboration::LeanElabFailure::truncated`].
    #[must_use]
    pub fn diagnostic_byte_limit(mut self, bytes: usize) -> Self {
        self.diagnostic_byte_limit = bytes.min(LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX);
        self
    }

    /// Replace the namespace context the elaborator opens before parsing
    /// the source (default empty, meaning the imported environment's
    /// root namespace). Long strings are truncated at
    /// [`crate::LEAN_ERROR_MESSAGE_LIMIT`] on a UTF-8 char boundary.
    #[must_use]
    pub fn namespace_context(mut self, ns: &str) -> Self {
        self.namespace_context = bound_message(ns.to_owned());
        self
    }

    /// Replace the file label echoed back in diagnostic positions
    /// (default `"<elaborate>"`). Useful when an editor / linter wants
    /// the diagnostic stream tagged with the originating filename. Long
    /// labels are truncated at [`crate::LEAN_ERROR_MESSAGE_LIMIT`].
    #[must_use]
    pub fn file_label(mut self, label: &str) -> Self {
        self.file_label = bound_message(label.to_owned());
        self
    }

    // -- crate-internal accessors used by the dispatch site -----------

    #[allow(
        dead_code,
        reason = "first caller lands with the session-method dispatch in the same prompt"
    )]
    pub(crate) fn namespace_context_str(&self) -> &str {
        &self.namespace_context
    }

    #[allow(
        dead_code,
        reason = "first caller lands with the session-method dispatch in the same prompt"
    )]
    pub(crate) fn file_label_str(&self) -> &str {
        &self.file_label
    }

    #[allow(
        dead_code,
        reason = "first caller lands with the session-method dispatch in the same prompt"
    )]
    pub(crate) fn heartbeats(&self) -> u64 {
        self.heartbeat_limit
    }

    #[allow(
        dead_code,
        reason = "first caller lands with the session-method dispatch in the same prompt"
    )]
    pub(crate) fn diagnostic_byte_limit_usize(&self) -> usize {
        self.diagnostic_byte_limit
    }
}

impl Default for LeanElabOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_published_constants() {
        let opts = LeanElabOptions::new();
        assert_eq!(opts.heartbeats(), LEAN_HEARTBEAT_LIMIT_DEFAULT);
        assert_eq!(opts.diagnostic_byte_limit_usize(), LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT);
        assert_eq!(opts.namespace_context_str(), "");
        assert_eq!(opts.file_label_str(), DEFAULT_FILE_LABEL);
    }

    #[test]
    fn heartbeat_setter_saturates_at_max() {
        let opts = LeanElabOptions::new().heartbeat_limit(u64::MAX);
        assert_eq!(opts.heartbeats(), LEAN_HEARTBEAT_LIMIT_MAX);
    }

    #[test]
    fn diagnostic_byte_limit_setter_saturates_at_max() {
        let opts = LeanElabOptions::new().diagnostic_byte_limit(usize::MAX);
        assert_eq!(opts.diagnostic_byte_limit_usize(), LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX);
    }

    #[test]
    fn string_setters_bound_input() {
        let long = "x".repeat(LEAN_ERROR_MESSAGE_LIMIT * 2);
        let opts = LeanElabOptions::new().namespace_context(&long).file_label(&long);
        assert!(opts.namespace_context_str().len() <= LEAN_ERROR_MESSAGE_LIMIT);
        assert!(opts.file_label_str().len() <= LEAN_ERROR_MESSAGE_LIMIT);
    }
}
