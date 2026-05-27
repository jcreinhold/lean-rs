//! Runtime descriptor for a Lean capability built by a downstream `build.rs`.
//!
//! The descriptor carries the identity of a Lean capability the consumer's
//! build script embedded—dylib path or manifest path (either literal or via
//! environment variable), Lake package and module names, and any dependent
//! dylibs. It is pure data: it does not link `libleanshared`, so the worker
//! parent crate can consume it without dragging the Lean runtime into its
//! link graph.
//!
//! The runtime opener that turns a descriptor into a loaded capability lives
//! in `lean-rs` (`LeanCapability`); the corresponding preflight runner that
//! inspects exported symbols lives in `lean_rs::module::preflight`. Both keep
//! the descriptor here as their input.
//!
//! Source-compat: `lean-rs` re-exports [`LeanBuiltCapability`] and
//! [`LeanBuiltCapabilityError`] at their historical paths.

use std::path::PathBuf;

use crate::loader::LeanLibraryDependency;

/// Runtime descriptor for a Lean capability built by a downstream `build.rs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanBuiltCapability {
    dylib_path: Option<PathBuf>,
    env_var: Option<String>,
    manifest_path: Option<PathBuf>,
    manifest_env_var: Option<String>,
    package: Option<String>,
    module: Option<String>,
    dependencies: Vec<LeanLibraryDependency>,
}

