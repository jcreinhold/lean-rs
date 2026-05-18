//! Layered link-time / discovery diagnostics.
//!
//! Each variant `Display`s as exactly one line so the message can be emitted
//! verbatim via `cargo:warning=`.

use std::fmt;
use std::path::PathBuf;

/// Reasons the Lean toolchain or its linkage could not be resolved.
///
/// Variants carry enough context to produce a single actionable diagnostic.
/// The enum is `#[non_exhaustive]` — adding a new failure mode is not a
/// breaking change.
#[non_exhaustive]
#[derive(Debug)]
pub enum LinkDiagnostics {
    /// No discovery probe could locate a Lean prefix containing `include/lean/lean.h`.
    MissingLean {
        /// One human-readable line per probe attempted, in precedence order.
        tried: Vec<String>,
    },
    /// A discovered prefix is missing the expected header file.
    MissingHeader {
        /// Where the header was expected to live.
        path: PathBuf,
    },
    /// A required Lean library was not found in any search directory.
    MissingLib {
        /// Library name as it would appear in `-l<name>`.
        name: String,
        /// Directories searched, in order.
        search_dirs: Vec<PathBuf>,
    },
    /// The discovered Lean version disagrees with the build-baked one.
    VersionMismatch {
        /// Version this build of `lean-toolchain` was compiled against.
        expected: String,
        /// Version reported by the active toolchain at runtime.
        actual: String,
    },
    /// A symbol in the required-symbols allowlist failed to resolve.
    AllowlistFailure {
        /// Name of the missing symbol.
        name: &'static str,
    },
    /// A built Lake fixture artifact is missing.
    FixtureArtifactMissing {
        /// Path to the missing artifact.
        path: PathBuf,
        /// One-liner recovery command for the embedder.
        recovery: &'static str,
    },
    /// The active Lean toolchain is outside the supported window
    /// declared by [`lean_rs_sys::SUPPORTED_TOOLCHAINS`].
    UnsupportedToolchain {
        /// `LEAN_VERSION_STRING` of the active toolchain.
        active: String,
        /// Comma-joined `versions` arrays from each
        /// [`SupportedToolchain`](lean_rs_sys::SupportedToolchain) entry,
        /// rendered as `["4.23.0", "4.24.0", "4.24.1"], ["4.25.0", ...], ...`.
        supported_window: String,
    },
}

impl fmt::Display for LinkDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLean { tried } => {
                // Keep the message on one line: collapse the per-probe records
                // with a literal "; ".
                let joined = tried.join("; ");
                write!(f, "lean-toolchain: no Lean toolchain found; tried: {joined}")
            }
            Self::MissingHeader { path } => {
                write!(f, "lean-toolchain: missing Lean header at {}", path.display())
            }
            Self::MissingLib { name, search_dirs } => {
                let dirs = search_dirs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":");
                write!(f, "lean-toolchain: missing Lean library `{name}` (searched: {dirs})")
            }
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "lean-toolchain: Lean version mismatch: built against {expected}, discovered {actual}"
                )
            }
            Self::AllowlistFailure { name } => {
                write!(f, "lean-toolchain: required symbol `{name}` failed to resolve")
            }
            Self::FixtureArtifactMissing { path, recovery } => {
                write!(
                    f,
                    "lean-toolchain: missing fixture artifact {} (recovery: {recovery})",
                    path.display()
                )
            }
            Self::UnsupportedToolchain {
                active,
                supported_window,
            } => {
                write!(
                    f,
                    "lean-toolchain: active Lean toolchain {active} is not in the supported window: {supported_window}"
                )
            }
        }
    }
}

impl std::error::Error for LinkDiagnostics {}
