//! Typed error boundary for the safe `lean-rs` surface.
//!
//! Every fallible public function returns [`LeanResult<T>`]. [`LeanError`]
//! is the single error type that crosses the boundary; per
//! `RD-2026-05-17-006` it has two variants:
//!
//! - [`LeanError::LeanException`] when Lean threw through its `IO` error
//!   channel. The payload reports the `IO.Error` constructor as
//!   [`LeanExceptionKind`] and a bounded `message()` that callers may
//!   surface to end users.
//! - [`LeanError::Host`] when our stack failed (init, ABI conversion,
//!   contained callback panic, future link/load, internal invariant). The
//!   payload reports the [`HostStage`] and a bounded developer-facing
//!   `message()`.
//!
//! Bounded messages are a *structural* invariant: [`LeanException`] and
//! [`HostFailure`] have private fields, and the only constructors are
//! `pub(crate)`; both run a shared helper that truncates the message at
//! [`LEAN_ERROR_MESSAGE_LIMIT`] on a UTF-8 char boundary. External
//! callers receive `LeanError` values but cannot mint one with an
//! unbounded message.
//!
//! The rule callers learn: **runtime / host failures are [`LeanError`];
//! application semantics are values.** A Lean function returning
//! `IO (Except E T)` decodes as `LeanResult<Result<T, E>>` — outer `IO`
//! failure becomes a [`LeanError::LeanException`], inner `Except`
//! becomes a Rust [`Result`].

use std::any::Any;
use std::fmt;

pub(crate) mod io;
pub(crate) mod panic;

#[cfg(test)]
mod tests;

/// Hard cap on the byte length of any [`LeanError`] message.
///
/// Enforced at construction time by the `pub(crate)` constructors in this
/// module. The truncation respects UTF-8 char boundaries, so the cap is
/// an upper bound rather than an exact length.
pub const LEAN_ERROR_MESSAGE_LIMIT: usize = 4096;

/// Result alias used by every fallible public API in `lean-rs`.
pub type LeanResult<T> = Result<T, LeanError>;

/// Errors reported across the safe `lean-rs` boundary.
///
/// `#[non_exhaustive]` so future toolchain or platform refinements can add
/// new diagnostic tags inside [`HostStage`] / [`LeanExceptionKind`]
/// without breaking pattern-matching code that already handles the two
/// top-level variants.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum LeanError {
    /// Lean threw through its `IO` error channel; see [`LeanException`].
    LeanException(LeanException),
    /// The host stack failed at a particular stage; see [`HostFailure`].
    Host(HostFailure),
}

impl LeanError {
    /// Build a host-stack failure with a bounded message.
    ///
    /// `pub(crate)` so downstream callers cannot mint `LeanError` values
    /// directly — they receive them from the safe API.
    pub(crate) fn host(stage: HostStage, message: impl Into<String>) -> Self {
        Self::Host(HostFailure {
            stage,
            message: bound_message(message.into()),
        })
    }

    /// Build a Lean-thrown-exception report with a bounded message.
    #[allow(
        dead_code,
        reason = "first non-test caller lands in prompts 11–12 (LeanModule + LeanExported{N})"
    )]
    pub(crate) fn lean_exception(kind: LeanExceptionKind, message: impl Into<String>) -> Self {
        Self::LeanException(LeanException {
            kind,
            message: bound_message(message.into()),
        })
    }

    /// Build a host-stack failure from a caught `std::panic::catch_unwind`
    /// payload. The payload is rendered into a string before bounding so
    /// the panic value never escapes the error boundary.
    pub(crate) fn host_panic(stage: HostStage, payload: &(dyn Any + Send)) -> Self {
        Self::host(stage, render_panic_payload(payload))
    }
}

impl fmt::Display for LeanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LeanException(e) => write!(f, "lean-rs: {e}"),
            Self::Host(e) => write!(f, "lean-rs: {e}"),
        }
    }
}

impl std::error::Error for LeanError {}

/// A Lean exception thrown through the `IO` error channel.
///
/// Constructed only by the crate; the `kind` and `message` fields are
/// private so the bounded-message invariant survives downstream
/// pattern-matching.
#[derive(Clone, Debug)]
pub struct LeanException {
    kind: LeanExceptionKind,
    message: String,
}

impl LeanException {
    /// The `IO.Error` constructor Lean reported.
    #[must_use]
    pub fn kind(&self) -> LeanExceptionKind {
        self.kind
    }

    /// What Lean said about the failure, truncated to at most
    /// [`LEAN_ERROR_MESSAGE_LIMIT`] bytes on a UTF-8 char boundary.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for LeanException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lean threw {:?}: {}", self.kind, self.message)
    }
}

