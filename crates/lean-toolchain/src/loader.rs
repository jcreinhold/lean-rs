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
