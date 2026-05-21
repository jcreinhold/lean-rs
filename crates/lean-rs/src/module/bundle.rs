//! Capability-level loader that anchors dependent Lean libraries.
//!
//! [`LeanLibrary`] is intentionally low-level: it owns one dynamic-loader
//! handle. [`LeanLibraryBundle`] is the shipped-capability boundary. It opens
//! dependency dylibs first, gives them exported symbol visibility when a later
//! Lean dylib needs it, initializes dependency modules when requested, and keeps
//! every handle alive until the primary capability is dropped.

use std::path::{Path, PathBuf};

use super::{LeanLibrary, LeanModule};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;

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
}

/// A primary Lean capability plus its dependent Lean dylibs.
///
/// The primary library is dropped before dependency libraries so imported
/// symbols remain available for as long as capability code can run.
pub struct LeanLibraryBundle<'lean> {
    primary: LeanLibrary<'lean>,
    dependencies: Vec<LeanLibrary<'lean>>,
}

impl<'lean> LeanLibraryBundle<'lean> {
    /// Open a primary capability dylib and every dependency it needs.
    ///
    /// Dependencies are opened and optionally initialized in iterator order.
    /// The primary dylib is opened last. Every dependency handle is stored
    /// inside the bundle, so helper functions can return a bundle without
    /// leaking handles or relying on platform loader state that outlives Rust's
    /// RAII values.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LeanError`] if any dependency or primary dylib cannot
    /// be opened, or if any requested dependency initializer fails.
    pub fn open(
        runtime: &'lean LeanRuntime,
        primary_path: impl AsRef<Path>,
        dependencies: impl IntoIterator<Item = LeanLibraryDependency>,
    ) -> LeanResult<Self> {
        let mut opened_dependencies = Vec::new();
        for dependency in dependencies {
            let library = if dependency.exports_symbols_for_dependents {
                LeanLibrary::open_globally(runtime, &dependency.path)?
            } else {
                LeanLibrary::open(runtime, &dependency.path)?
            };
            if let Some(initializer) = dependency.initializer {
                let _module = library.initialize_module(&initializer.package, &initializer.module)?;
            }
            opened_dependencies.push(library);
        }

        let primary = LeanLibrary::open(runtime, primary_path)?;
        Ok(Self {
            primary,
            dependencies: opened_dependencies,
        })
    }

    /// Initialize a module from the primary capability library.
    ///
    /// This delegates to [`LeanLibrary::initialize_module`] while preserving the
    /// bundle lifetime that keeps dependent dylibs anchored.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LeanError`] if the requested module name cannot be
    /// mapped to an initializer symbol, if that symbol is missing from the
    /// primary library, or if the Lean initializer returns `IO.error`.
    pub fn initialize_module<'bundle>(
        &'bundle self,
        package: &str,
        module: &str,
    ) -> LeanResult<LeanModule<'lean, 'bundle>> {
        self.primary.initialize_module(package, module)
    }

    /// Borrow the primary capability library.
    #[must_use]
    pub fn library(&self) -> &LeanLibrary<'lean> {
        &self.primary
    }

    /// Number of dependency dylibs anchored by this bundle.
    #[must_use]
    pub fn dependency_count(&self) -> usize {
        self.dependencies.len()
    }
}

impl std::fmt::Debug for LeanLibraryBundle<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeanLibraryBundle")
            .field("primary", &self.primary)
            .field("dependency_count", &self.dependencies.len())
            .finish()
    }
}