/// A host-stack failure observed inside `lean-rs`.
///
/// Constructed only by the crate; fields are private so the bounded-message
/// invariant survives downstream pattern-matching.
#[derive(Clone, Debug)]
pub struct HostFailure {
    stage: HostStage,
    message: String,
}

impl HostFailure {
    /// The host-stack stage that observed the failure.
    #[must_use]
    pub fn stage(&self) -> HostStage {
        self.stage
    }

    /// Developer-facing diagnostic, truncated to at most
    /// [`LEAN_ERROR_MESSAGE_LIMIT`] bytes on a UTF-8 char boundary.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "host stage {:?}: {}", self.stage, self.message)
    }
}

/// What the host stack was doing when it failed.
///
/// A flat tag enum; callers rarely match on it and read
/// [`HostFailure::message`] instead. Marked `#[non_exhaustive]` so later
/// prompts can add variants without breaking exhaustive matches.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HostStage {
    /// `OnceLock` + `lean_initialize_*` panic-or-failure.
    RuntimeInit,
    /// First-order ABI value malformed (wrong kind, out of range,
    /// invalid UTF-8, non-scalar `char`).
    Conversion,
    /// A Rust panic was contained at a Lean → Rust callback boundary.
    CallbackPanic,
    /// Link-time failure (missing symbol, header-digest mismatch).
    /// Reserved for prompt 11+.
    Link,
    /// Load-time failure (`dlopen`, per-module initializer). Reserved
    /// for prompt 11+.
    Load,
    /// A `pub(crate)` invariant tripped. Indicates a bug in `lean-rs`.
    Internal,
}

/// Constructor tag for Lean's `IO.Error`.
///
/// 1:1 with `IO.Error` at the active Lean toolchain version (4.29.1 —
/// see `src/lean/Init/System/IOError.lean`), plus an [`Other`] catch-all
/// for tag values not in the declaration. The crate-internal decoder
/// maps the raw `lean_obj_tag` to a variant via a `const` table; a unit
/// test anchors the `userError` mapping against the live Lean runtime
/// and will fail if the constructor index drifts.
///
/// [`Other`]: Self::Other
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LeanExceptionKind {
    /// `IO.Error.alreadyExists`
    AlreadyExists,
    /// `IO.Error.otherError`
    OtherError,
    /// `IO.Error.resourceBusy`
    ResourceBusy,
    /// `IO.Error.resourceVanished`
    ResourceVanished,
    /// `IO.Error.unsupportedOperation`
    UnsupportedOperation,
    /// `IO.Error.hardwareFault`
    HardwareFault,
    /// `IO.Error.unsatisfiedConstraints`
    UnsatisfiedConstraints,
    /// `IO.Error.illegalOperation`
    IllegalOperation,
    /// `IO.Error.protocolError`
    ProtocolError,
    /// `IO.Error.timeExpired`
    TimeExpired,
    /// `IO.Error.interrupted`
    Interrupted,
    /// `IO.Error.noFileOrDirectory`
    NoFileOrDirectory,
    /// `IO.Error.invalidArgument`
    InvalidArgument,
    /// `IO.Error.permissionDenied`
    PermissionDenied,
    /// `IO.Error.resourceExhausted`
    ResourceExhausted,
    /// `IO.Error.inappropriateType`
    InappropriateType,
    /// `IO.Error.noSuchThing`
    NoSuchThing,
    /// `IO.Error.unexpectedEof`
    UnexpectedEof,
    /// `IO.Error.userError`
    UserError,
    /// Tag did not match any known `IO.Error` constructor at the active
    /// Lean toolchain version.
    Other,
}

/// Truncate `s` to at most [`LEAN_ERROR_MESSAGE_LIMIT`] bytes on a UTF-8
/// char boundary. The single place every constructor enforces the bound.
///
/// `pub(crate)` so the elaboration diagnostic decoder can apply the same
/// bound to per-diagnostic messages it pulls out of Lean.
pub(crate) fn bound_message(mut s: String) -> String {
    if s.len() <= LEAN_ERROR_MESSAGE_LIMIT {
        return s;
    }
    // Walk to the largest char boundary at or below the limit, then
    // truncate. `floor_char_boundary` is unstable; do it manually with a
    // single backward scan from the limit. `saturating_sub` keeps clippy's
    // arithmetic-side-effects lint quiet without changing semantics — the
    // loop guard already prevents `cut == 0` from underflowing.
    let mut cut = LEAN_ERROR_MESSAGE_LIMIT;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    s.truncate(cut);
    s
}

/// Render an arbitrary panic payload as a human-readable string. Strings
/// (both `&'static str` and `String`) come through verbatim; other types
/// collapse to a generic placeholder. Centralised so the
/// `HostStage::RuntimeInit` and `HostStage::CallbackPanic` sites stay
/// consistent.
fn render_panic_payload(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic payload was not a string".to_owned()
    }
}
