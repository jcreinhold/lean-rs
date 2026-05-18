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
//!
//! ## Diagnostic codes
//!
//! Every error-bearing type on the public surface projects to a stable
//! [`LeanDiagnosticCode`] via `.code()`. The code names the failure
//! family a downstream caller must react to — `Linking`, `Elaboration`,
//! `Unsupported`, and so on — independent of the internal [`HostStage`]
//! tag. The `as_str()` form of each code is the identifier used by the
//! tracing spans and the published `docs/diagnostics.md` catalogue;
//! variant names and string ids are stable across patch releases.

use std::any::Any;
use std::fmt;

pub(crate) mod capture;
pub(crate) mod io;
pub(crate) mod panic;
pub(crate) mod redact;

#[cfg(test)]
mod tests;

pub use self::capture::{CapturedEvent, DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY, DiagnosticCapture};

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
    /// Project to the stable [`LeanDiagnosticCode`] taxonomy.
    ///
    /// `LeanException` always maps to [`LeanDiagnosticCode::LeanException`];
    /// `Host` returns the code recorded by the construction site.
    #[must_use]
    pub fn code(&self) -> LeanDiagnosticCode {
        match self {
            Self::LeanException(_) => LeanDiagnosticCode::LeanException,
            Self::Host(failure) => failure.code,
        }
    }

    /// Build a `RuntimeInit` host failure.
    pub(crate) fn runtime_init(message: impl Into<String>) -> Self {
        Self::host(HostStage::RuntimeInit, LeanDiagnosticCode::RuntimeInit, message)
    }

    /// Build a `RuntimeInit` host failure from a caught panic payload.
    pub(crate) fn runtime_init_panic(payload: &(dyn Any + Send)) -> Self {
        Self::runtime_init(render_panic_payload(payload))
    }

    /// Build a `Linking` host failure (missing/invalid Lake names,
    /// missing initializer symbol, header-digest mismatch).
    pub(crate) fn linking(message: impl Into<String>) -> Self {
        Self::host(HostStage::Link, LeanDiagnosticCode::Linking, message)
    }

    /// Build a `ModuleInit` host failure (dylib could not be opened,
    /// initializer raised, Lake project path bad).
    pub(crate) fn module_init(message: impl Into<String>) -> Self {
        Self::host(HostStage::Load, LeanDiagnosticCode::ModuleInit, message)
    }

    /// Build a `ModuleInit` host failure from a caught panic payload.
    pub(crate) fn module_init_panic(payload: &(dyn Any + Send)) -> Self {
        Self::module_init(render_panic_payload(payload))
    }

    /// Build a `SymbolLookup` host failure (dlsym miss, signature
    /// mismatch — function symbol expected but global found, etc.).
    pub(crate) fn symbol_lookup(message: impl Into<String>) -> Self {
        Self::host(HostStage::Link, LeanDiagnosticCode::SymbolLookup, message)
    }

    /// Build an `AbiConversion` host failure (wrong Lean kind, integer
    /// out of range, invalid UTF-8, missing declaration).
    pub(crate) fn abi_conversion(message: impl Into<String>) -> Self {
        Self::host(HostStage::Conversion, LeanDiagnosticCode::AbiConversion, message)
    }

    /// Build a Lean-thrown-exception report with a bounded message.
    pub(crate) fn lean_exception(kind: LeanExceptionKind, message: impl Into<String>) -> Self {
        Self::LeanException(LeanException {
            kind,
            message: bound_message(message.into()),
        })
    }

    /// Build a host-stack failure from a caught `std::panic::catch_unwind`
    /// payload at a Lean → Rust callback boundary. The payload is
    /// rendered into a string before bounding so the panic value never
    /// escapes the error boundary.
    pub(crate) fn callback_panic(payload: &(dyn Any + Send)) -> Self {
        Self::host(
            HostStage::CallbackPanic,
            LeanDiagnosticCode::Internal,
            render_panic_payload(payload),
        )
    }

    /// Shared host-failure constructor. Private — every call site uses
    /// the typed wrappers above so the [`HostStage`] / [`LeanDiagnosticCode`]
    /// pair cannot drift.
    fn host(stage: HostStage, code: LeanDiagnosticCode, message: impl Into<String>) -> Self {
        Self::Host(HostFailure {
            stage,
            code,
            message: bound_message(message.into()),
        })
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
    ///
    /// Use this to classify a Lean-thrown exception when the
    /// caller-facing taxonomy in [`LeanDiagnosticCode`] is not specific
    /// enough — for example, distinguishing `FileNotFound` from
    /// `PermissionDenied` to drive different recovery paths.
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
    code: LeanDiagnosticCode,
    message: String,
}

