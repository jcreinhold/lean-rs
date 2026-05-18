//! Panic boundary for Rust closures invoked from Lean.
//!
//! Rust panics must not unwind across C or Lean frames (see
//! `docs/architecture/01-safety-model.md`, "Panic discipline"). The
//! [`catch_callback_panic`] helper wraps a closure that may be called by
//! Lean, contains any panic via [`std::panic::catch_unwind`], and renders
//! the payload as a [`LeanError::Host`] with [`super::HostStage::CallbackPanic`]
//! and code [`super::LeanDiagnosticCode::Internal`].
//!
//! Prompt 10 adds this seam and exercises it from
//! [`crate::error::tests`]; later prompts adopt it for every host-callback
//! registration. The contained-and-converted mode is the only mode this
//! crate offers; an explicit-abort mode (panic-the-process when a
//! callback panics) is not part of the public discipline today.

#![allow(
    dead_code,
    reason = "first non-test caller lands in prompts 11+ (host-callback registration)"
)]

use std::panic::{self, AssertUnwindSafe};

use super::{LeanError, LeanResult};

/// Run `f` and return its result; if `f` panics, contain the panic and
/// return [`LeanError::callback_panic`] (code
/// [`super::LeanDiagnosticCode::Internal`], stage
/// [`super::HostStage::CallbackPanic`]).
///
/// `AssertUnwindSafe` is required because [`LeanResult`] does not
/// implement [`UnwindSafe`] (it can carry interior types that do not).
/// The closure is expected to run in Rust-only territory before
/// mutating any Lean state: if a callback is half-way through updating
/// Lean-owned data when it panics, the recovery here cannot restore
/// that state.
///
/// [`UnwindSafe`]: std::panic::UnwindSafe
pub(crate) fn catch_callback_panic<F, R>(f: F) -> LeanResult<R>
where
    F: FnOnce() -> LeanResult<R>,
{
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => Err(LeanError::callback_panic(payload.as_ref())),
    }
}
