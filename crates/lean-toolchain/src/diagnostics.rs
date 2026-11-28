//! Layered link-time / discovery diagnostics.
//!
//! Each variant `Display`s as exactly one line so the message can be emitted
//! verbatim via `cargo:warning=`.

use std::fmt;
use std::path::PathBuf;

/// Reasons the Lean toolchain or its linkage could not be resolved.
///
/// Variants carry enough context to produce a single actionable diagnostic.
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
    /// declared by [`lean_rs_abi::SUPPORTED_TOOLCHAINS`].
    UnsupportedToolchain {
        /// `LEAN_VERSION_STRING` of the active toolchain.
        active: String,
        /// Comma-joined `versions` arrays from each
        /// [`SupportedToolchain`](lean_rs_abi::SupportedToolchain) entry,
        /// rendered as `["4.23.0", "4.24.0", "4.24.1"], ["4.25.0", ...], ...`.
        supported_window: String,
    },
    /// The directory has neither `lakefile.lean` nor `lakefile.toml`, so Lake
    /// has no workspace to answer for.
    LakeConfigMissing {
        /// Directory that should hold the lakefile.
        project_root: PathBuf,
    },
    /// Lake reported that the workspace has no Lean library by this name.
    LakeTargetMissing {
        /// Root of the Lake workspace.
        project_root: PathBuf,
        /// Requested library name.
        target_name: String,
    },
    /// The `lake` executable could not be started.
    LakeUnavailable {
        /// Directory where the Lake command would have run.
        project_root: PathBuf,
        /// The Lake command line, for example `lake query Foo:shared`.
        command: String,
        /// One-line process-spawn failure.
        detail: String,
    },
    /// A Lake command exited unsuccessfully.
    LakeFailed {
        /// Directory where the Lake command ran.
        project_root: PathBuf,
        /// The Lake command line, for example `lake query Foo:shared`.
        command: String,
        /// Process exit status rendered for diagnostics.
        status: String,
        /// Bounded one-line stdout/stderr summary from Lake.
        detail: String,
    },
    /// A Lake command succeeded, but its output was not the answer it
    /// documents, such as a relative path or a missing `LEAN_PATH` line.
    LakeAnswerInvalid {
        /// Directory where the Lake command ran.
        project_root: PathBuf,
        /// The Lake command line, for example `lake query Foo:shared`.
        command: String,
        /// One-line reason.
        reason: String,
    },
    /// `lean-toolchain` refused a capability's settings or Lake's answer for
    /// its library, or could not write the capability manifest.
    LakeOutputUnresolved {
        /// Root of the Lake workspace.
        project_root: PathBuf,
        /// Requested library name.
        target_name: String,
        /// One-line reason, naming the path that failed.
        reason: String,
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
            Self::LakeConfigMissing { project_root } => {
                write!(
                    f,
                    "lean-toolchain: no lakefile.lean or lakefile.toml in {}",
                    project_root.display()
                )
            }
            Self::LakeTargetMissing {
                project_root,
                target_name,
            } => {
                write!(
                    f,
                    "lean-toolchain: Lake has no library `{target_name}` in {}",
                    project_root.display()
                )
            }
            Self::LakeUnavailable {
                project_root,
                command,
                detail,
            } => {
                write!(
                    f,
                    "lean-toolchain: could not start `{command}` in {}: {detail}",
                    project_root.display()
                )
            }
            Self::LakeFailed {
                project_root,
                command,
                status,
                detail,
            } => {
                write!(
                    f,
                    "lean-toolchain: `{command}` failed in {} with {status}: {detail}",
                    project_root.display()
                )
            }
            Self::LakeAnswerInvalid {
                project_root,
                command,
                reason,
            } => {
                write!(
                    f,
                    "lean-toolchain: `{command}` in {} gave an answer lean-toolchain cannot use: {reason}",
                    project_root.display()
                )
            }
            Self::LakeOutputUnresolved {
                project_root,
                target_name,
                reason,
            } => {
                write!(
                    f,
                    "lean-toolchain: could not prepare library `{target_name}` in {}: {reason}",
                    project_root.display()
                )
            }
        }
    }
}

impl std::error::Error for LinkDiagnostics {}
