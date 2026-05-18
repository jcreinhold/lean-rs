//! Term elaboration and kernel-checking capabilities on
//! [`crate::LeanSession`].
//!
//! Two session methods sit alongside the read-only environment-query
//! surface:
//!
//! - [`crate::LeanSession::elaborate`] — parse + elaborate a single Lean
//!   term against an optional expected type, returning a
//!   [`crate::LeanExpr`] handle on success or a [`LeanElabFailure`]
//!   carrying typed diagnostics on parse / type failures.
//! - [`crate::LeanSession::kernel_check`] — parse + elaborate + kernel-
//!   check a full Lean declaration, returning a
//!   [`crate::host::evidence::LeanKernelOutcome`] that tags the result
//!   as `Checked` (and carries a [`crate::LeanEvidence`] handle),
//!   `Rejected`, `Unavailable`, or `Unsupported`.
//!
//! Both methods accept a [`LeanElabOptions`] bundle whose setters
//! saturate at the published ceilings — there is no error path for
//! out-of-range option values; the bound exists as a safety rail.
//!
//! The capability contract names two Lean-side fixture exports
//! (`lean_rs_host_elaborate`, `lean_rs_host_kernel_check`) alongside the
//! seven environment-query symbols. [`crate::host::LeanCapabilities`]
//! caches both addresses at load time so the per-call cost is one
//! struct-field read plus one FFI call.

pub(crate) mod diagnostic;
pub(crate) mod failure;
mod options;

pub use self::diagnostic::{LeanDiagnostic, LeanPosition, LeanSeverity};
pub use self::failure::LeanElabFailure;
pub use self::options::{
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX, LEAN_HEARTBEAT_LIMIT_DEFAULT,
    LEAN_HEARTBEAT_LIMIT_MAX, LeanElabOptions,
};
