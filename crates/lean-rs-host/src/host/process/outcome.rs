//! [`ProcessFileOutcome`] — the typed outcome of
//! [`crate::LeanSession::process_with_info_tree`].
//!
//! Two branches cover the cases callers must distinguish:
//!
//! - `Processed` — the elaborator ran; the projection is in the
//!   payload. Heartbeat exhaustion during a single command's
//!   elaboration surfaces as an error-severity entry in
//!   `ProcessedFile::diagnostics`, the same path the elaborator uses
//!   for any other failed command — there is no separate "timed out"
//!   wire arm because `IO.processCommands` catches per-command
//!   exceptions and attaches them to the message log.
//! - `Unsupported` — the loaded capability dylib does not export
//!   `lean_rs_host_process_with_info_tree`. The session returns this
//!   without invoking the FFI, matching the meta-service degradation
//!   pattern.

use crate::host::process::info_tree::ProcessedFile;

/// Outcome of [`crate::LeanSession::process_with_info_tree`].
///
/// `#[non_exhaustive]` so future capability refinements can extend the
/// taxonomy without breaking exhaustive matches.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessFileOutcome {
    /// The elaborator ran and produced an `Elab.InfoTree` projection.
    /// `ProcessedFile::diagnostics` carries every error-severity entry
    /// the elaborator emitted; callers that need to distinguish
    /// heartbeat exhaustion from other failures should match on the
    /// diagnostics there.
    Processed(ProcessedFile),
    /// The capability dylib does not export the
    /// `lean_rs_host_process_with_info_tree` shim. No FFI call was
    /// made; callers can fall back or degrade as appropriate.
    Unsupported,
}