impl LeanBuiltCapability {
    /// Build a descriptor from an embedded dylib path.
    ///
    /// This remains supported for simple or compatibility cases. Prefer
    /// [`Self::manifest_path`] for shipped binaries because the manifest also
    /// carries dependency and loader-order facts:
    ///
    /// ```ignore
    /// let spec = lean_toolchain::LeanBuiltCapability::path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB"))
    ///     .package("my_app")
    ///     .module("MyCapability");
    /// ```
    #[must_use]
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            dylib_path: Some(path.into()),
            env_var: None,
            manifest_path: None,
            manifest_env_var: None,
            package: None,
            module: None,
            dependencies: Vec::new(),
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
            manifest_path: None,
            manifest_env_var: None,
            package: None,
            module: None,
            dependencies: Vec::new(),
        }
    }

    /// Build a descriptor from an embedded artifact manifest path.
    ///
    /// This is the canonical form for shipped binaries using
    /// `CargoLeanCapability`'s manifest output:
    ///
    /// ```ignore
    /// let spec = lean_toolchain::LeanBuiltCapability::manifest_path(
    ///     env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST"),
    /// );
    /// ```
    #[must_use]
    pub fn manifest_path(path: impl Into<PathBuf>) -> Self {
        Self {
            dylib_path: None,
            env_var: None,
            manifest_path: Some(path.into()),
            manifest_env_var: None,
            package: None,
            module: None,
            dependencies: Vec::new(),
        }
    }

    /// Build a descriptor that resolves the artifact manifest path from a
    /// runtime environment variable.
    ///
    /// Prefer [`Self::manifest_path`] with Rust's `env!` macro for
    /// redistributable binaries. Runtime environment lookup is useful for
    /// tests, local overrides, and launcher-managed deployments.
    #[must_use]
    pub fn manifest_env(env_var: impl Into<String>) -> Self {
        Self {
            dylib_path: None,
            env_var: None,
            manifest_path: None,
            manifest_env_var: Some(env_var.into()),
            package: None,
            module: None,
            dependencies: Vec::new(),
        }
    }

    /// Preserve the Cargo environment variable name for diagnostics.
    #[must_use]
    pub fn env_var(mut self, env_var: impl Into<String>) -> Self {
        self.env_var = Some(env_var.into());
        self
    }

    /// Preserve the Cargo manifest environment variable name for diagnostics.
    #[must_use]
    pub fn manifest_env_var(mut self, env_var: impl Into<String>) -> Self {
        self.manifest_env_var = Some(env_var.into());
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

    /// Add a dependent Lean dylib that must stay alive with this capability.
    #[must_use]
    pub fn dependency(mut self, dependency: LeanLibraryDependency) -> Self {
        self.dependencies.push(dependency);
        self
    }

    /// Add multiple dependent Lean dylibs that must stay alive with this
    /// capability.
    #[must_use]
    pub fn dependencies(mut self, dependencies: impl IntoIterator<Item = LeanLibraryDependency>) -> Self {
        self.dependencies.extend(dependencies);
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

    /// Take the configured package name, leaving `None` behind.
    pub fn take_package_name(&mut self) -> Option<String> {
        self.package.take()
    }

    /// Take the configured module name, leaving `None` behind.
    pub fn take_module_name(&mut self) -> Option<String> {
        self.module.take()
    }

    /// Take the recorded dependency descriptors, leaving an empty list.
    pub fn take_dependencies(&mut self) -> Vec<LeanLibraryDependency> {
        std::mem::take(&mut self.dependencies)
    }

    /// Dependency dylibs that will be opened before the primary capability.
    #[must_use]
    pub fn dependency_descriptors(&self) -> &[LeanLibraryDependency] {
        &self.dependencies
    }

    /// Resolve the capability dylib path.
    ///
    /// # Errors
    ///
    /// Returns [`LeanBuiltCapabilityError::MissingDylibSource`] if no dylib
    /// path or environment variable is configured, or
    /// [`LeanBuiltCapabilityError::EnvVarNotSet`] if the configured environment
    /// variable is not present at runtime.
    pub fn dylib_path(&self) -> Result<PathBuf, LeanBuiltCapabilityError> {
        if let Some(path) = &self.dylib_path {
            return Ok(path.clone());
        }
        let env_var = self
            .env_var
            .as_deref()
            .ok_or(LeanBuiltCapabilityError::MissingDylibSource)?;
        std::env::var_os(env_var)
            .map(PathBuf::from)
            .ok_or_else(|| LeanBuiltCapabilityError::EnvVarNotSet {
                env_var: env_var.to_owned(),
                kind: BuiltCapabilityArtifact::Dylib,
            })
    }

    /// Resolve the build artifact manifest path.
    ///
    /// # Errors
    ///
    /// Returns [`LeanBuiltCapabilityError::MissingManifestSource`] if no
    /// manifest path or manifest environment variable is configured, or
    /// [`LeanBuiltCapabilityError::EnvVarNotSet`] if the configured environment
    /// variable is not present at runtime.
    pub fn resolved_manifest_path(&self) -> Result<PathBuf, LeanBuiltCapabilityError> {
        if let Some(path) = &self.manifest_path {
            return Ok(path.clone());
        }
        let env_var = self
            .manifest_env_var
            .as_deref()
            .ok_or(LeanBuiltCapabilityError::MissingManifestSource)?;
        std::env::var_os(env_var)
            .map(PathBuf::from)
            .ok_or_else(|| LeanBuiltCapabilityError::EnvVarNotSet {
                env_var: env_var.to_owned(),
                kind: BuiltCapabilityArtifact::Manifest,
            })
    }
}

impl From<&crate::build_helpers::BuiltLeanCapability> for LeanBuiltCapability {
    fn from(value: &crate::build_helpers::BuiltLeanCapability) -> Self {
        Self {
            dylib_path: Some(value.dylib_path().to_path_buf()),
            env_var: Some(value.env_var().to_owned()),
            manifest_path: Some(value.manifest_path().to_path_buf()),
            manifest_env_var: Some(value.manifest_env_var().to_owned()),
            package: Some(value.package().to_owned()),
            module: Some(value.module().to_owned()),
            dependencies: Vec::new(),
        }
    }
}

/// Which artifact a [`LeanBuiltCapability`] resolution was looking for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltCapabilityArtifact {
    /// The capability's primary dylib.
    Dylib,
    /// The capability's build artifact manifest.
    Manifest,
}