impl HostFailure {
    /// The host-stack stage that observed the failure.
    ///
    /// This is the *internal* classification; reach for
    /// [`Self::code`] when displaying a stable identifier or routing on
    /// the caller-facing failure family. The [`HostStage`] tag may grow
    /// new variants alongside new internal paths.
    #[must_use]
    pub fn stage(&self) -> HostStage {
        self.stage
    }

    /// The stable diagnostic code matching this failure.
    ///
    /// Recorded at the construction site rather than projected from
    /// [`Self::stage`], so the code identity does not drift if the
    /// internal stage tag is later refined. Use this for stable
    /// logging, metrics, or downstream error routing.
    #[must_use]
    pub fn code(&self) -> LeanDiagnosticCode {
        self.code
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
        write!(
            f,
            "host stage {:?} [{}]: {}",
            self.stage,
            self.code.as_str(),
            self.message
        )
    }
}

/// What the host stack was doing when it failed.
///
/// A flat tag enum; callers rarely match on it and read
/// [`HostFailure::message`] instead. Use [`LeanDiagnosticCode`] for the
/// stable, caller-facing failure taxonomy — `HostStage` is the
/// host-stack's internal classification and may grow new variants when
/// new internal paths are added. Marked `#[non_exhaustive]` so later
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
    Link,
    /// Load-time failure (`dlopen`, per-module initializer).
    Load,
    /// A `pub(crate)` invariant tripped. Indicates a bug in `lean-rs`.
    Internal,
}

/// Stable, caller-facing classification of a `lean-rs` failure.
///
/// Every error-bearing public type projects to one of these via
/// `.code()`:
///
/// - [`LeanError::code`] — `LeanException` → [`LeanDiagnosticCode::LeanException`],
///   `Host` → the code recorded by the construction site.
/// - `lean_rs_host::LeanElabFailure::code` — always [`LeanDiagnosticCode::Elaboration`].
/// - `lean_rs_host::meta::LeanMetaResponse::code` — `Ok` → `None`,
///   `Failed` / `TimeoutOrHeartbeat` → `Elaboration`, `Unsupported` →
///   `Unsupported`.
///
/// Use `match err.code() { Linking => ..., ModuleInit => ..., _ => ... }`
/// to react by family; reach for [`HostStage`] only when you need the
/// host-stack's internal classification (and accept that it may grow new
/// variants). The string form returned by [`Self::as_str`] is also the
/// identifier emitted in tracing fields and listed in
/// `docs/diagnostics.md`.
///
/// `#[non_exhaustive]` so later prompts may add new families. The
/// variant names and `as_str()` ids are stable across patch releases.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LeanDiagnosticCode {
    /// Lean runtime initialization failed (panic in `lean_initialize`,
    /// thread-attach floor failure, task-manager init failure).
    RuntimeInit,
    /// A linkable artefact was missing or did not match: a Lake
    /// package/module name was invalid, the initializer symbol was
    /// absent, or the header digest did not match the active toolchain.
    Linking,
    /// A capability dylib could not be opened, parsed, or its root
    /// module initializer raised. Also: the Lake project root did not
    /// exist or was not a directory.
    ModuleInit,
    /// A function or global symbol was not present in the loaded dylib
    /// when a session call tried to resolve it.
    SymbolLookup,
    /// An ABI conversion failed: wrong Lean kind for the requested
    /// Rust type, integer out of range, invalid UTF-8, or a queried
    /// declaration was missing from the imported environment.
    AbiConversion,
    /// Lean raised through its `IO` error channel. Inspect
    /// [`LeanException::kind`] for the `IO.Error` constructor.
    LeanException,
    /// Term parsing or elaboration produced one or more diagnostics.
    /// The payload is a `lean_rs_host::LeanElabFailure` with the typed
    /// diagnostic list.
    Elaboration,
    /// The loaded capability does not expose the requested service —
    /// either the Lean shim returned `unsupported` for the request
    /// shape, or the optional capability symbol was absent at load
    /// time.
    Unsupported,
    /// A `pub(crate)` invariant tripped, or a callback panicked inside
    /// the safe boundary. Indicates a bug in `lean-rs`.
    Internal,
}

