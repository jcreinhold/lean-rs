//! Typed error boundary for the safe `lean-rs` surface.
//!
//! Every fallible public function returns [`LeanResult<T>`]. [`LeanError`] is
//! the single error type that crosses the boundary: a [`#[non_exhaustive]`]
//! enum whose variants are filled in as the implementation prompts land.
//!
//! Prompt 06 lands only the [`LeanError::Init`] variant, backed by
//! [`InitError`], so [`crate::LeanRuntime::init`] has a typed return.
//! Prompt 10 will land the remaining variants (`Link`, `Load`,
//! `Conversion`, `LeanException`, `Internal`) plus the `IoResult<T>`
//! machinery and the `LEAN_ERROR_MESSAGE_LIMIT` constant — additions
//! remain non-breaking because the enum is marked `#[non_exhaustive]`.
//!
//! The rule callers will eventually learn (prompt 10): **runtime and host
//! failures are [`LeanError`]; application semantics are values.** A Lean
//! function returning `IO (Except E T)` decodes as
//! `LeanResult<Result<T, E>>` — outer `IO` failure becomes a [`LeanError`]
//! variant, inner `Except` becomes a Rust [`Result`].

use std::any::Any;
use std::fmt;

/// Result alias used by every fallible public API in `lean-rs`.
pub type LeanResult<T> = Result<T, LeanError>;

/// Errors reported across the safe `lean-rs` boundary.
///
/// `#[non_exhaustive]` so additional variants (introduced by prompt 10) do
/// not break callers that match on this enum.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum LeanError {
    /// Lean runtime initialization, toolchain discovery, or argument-setup
    /// failure. See [`InitError`].
    Init(InitError),
}

impl fmt::Display for LeanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init(err) => write!(f, "lean-rs: {err}"),
        }
    }
}

impl std::error::Error for LeanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Init(err) => Some(err),
        }
    }
}

impl From<InitError> for LeanError {
    fn from(err: InitError) -> Self {
        Self::Init(err)
    }
}

/// Failure modes reported when bringing up the Lean runtime.
///
/// `#[non_exhaustive]` so prompt 10 (and later prompts that add discovery
/// or argument-setup hooks) can extend the set without breaking matches.
/// Cloneable so that [`crate::LeanRuntime::init`] can cache a failed
/// initialization result in its `OnceLock` and replay it on every
/// subsequent call.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum InitError {
    /// A Rust panic was caught by the panic boundary around the initial
    /// runtime calls. The payload is captured as a bounded UTF-8 string
    /// (default cap 4 KiB) so it can be reported without propagating
    /// across the C frames.
    RuntimePanic {
        /// Best-effort rendering of the panic payload.
        message: String,
    },
}

impl InitError {
    /// Build an [`InitError::RuntimePanic`] from a captured panic payload.
    ///
    /// Renders `&'static str` and `String` payloads verbatim; other payload
    /// types collapse to a generic placeholder. The resulting message is
    /// truncated to at most 4 KiB on a UTF-8 char boundary.
    pub(crate) fn runtime_panic(payload: &(dyn Any + Send)) -> Self {
        let raw: &str = if let Some(s) = payload.downcast_ref::<&'static str>() {
            s
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.as_str()
        } else {
            "Lean runtime initialization panicked with a non-string payload"
        };
        Self::RuntimePanic {
            message: bound_message(raw, INIT_ERROR_MESSAGE_LIMIT),
        }
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimePanic { message } => {
                write!(f, "Lean runtime initialization panicked: {message}")
            }
        }
    }
}

impl std::error::Error for InitError {}

/// Hard cap on bytes captured from an [`InitError::RuntimePanic`] payload.
///
/// Mirrors the 4 KiB cap that prompt 10 will publish as the workspace-wide
/// `LEAN_ERROR_MESSAGE_LIMIT`; declared locally so this prompt does not
/// reach forward to a constant that does not yet exist.
const INIT_ERROR_MESSAGE_LIMIT: usize = 4096;

/// Truncate `s` to at most `limit` bytes on a UTF-8 char boundary.
fn bound_message(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_owned();
    }
    let mut acc = String::with_capacity(limit);
    for ch in s.chars() {
        let next = acc.len().saturating_add(ch.len_utf8());
        if next > limit {
            break;
        }
        acc.push(ch);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::{INIT_ERROR_MESSAGE_LIMIT, InitError, LeanError, bound_message};

    #[test]
    fn bound_message_passes_short_strings_through() {
        let short = "hello";
        assert_eq!(bound_message(short, 16), "hello");
    }

    #[test]
    fn bound_message_truncates_on_char_boundary() {
        // Three-byte chars; cap mid-char must drop the partial one.
        let s = "\u{1F600}\u{1F600}\u{1F600}"; // 4 bytes each
        let bounded = bound_message(s, 5);
        assert!(bounded.len() <= 5);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn runtime_panic_renders_str_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");
        let err = InitError::runtime_panic(&*payload);
        let InitError::RuntimePanic { message } = err;
        assert_eq!(message, "boom");
    }

    #[test]
    fn runtime_panic_renders_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("kaboom"));
        let err = InitError::runtime_panic(&*payload);
        let InitError::RuntimePanic { message } = err;
        assert_eq!(message, "kaboom");
    }

    #[test]
    fn runtime_panic_bounds_oversize_payload() {
        let payload: Box<dyn std::any::Any + Send> =
            Box::new("x".repeat(INIT_ERROR_MESSAGE_LIMIT.saturating_add(1024)));
        let err = InitError::runtime_panic(&*payload);
        let InitError::RuntimePanic { message } = err;
        assert!(message.len() <= INIT_ERROR_MESSAGE_LIMIT);
    }

    #[test]
    fn lean_error_displays_init_failure() {
        let err = LeanError::Init(InitError::RuntimePanic { message: "boom".into() });
        let rendered = err.to_string();
        assert!(rendered.contains("Lean runtime initialization panicked"));
        assert!(rendered.contains("boom"));
    }
}
