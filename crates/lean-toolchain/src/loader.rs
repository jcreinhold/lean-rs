//! Loader-side data types shared between `lean-rs` (runtime opener) and the
//! worker wire protocol (parent ↔ child serialisation).
//!
//! These types live in `lean-toolchain` because the worker-protocol crate needs
//! them on the wire and must not depend on `lean-rs` (which would re-link
//! `libleanshared` into every parent process). `lean-rs` re-exports them at
//! their historical paths (`lean_rs::module::*`) for source compatibility.

use std::path::{Path, PathBuf};

/// Stable preflight diagnostic codes for manifest-backed capability loading.
///
/// Single source of truth shared between the runtime preflight in `lean-rs`
/// and the wire payloads in the worker-protocol crate.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LeanLoaderDiagnosticCode {
    /// The manifest path was absent, unreadable, or pointed at a missing file.
    MissingManifest,
    /// The manifest was not valid JSON or missed required fields.
    MalformedManifest,
    /// The manifest schema version is newer or otherwise unsupported.
    UnsupportedManifestSchema,
    /// The manifest's primary capability dylib is missing.
    MissingPrimaryDylib,
    /// A dependency dylib named by the manifest is missing.
    MissingTransitiveDependency,
    /// A dylib could not be parsed as a native object for this platform.
    UnsupportedArchitecture,
    /// The manifest was produced by an unsupported or mismatched Lean toolchain.
    UnsupportedToolchainFingerprint,
    /// A manifest appears older than the build artifact it describes.
    StaleManifest,
    /// The root module initializer named by the manifest is not exported.
    MissingInitializer,
    /// A Lean/imported symbol is not supplied by the manifest dependency set.
    MissingImportedSymbol,
}

impl LeanLoaderDiagnosticCode {
    /// Stable string identifier suitable for logs and support reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingManifest => "lean_rs.loader.missing_manifest",
            Self::MalformedManifest => "lean_rs.loader.malformed_manifest",
            Self::UnsupportedManifestSchema => "lean_rs.loader.unsupported_manifest_schema",
            Self::MissingPrimaryDylib => "lean_rs.loader.missing_primary_dylib",
            Self::MissingTransitiveDependency => "lean_rs.loader.missing_transitive_dependency",
            Self::UnsupportedArchitecture => "lean_rs.loader.unsupported_architecture",
            Self::UnsupportedToolchainFingerprint => "lean_rs.loader.unsupported_toolchain_fingerprint",
            Self::StaleManifest => "lean_rs.loader.stale_manifest",
            Self::MissingInitializer => "lean_rs.loader.missing_initializer",
            Self::MissingImportedSymbol => "lean_rs.loader.missing_imported_symbol",
        }
    }
}

impl std::fmt::Display for LeanLoaderDiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Severity of one loader preflight finding.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LeanLoaderSeverity {
    /// Informational finding that does not block loading.
    Info,
    /// Suspicious state that may still load.
    Warning,
    /// The capability should not be opened until this is fixed.
    Error,
}

/// Maximum bytes preserved in user-facing loader diagnostic strings.
///
/// Matches the workspace error-message bound so diagnostics that flow back
/// through `LeanError` are not double-truncated.
pub const LOADER_DIAGNOSTIC_TEXT_LIMIT: usize = 4 * 1024;

/// Truncate `text` to at most [`LOADER_DIAGNOSTIC_TEXT_LIMIT`] bytes on a
/// UTF-8 char boundary.
#[must_use]
pub fn bound_loader_text(mut text: String) -> String {
    if text.len() <= LOADER_DIAGNOSTIC_TEXT_LIMIT {
        return text;
    }
    let mut cut = LOADER_DIAGNOSTIC_TEXT_LIMIT;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    text.truncate(cut);
    text
}

/// One bounded preflight finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanLoaderCheck {
    code: LeanLoaderDiagnosticCode,
    severity: LeanLoaderSeverity,
    subject: String,
    message: String,
    repair_hint: String,
}

impl LeanLoaderCheck {
    /// Construct an `Error` finding with bounded text fields.
    #[must_use]
    pub fn error(
        code: LeanLoaderDiagnosticCode,
        subject: impl Into<String>,
        message: impl Into<String>,
        repair_hint: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: LeanLoaderSeverity::Error,
            subject: bound_loader_text(subject.into()),
            message: bound_loader_text(message.into()),
            repair_hint: bound_loader_text(repair_hint.into()),
        }
    }

    /// Stable loader diagnostic code.
    #[must_use]
    pub fn code(&self) -> LeanLoaderDiagnosticCode {
        self.code
    }

    /// Whether this finding blocks capability loading.
    #[must_use]
    pub fn severity(&self) -> LeanLoaderSeverity {
        self.severity
    }

    /// Artifact, symbol, or manifest field this finding is about.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Bounded explanation of the failure.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Bounded repair hint for normal users.
    #[must_use]
    pub fn repair_hint(&self) -> &str {
        &self.repair_hint
    }
}

