//! Capability-level loader that anchors dependent Lean libraries.
//!
//! [`LeanLibrary`] is intentionally low-level: it owns one dynamic-loader
//! handle. [`LeanLibraryBundle`] is the shipped-capability boundary. It opens
//! dependency dylibs first, gives them exported symbol visibility when a later
//! Lean dylib needs it, initializes dependency modules when requested, and keeps
//! every handle alive until the primary capability is dropped.

use std::path::Path;

use super::{LeanLibrary, LeanModule};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;

// `LeanLibraryDependency` and `LeanModuleInitializer` are pure data shared
// with the worker wire protocol; they live in `lean-toolchain` (below
// `lean-rs`) so the protocol crate can reference them without re-linking
// `libleanshared`. Re-exported here for callers using the historical paths.
pub use lean_toolchain::{LeanLibraryDependency, LeanModuleInitializer};

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
            let library = if dependency.exports_symbols_for_dependents() {
                LeanLibrary::open_globally(runtime, dependency.path_ref())?
            } else {
                LeanLibrary::open(runtime, dependency.path_ref())?
            };
            if let Some(initializer) = dependency.into_module_initializer() {
                let _module = library.initialize_module(initializer.package_name(), initializer.module_name())?;
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