impl LeanDiagnosticCode {
    /// Stable identifier used in tracing fields and `docs/diagnostics.md`.
    ///
    /// The returned string is part of the published API; the value for
    /// an existing variant is fixed across patch releases. New variants
    /// may add new ids.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeInit => "lean_rs.runtime_init",
            Self::Linking => "lean_rs.linking",
            Self::ModuleInit => "lean_rs.module_init",
            Self::SymbolLookup => "lean_rs.symbol_lookup",
            Self::AbiConversion => "lean_rs.abi_conversion",
            Self::LeanException => "lean_rs.lean_exception",
            Self::Elaboration => "lean_rs.elaboration",
            Self::Unsupported => "lean_rs.unsupported",
            Self::Internal => "lean_rs.internal",
        }
    }

    /// One-line prose description used in `docs/diagnostics.md` and as
    /// the default fallback when a span needs human-readable context.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::RuntimeInit => "Lean runtime initialization failed",
            Self::Linking => "a linkable artefact was missing or mismatched",
            Self::ModuleInit => "a capability dylib could not be opened or initialized",
            Self::SymbolLookup => "a symbol was not present in the loaded dylib",
            Self::AbiConversion => "an ABI conversion failed",
            Self::LeanException => "Lean raised through its IO error channel",
            Self::Elaboration => "term parsing or elaboration produced diagnostics",
            Self::Unsupported => "the loaded capability does not expose the requested service",
            Self::Internal => "a pub(crate) invariant tripped — likely a bug in lean-rs",
        }
    }
}

impl fmt::Display for LeanDiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
/// `#[doc(hidden)] pub` so the sibling `lean-rs-host` crate can apply the
/// same bound when it constructs `LeanError::Host(HostFailure)` values
/// through the `__host_internals` helpers. Re-exported through
/// [`crate::__host_internals`]; not part of the public semver promise.
#[doc(hidden)]
pub fn bound_message(mut s: String) -> String {
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

// -- L1 → L2 boundary helpers -----------------------------------------
//
// `LeanError`'s constructors are `pub(crate)` to preserve the structural
// bounding invariant: external crates cannot mint `LeanError` values
// directly (per RD-2026-05-17-006). The sibling `lean-rs-host` crate
// needs to construct host failures when it dispatches capability shims;
// it reaches the one constructor it actually uses through this
// `#[doc(hidden)] pub fn` wrapper, re-exported at
// [`crate::__host_internals`].
//
// The original boundary exposed eight constructor wrappers
// (`host_linking`, `host_module_init_panic`, `host_symbol_lookup`,
// `host_callback_panic`, `host_internal`, `lean_exception`, plus
// `bound_message`); the post-RD-2026-05-18-001 audit confirmed only
// `host_module_init` (called from `lake.rs`) was wired up. Carrying
// the dead seven was 87% speculative surface; they were removed. Add
// them back the same way (single-call wrapper + re-export in
// `crate::__host_internals`) if a future call site needs one.

/// Construct a `ModuleInit` host failure. See [`LeanError::module_init`].
#[doc(hidden)]
pub fn host_module_init(message: impl Into<String>) -> LeanError {
    LeanError::module_init(message)
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
