//! Build-script paired capability opener.
//!
//! This module is the runtime half of
//! [`lean_toolchain::CargoLeanCapability`]. It lets shipped crates open the
//! dylib path their `build.rs` embedded without repeating environment-path
//! lookup and module-initialization names at every call site.

use std::path::{Path, PathBuf};

use super::{LeanLibrary, LeanModule};
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;

/// Runtime descriptor for a Lean capability built by a downstream `build.rs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanBuiltCapability {
    dylib_path: Option<PathBuf>,
    env_var: Option<String>,
    package: Option<String>,
    module: Option<String>,
}

impl LeanBuiltCapability {
    /// Build a descriptor from an embedded dylib path.
    ///
    /// This is the canonical form for shipped binaries:
    ///
    /// ```ignore
    /// let spec = lean_rs::LeanBuiltCapability::path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB"))
    ///     .package("my_app")
    ///     .module("MyCapability");
    /// ```
    #[must_use]
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            dylib_path: Some(path.into()),
            env_var: None,
            package: None,
            module: None,
        }
    }

    /// Build a descriptor that resolves the dylib path from a runtime
    /// environment variable.
    ///
    /// Prefer [`Self::path`] with Rust's `env!` macro for redistributable
    /// binaries. Runtime environment lookup is useful for tests, local
    /// overrides, and launcher-managed deployments.
    #[must_use]
    pub fn env(env_var: impl Into<String>) -> Self {
        Self {
            dylib_path: None,
            env_var: Some(env_var.into()),
            package: None,
            module: None,
        }
    }

    /// Preserve the Cargo environment variable name for diagnostics.
    #[must_use]
    pub fn env_var(mut self, env_var: impl Into<String>) -> Self {
        self.env_var = Some(env_var.into());
        self
    }

    /// Set the Lake package name used by the Lean initializer.
    #[must_use]
    pub fn package(mut self, package: impl Into<String>) -> Self {
        self.package = Some(package.into());
        self
    }

    /// Set the root Lean module name initialized by Rust.
    #[must_use]
    pub fn module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Return the configured package name.
    #[must_use]
    pub fn package_name(&self) -> Option<&str> {
        self.package.as_deref()
    }

    /// Return the configured module name.
    #[must_use]
    pub fn module_name(&self) -> Option<&str> {
        self.module.as_deref()
    }

    /// Resolve the capability dylib path.
    ///
    /// # Errors
    ///
    /// Returns a host module-initialization error if neither a path nor a
    /// readable environment variable is configured.
    pub fn dylib_path(&self) -> LeanResult<PathBuf> {
        if let Some(path) = &self.dylib_path {
            return Ok(path.clone());
        }
        let env_var = self.env_var.as_deref().ok_or_else(|| {
            LeanError::module_init("LeanBuiltCapability needs either a dylib path or an environment variable")
        })?;
        std::env::var_os(env_var).map(PathBuf::from).ok_or_else(|| {
            LeanError::module_init(format!(
                "environment variable {env_var} is not set for Lean capability dylib"
            ))
        })
    }
}

impl From<&lean_toolchain::BuiltLeanCapability> for LeanBuiltCapability {
    fn from(value: &lean_toolchain::BuiltLeanCapability) -> Self {
        Self::path(value.dylib_path())
            .env_var(value.env_var())
            .package(value.package())
            .module(value.module())
    }
}

/// Opened Lean capability whose dylib path and initializer names came from
/// the build-script pairing.
pub struct LeanCapability<'lean> {
    library: LeanLibrary<'lean>,
    package: String,
    module: String,
}

impl<'lean> LeanCapability<'lean> {
    /// Open and initialize a build-script produced Lean capability.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] when the dylib path cannot be resolved, the
    /// dynamic loader cannot open it, or the configured module initializer
    /// fails.
    pub fn from_build_env(runtime: &'lean LeanRuntime, mut spec: LeanBuiltCapability) -> LeanResult<Self> {
        let dylib_path = spec.dylib_path()?;
        let package = spec.package.take().ok_or_else(|| {
            LeanError::linking("LeanBuiltCapability is missing the Lake package name; call `.package(...)`")
        })?;
        let module = spec.module.take().ok_or_else(|| {
            LeanError::linking("LeanBuiltCapability is missing the root Lean module name; call `.module(...)`")
        })?;
        Self::open(runtime, dylib_path, package, module)
    }

    /// Open and initialize a capability from an explicit dylib path and
    /// initializer names.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] when the dynamic loader cannot open the dylib or
    /// the configured module initializer fails.
    pub fn open(
        runtime: &'lean LeanRuntime,
        dylib_path: impl AsRef<Path>,
        package: impl Into<String>,
        module: impl Into<String>,
    ) -> LeanResult<Self> {
        let package = package.into();
        let module = module.into();
        let library = LeanLibrary::open(runtime, dylib_path)?;
        let _module = library.initialize_module(&package, &module)?;
        Ok(Self {
            library,
            package,
            module,
        })
    }

    /// Return an initialized module handle.
    ///
    /// Lean module initializers are idempotent, so obtaining the handle after
    /// construction is cheap and safe.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] if the module initializer unexpectedly fails when
    /// invoked again.
    pub fn module(&self) -> LeanResult<LeanModule<'lean, '_>> {
        self.library.initialize_module(&self.package, &self.module)
    }

    /// Borrow the underlying library for advanced symbol access.
    #[must_use]
    pub fn library(&self) -> &LeanLibrary<'lean> {
        &self.library
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

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::LeanBuiltCapability;

    #[test]
    fn built_capability_path_is_resolved_without_runtime_env() {
        let spec = LeanBuiltCapability::path("/tmp/libcap.so")
            .env_var("LEAN_RS_CAPABILITY_CAP_DYLIB")
            .package("pkg")
            .module("Cap");

        let path = match spec.dylib_path() {
            Ok(path) => path,
            Err(err) => panic!("expected path, got {err}"),
        };
        assert_eq!(path, std::path::PathBuf::from("/tmp/libcap.so"));
        assert_eq!(spec.package_name(), Some("pkg"));
        assert_eq!(spec.module_name(), Some("Cap"));
    }

    #[test]
    fn missing_runtime_env_is_typed() {
        let spec = LeanBuiltCapability::env("LEAN_RS_TEST_MISSING_CAPABILITY_DYLIB")
            .package("pkg")
            .module("Cap");
        let err = match spec.dylib_path() {
            Ok(path) => panic!("expected missing env error, got {}", path.display()),
            Err(err) => err,
        };
        assert_eq!(err.code(), crate::LeanDiagnosticCode::ModuleInit);
    }
}
