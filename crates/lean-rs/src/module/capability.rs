//! Build-script paired capability opener.
//!
//! This module is the runtime half of
//! [`lean_toolchain::CargoLeanCapability`]. It lets shipped crates open the
//! dylib path their `build.rs` embedded without repeating environment-path
//! lookup and module-initialization names at every call site.
//!
//! The [`LeanBuiltCapability`] descriptor itself is link-free and lives in
//! `lean-toolchain` so the worker parent crate can consume it without
//! relinking `libleanshared`. It is re-exported here for source compatibility.

use std::path::Path;

use super::preflight::{CapabilityManifest, LeanRuntimePreflight, manifest_error_to_lean_error, report_into_error};
use super::{LeanLibrary, LeanLibraryBundle, LeanLibraryDependency, LeanModule};
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;

// Build-script descriptor lives in `lean-toolchain` (below `lean-rs`) so the
// worker parent crate can construct and consume it without relinking
// `libleanshared`. Re-exported here for the historical `lean_rs::LeanBuiltCapability`
// path.
pub use lean_toolchain::{BuiltCapabilityArtifact, LeanBuiltCapability, LeanBuiltCapabilityError};

fn built_capability_error_to_lean_error(err: &LeanBuiltCapabilityError) -> LeanError {
    LeanError::module_init(err.to_string())
}

/// Opened Lean capability whose dylib path and initializer names came from
/// the build-script pairing.
pub struct LeanCapability<'lean> {
    bundle: LeanLibraryBundle<'lean>,
    package: String,
    module: String,
}

impl<'lean> LeanCapability<'lean> {
    /// Open and initialize a build-script produced Lean capability from its
    /// JSON artifact manifest.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] when the manifest path cannot be resolved, the
    /// manifest is missing, malformed, or unsupported, or the bundle described
    /// by the manifest cannot be opened.
    #[allow(clippy::needless_pass_by_value)]
    pub fn from_build_manifest(runtime: &'lean LeanRuntime, spec: LeanBuiltCapability) -> LeanResult<Self> {
        let report = LeanRuntimePreflight::new(spec.clone()).check();
        if !report.is_ok() {
            return Err(report_into_error(report));
        }
        let manifest_path = spec
            .resolved_manifest_path()
            .map_err(|err| built_capability_error_to_lean_error(&err))?;
        let manifest = CapabilityManifest::read(&manifest_path).map_err(manifest_error_to_lean_error)?;
        Self::open_with_dependencies(
            runtime,
            manifest.primary_dylib,
            manifest.package,
            manifest.module,
            manifest.dependencies,
        )
    }

    /// Open and initialize a build-script produced Lean capability from a
    /// direct dylib path.
    ///
    /// This compatibility path cannot carry dependency ordering by itself.
    /// Prefer [`Self::from_build_manifest`] for shipped crates.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] when the dylib path cannot be resolved, the
    /// dynamic loader cannot open it, or the configured module initializer
    /// fails.
    pub fn from_build_env(runtime: &'lean LeanRuntime, mut spec: LeanBuiltCapability) -> LeanResult<Self> {
        let dylib_path = spec
            .dylib_path()
            .map_err(|err| built_capability_error_to_lean_error(&err))?;
        let package = spec.take_package_name().ok_or_else(|| {
            LeanError::linking("LeanBuiltCapability is missing the Lake package name; call `.package(...)`")
        })?;
        let module = spec.take_module_name().ok_or_else(|| {
            LeanError::linking("LeanBuiltCapability is missing the root Lean module name; call `.module(...)`")
        })?;
        let dependencies = spec.take_dependencies();
        Self::open_with_dependencies(runtime, dylib_path, package, module, dependencies)
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
        Self::open_with_dependencies(runtime, dylib_path, package, module, [])
    }