impl std::fmt::Display for LeanLoaderCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{:?}] {}: {} (repair: {})",
            self.code.as_str(),
            self.severity,
            self.subject,
            self.message,
            self.repair_hint
        )
    }
}

/// Structured result of loader preflight for one capability manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanLoaderReport {
    manifest_path: Option<PathBuf>,
    checks: Vec<LeanLoaderCheck>,
}

impl LeanLoaderReport {
    /// Bundle preflight findings with the manifest path they concern.
    #[must_use]
    pub fn new(manifest_path: Option<PathBuf>, checks: Vec<LeanLoaderCheck>) -> Self {
        Self { manifest_path, checks }
    }

    /// Manifest path checked, if the descriptor resolved one.
    #[must_use]
    pub fn manifest_path(&self) -> Option<&Path> {
        self.manifest_path.as_deref()
    }

    /// All preflight findings.
    #[must_use]
    pub fn checks(&self) -> &[LeanLoaderCheck] {
        &self.checks
    }

    /// Blocking findings only.
    pub fn errors(&self) -> impl Iterator<Item = &LeanLoaderCheck> {
        self.checks
            .iter()
            .filter(|check| check.severity == LeanLoaderSeverity::Error)
    }

    /// Whether preflight found no blocking findings.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors().next().is_none()
    }

    /// First blocking finding, if any.
    #[must_use]
    pub fn first_error(&self) -> Option<&LeanLoaderCheck> {
        self.errors().next()
    }

    /// Consume the report and return its findings.
    #[must_use]
    pub fn into_checks(self) -> Vec<LeanLoaderCheck> {
        self.checks
    }
}

/// Initializer for a Lean module hosted by a loaded dylib.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanModuleInitializer {
    package: String,
    module: String,
}

impl LeanModuleInitializer {
    /// Create an initializer descriptor from Lake package and root module names.
    #[must_use]
    pub fn new(package: impl Into<String>, module: impl Into<String>) -> Self {
        Self {
            package: package.into(),
            module: module.into(),
        }
    }

    /// Lake package name used by the initializer.
    #[must_use]
    pub fn package_name(&self) -> &str {
        &self.package
    }

    /// Root Lean module name used by the initializer.
    #[must_use]
    pub fn module_name(&self) -> &str {
        &self.module
    }
}

/// Dependency dylib that must stay alive while a capability is loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanLibraryDependency {
    path: PathBuf,
    exports_symbols_for_dependents: bool,
    initializer: Option<LeanModuleInitializer>,
}

impl LeanLibraryDependency {
    /// Add a dependency dylib to the bundle.
    #[must_use]
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            exports_symbols_for_dependents: false,
            initializer: None,
        }
    }

    /// Make this dependency's Lean symbols available to later dylibs in the
    /// same bundle.
    ///
    /// This is a capability-level requirement, not a platform-loader flag in
    /// the public contract. On ELF platforms it maps to global symbol
    /// visibility; other platforms use the equivalent behavior provided by the
    /// native loader.
    #[must_use]
    pub fn export_symbols_for_dependents(mut self) -> Self {
        self.exports_symbols_for_dependents = true;
        self
    }

    /// Initialize a module from this dependency after it is opened.
    #[must_use]
    pub fn initializer(mut self, package: impl Into<String>, module: impl Into<String>) -> Self {
        self.initializer = Some(LeanModuleInitializer::new(package, module));
        self
    }

    /// On-disk path to the dependency dylib.
    #[must_use]
    pub fn path_ref(&self) -> &Path {
        &self.path
    }

    /// Whether symbols from this dependency are exported to later bundle
    /// members.
    #[must_use]
    pub fn exports_symbols_for_dependents(&self) -> bool {
        self.exports_symbols_for_dependents
    }

    /// Optional module initializer for this dependency.
    #[must_use]
    pub fn module_initializer(&self) -> Option<&LeanModuleInitializer> {
        self.initializer.as_ref()
    }

    /// Consume the dependency and return its module initializer, if any.
    ///
    /// Used by the runtime opener (`lean-rs`) to take owned ownership of the
    /// initializer when opening the bundle, without re-cloning the strings.
    #[must_use]
    pub fn into_module_initializer(self) -> Option<LeanModuleInitializer> {
        self.initializer
    }
}