impl BuiltCapabilityArtifact {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dylib => "Lean capability dylib",
            Self::Manifest => "Lean capability manifest",
        }
    }
}

/// Errors returned when resolving a [`LeanBuiltCapability`] descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeanBuiltCapabilityError {
    /// The descriptor has no dylib path and no dylib environment variable.
    MissingDylibSource,
    /// The descriptor has no manifest path and no manifest environment variable.
    MissingManifestSource,
    /// The configured environment variable is not set at runtime.
    EnvVarNotSet {
        /// Name of the environment variable that was queried.
        env_var: String,
        /// Which artifact the environment variable was supposed to point at.
        kind: BuiltCapabilityArtifact,
    },
}

impl std::fmt::Display for LeanBuiltCapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDylibSource => {
                f.write_str("LeanBuiltCapability needs either a dylib path or an environment variable")
            }
            Self::MissingManifestSource => {
                f.write_str("LeanBuiltCapability needs either a manifest path or manifest environment variable")
            }
            Self::EnvVarNotSet { env_var, kind } => {
                write!(f, "environment variable {env_var} is not set for {}", kind.as_str())
            }
        }
    }
}

impl std::error::Error for LeanBuiltCapabilityError {}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::{BuiltCapabilityArtifact, LeanBuiltCapability, LeanBuiltCapabilityError};
    use std::path::PathBuf;

    #[test]
    fn path_descriptor_resolves_without_runtime_env() {
        let spec = LeanBuiltCapability::path("/tmp/libcap.so")
            .env_var("LEAN_RS_CAPABILITY_CAP_DYLIB")
            .package("pkg")
            .module("Cap");

        let path = match spec.dylib_path() {
            Ok(path) => path,
            Err(err) => panic!("expected path, got {err}"),
        };
        assert_eq!(path, PathBuf::from("/tmp/libcap.so"));
        assert_eq!(spec.package_name(), Some("pkg"));
        assert_eq!(spec.module_name(), Some("Cap"));
    }

    #[test]
    fn missing_runtime_env_is_typed() {
        let spec = LeanBuiltCapability::env("LEAN_TC_TEST_MISSING_CAPABILITY_DYLIB")
            .package("pkg")
            .module("Cap");
        let err = match spec.dylib_path() {
            Ok(path) => panic!("expected missing env error, got {}", path.display()),
            Err(err) => err,
        };
        assert!(matches!(
            err,
            LeanBuiltCapabilityError::EnvVarNotSet {
                kind: BuiltCapabilityArtifact::Dylib,
                ..
            }
        ));
    }

    #[test]
    fn missing_runtime_manifest_env_is_typed() {
        let spec = LeanBuiltCapability::manifest_env("LEAN_TC_TEST_MISSING_CAPABILITY_MANIFEST");
        let err = match spec.resolved_manifest_path() {
            Ok(path) => panic!("expected missing env error, got {}", path.display()),
            Err(err) => err,
        };
        assert!(matches!(
            err,
            LeanBuiltCapabilityError::EnvVarNotSet {
                kind: BuiltCapabilityArtifact::Manifest,
                ..
            }
        ));
    }

    #[test]
    fn missing_dylib_source_is_typed() {
        let spec = LeanBuiltCapability::manifest_path("/tmp/manifest.json");
        let err = match spec.dylib_path() {
            Ok(path) => panic!("expected missing dylib source error, got {}", path.display()),
            Err(err) => err,
        };
        assert_eq!(err, LeanBuiltCapabilityError::MissingDylibSource);
    }

    #[test]
    fn missing_manifest_source_is_typed() {
        let spec = LeanBuiltCapability::path("/tmp/libcap.so").package("pkg").module("Cap");
        let err = match spec.resolved_manifest_path() {
            Ok(path) => panic!("expected missing manifest source error, got {}", path.display()),
            Err(err) => err,
        };
        assert_eq!(err, LeanBuiltCapabilityError::MissingManifestSource);
    }
}