    /// Open and initialize a capability with explicitly described dependency
    /// dylibs.
    ///
    /// This is the runtime form artifact manifests feed. Use
    /// [`LeanCapability::from_build_manifest`] for shipped crates when
    /// build-script metadata is available.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] when a dependency or primary dylib cannot be
    /// loaded, or when a dependency or primary module initializer fails.
    pub fn open_with_dependencies(
        runtime: &'lean LeanRuntime,
        dylib_path: impl AsRef<Path>,
        package: impl Into<String>,
        module: impl Into<String>,
        dependencies: impl IntoIterator<Item = LeanLibraryDependency>,
    ) -> LeanResult<Self> {
        let package = package.into();
        let module = module.into();
        let bundle = LeanLibraryBundle::open(runtime, dylib_path, dependencies)?;
        let _module = bundle.initialize_module(&package, &module)?;
        Ok(Self {
            bundle,
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
        self.bundle.initialize_module(&self.package, &self.module)
    }

    /// Borrow the underlying library for advanced symbol access.
    #[must_use]
    pub fn library(&self) -> &LeanLibrary<'lean> {
        self.bundle.library()
    }

    /// Borrow the bundle that anchors this capability and its dependencies.
    #[must_use]
    pub fn bundle(&self) -> &LeanLibraryBundle<'lean> {
        &self.bundle
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
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        BuiltCapabilityArtifact, CapabilityManifest, LeanBuiltCapability, LeanBuiltCapabilityError,
        LeanLibraryDependency,
    };
    use std::fs;
    use std::path::PathBuf;

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
        let spec = LeanBuiltCapability::manifest_env("LEAN_RS_TEST_MISSING_CAPABILITY_MANIFEST");
        let err = match spec.resolved_manifest_path() {
            Ok(path) => panic!("expected missing manifest env error, got {}", path.display()),
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
    fn manifest_descriptor_parses_dependencies() {
        let path = temp_manifest_path("manifest_descriptor_parses_dependencies");
        write_manifest(
            &path,
            r#"{
  "schema_version": 1,
  "target_name": "Cap",
  "package": "pkg",
  "module": "Cap",
  "primary_dylib": "/tmp/libcap.so",
  "dependencies": [
    {
      "dylib_path": "/tmp/libdep.so",
      "export_symbols_for_dependents": true,
      "initializer": { "package": "dep_pkg", "module": "Dep" }
    }
  ]
}"#,
        );

        let manifest = match CapabilityManifest::read(&path) {
            Ok(manifest) => manifest,
            Err(err) => panic!("expected manifest to parse, got {err}"),
        };
        assert_eq!(manifest.primary_dylib, PathBuf::from("/tmp/libcap.so"));
        assert_eq!(manifest.package, "pkg");
        assert_eq!(manifest.module, "Cap");
        assert_eq!(manifest.dependencies.len(), 1);
        let Some(dependency) = manifest.dependencies.first() else {
            panic!("expected one dependency");
        };
        assert!(dependency.exports_symbols_for_dependents());
        assert_eq!(dependency.path_ref(), std::path::Path::new("/tmp/libdep.so"));
        let Some(initializer) = dependency.module_initializer() else {
            panic!("expected dependency initializer");
        };
        assert_eq!(initializer.package_name(), "dep_pkg");
        assert_eq!(initializer.module_name(), "Dep");
    }

    #[test]
    fn unsupported_manifest_schema_is_typed() {
        let path = temp_manifest_path("unsupported_manifest_schema_is_typed");
        write_manifest(
            &path,
            r#"{
  "schema_version": 999,
  "package": "pkg",
  "module": "Cap",
  "primary_dylib": "/tmp/libcap.so"
}"#,
        );

        let Err(err) = CapabilityManifest::read(&path) else {
            panic!("expected unsupported schema error");
        };
        assert_eq!(err.code(), crate::LeanLoaderDiagnosticCode::UnsupportedManifestSchema);
        assert!(err.message().contains("unsupported Lean capability manifest schema"));
    }

    #[test]
    fn built_capability_records_dependency_descriptors() {
        let spec = LeanBuiltCapability::path("/tmp/libcap.so").dependency(
            LeanLibraryDependency::path("/tmp/libdep.so")
                .export_symbols_for_dependents()
                .initializer("dep_pkg", "Dep"),
        );

        let dependencies = spec.dependency_descriptors();
        assert_eq!(dependencies.len(), 1);
        let Some(dependency) = dependencies.first() else {
            panic!("expected one dependency descriptor");
        };
        assert!(dependency.exports_symbols_for_dependents());
        let Some(initializer) = dependency.module_initializer() else {
            panic!("dependency initializer is recorded");
        };
        assert_eq!(initializer.package_name(), "dep_pkg");
        assert_eq!(initializer.module_name(), "Dep");
    }

    fn temp_manifest_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lean-rs-manifest-{}-{name}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("create manifest test dir");
        dir.join("capability.json")
    }

    fn write_manifest(path: &std::path::Path, contents: &str) {
        fs::write(path, contents).expect("write manifest fixture");
    }
}
