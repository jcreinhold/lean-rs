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

// Native loading and generic binding require caller-established artifact and
// value contracts. Manifest slot/kind checks are additional diagnostics; they
// cannot establish semantic trust for native code.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;

use super::preflight::{CapabilityManifest, LeanRuntimePreflight, manifest_error_to_lean_error, report_into_error};
use super::{
    DecodeCallResult, LeanArgs, LeanExported, LeanLibrary, LeanLibraryBundle, LeanLibraryDependency, LeanModule,
};
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use lean_toolchain::{LeanExportSignature, LeanExportSymbolKind};

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
    export_signatures: HashMap<String, LeanExportSignature>,
}

/// Typed failure from manifest-backed checked export lookup.
#[derive(Debug)]
pub enum LeanCheckedExportError {
    /// The manifest has no trusted signature entry for the requested symbol.
    MissingSignatureMetadata { symbol: String },
    /// The manifest signature does not match the requested Rust call shape.
    SignatureMismatch {
        symbol: String,
        expected: Box<LeanExportSignature>,
        manifest: Box<LeanExportSignature>,
    },
    /// The manifest's function/global classification does not match the dylib.
    SymbolKindMismatch {
        symbol: String,
        manifest: LeanExportSymbolKind,
        actual: LeanExportSymbolKind,
    },
    /// The manifest entry exists, but the symbol is absent from the loaded library.
    MissingSymbol { symbol: String, source: LeanError },
    /// The initialized module could not be reopened before lookup.
    Module(LeanError),
}

impl std::fmt::Display for LeanCheckedExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSignatureMetadata { symbol } => {
                write!(f, "missing trusted export signature metadata for symbol '{symbol}'")
            }
            Self::SignatureMismatch {
                symbol,
                expected,
                manifest,
            } => write!(
                f,
                "export signature mismatch for symbol '{symbol}': requested {expected:?}, manifest has {manifest:?}"
            ),
            Self::SymbolKindMismatch {
                symbol,
                manifest,
                actual,
            } => write!(
                f,
                "export symbol kind mismatch for symbol '{symbol}': manifest has {manifest:?}, dylib has {actual:?}"
            ),
            Self::MissingSymbol { symbol, source } => {
                write!(
                    f,
                    "manifest-backed export symbol '{symbol}' is missing from the loaded library: {source}"
                )
            }
            Self::Module(err) => write!(f, "failed to initialize module before checked export lookup: {err}"),
        }
    }
}

impl std::error::Error for LeanCheckedExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MissingSymbol { source, .. } | Self::Module(source) => Some(source),
            Self::MissingSignatureMetadata { .. }
            | Self::SignatureMismatch { .. }
            | Self::SymbolKindMismatch { .. } => None,
        }
    }
}

impl<'lean> LeanCapability<'lean> {
    pub(crate) fn pin_for_process(&self) {
        self.bundle.pin_for_process();
    }

    /// Open and initialize a build-script produced Lean capability from its
    /// JSON artifact manifest.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] when the manifest path cannot be resolved, the
    /// manifest is missing, malformed, or unsupported, or the bundle described
    /// by the manifest cannot be opened.
    #[allow(clippy::needless_pass_by_value)]
    /// # Safety
    ///
    /// The actual primary artifact and all transitive dependencies must satisfy
    /// [`LeanLibrary::open`]'s native-code, runtime, initializer, and unload
    /// requirements. Manifest metadata and paths do not establish native trust.
    pub unsafe fn from_build_manifest(runtime: &'lean LeanRuntime, spec: LeanBuiltCapability) -> LeanResult<Self> {
        let report = LeanRuntimePreflight::new(spec.clone()).check();
        if !report.is_ok() {
            return Err(report_into_error(report));
        }
        let manifest_path = spec
            .resolved_manifest_path()
            .map_err(|err| built_capability_error_to_lean_error(&err))?;
        let manifest = CapabilityManifest::read(&manifest_path).map_err(manifest_error_to_lean_error)?;
        // SAFETY: the caller established native trust for the primary artifact and dependencies.
        unsafe {
            Self::open_with_dependencies_and_exports(
                runtime,
                manifest.primary_dylib,
                manifest.package,
                manifest.module,
                manifest.dependencies,
                manifest.exports,
            )
        }
    }

    /// Open and initialize exactly one captured capability-manifest snapshot.
    ///
    /// Unlike [`Self::from_build_manifest`], this method never rereads the
    /// manifest path. Schema, fingerprint and symbol preflight inspect the same
    /// parsed bytes that provide library paths and checked ABI signatures.
    /// Artifact coordination must span this call, including all initializers;
    /// a captured manifest is storage metadata, not evidence of a loaded ABI.
    ///
    /// # Errors
    /// Returns [`LeanError`] for malformed metadata, preflight failure, or a
    /// dependency/primary load or module-initialization failure.
    /// # Safety
    ///
    /// The actual primary artifact and all transitive dependencies must satisfy
    /// [`LeanLibrary::open`]'s native-code, runtime, initializer, and unload
    /// requirements. Manifest metadata and paths do not establish native trust.
    pub unsafe fn from_manifest_snapshot(
        runtime: &'lean LeanRuntime,
        manifest_path: &Path,
        bytes: &[u8],
    ) -> LeanResult<Self> {
        let manifest = CapabilityManifest::from_bytes(manifest_path, bytes).map_err(manifest_error_to_lean_error)?;
        let report = LeanRuntimePreflight::check_manifest(manifest_path, &manifest);
        if !report.is_ok() {
            return Err(report_into_error(report));
        }
        // SAFETY: the caller established native trust for the primary artifact and dependencies.
        unsafe {
            Self::open_with_dependencies_and_exports(
                runtime,
                manifest.primary_dylib,
                manifest.package,
                manifest.module,
                manifest.dependencies,
                manifest.exports,
            )
        }
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
    /// # Safety
    ///
    /// The actual primary artifact and all transitive dependencies must satisfy
    /// [`LeanLibrary::open`]'s native-code, runtime, initializer, and unload
    /// requirements. Manifest metadata and paths do not establish native trust.
    pub unsafe fn from_build_env(runtime: &'lean LeanRuntime, mut spec: LeanBuiltCapability) -> LeanResult<Self> {
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
        // SAFETY: the caller established native trust for the primary artifact and dependencies.
        unsafe { Self::open_with_dependencies_and_exports(runtime, dylib_path, package, module, dependencies, []) }
    }

    /// Open and initialize a capability from an explicit dylib path and
    /// initializer names.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError`] when the dynamic loader cannot open the dylib or
    /// the configured module initializer fails.
    /// # Safety
    ///
    /// The actual primary artifact and all transitive dependencies must satisfy
    /// [`LeanLibrary::open`]'s native-code, runtime, initializer, and unload
    /// requirements. Manifest metadata and paths do not establish native trust.
    pub unsafe fn open(
        runtime: &'lean LeanRuntime,
        dylib_path: impl AsRef<Path>,
        package: impl Into<String>,
        module: impl Into<String>,
    ) -> LeanResult<Self> {
        let package = package.into();
        let module = module.into();
        // SAFETY: the caller established native trust for the primary artifact and dependencies.
        unsafe { Self::open_with_dependencies_and_exports(runtime, dylib_path, package, module, [], []) }
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
    /// # Safety
    ///
    /// The actual primary artifact and all transitive dependencies must satisfy
    /// [`LeanLibrary::open`]'s native-code, runtime, initializer, and unload
    /// requirements. Manifest metadata and paths do not establish native trust.
    pub unsafe fn open_with_dependencies(
        runtime: &'lean LeanRuntime,
        dylib_path: impl AsRef<Path>,
        package: impl Into<String>,
        module: impl Into<String>,
        dependencies: impl IntoIterator<Item = LeanLibraryDependency>,
    ) -> LeanResult<Self> {
        // SAFETY: the caller established native trust for the primary artifact and dependencies.
        unsafe { Self::open_with_dependencies_and_exports(runtime, dylib_path, package, module, dependencies, []) }
    }

    unsafe fn open_with_dependencies_and_exports(
        runtime: &'lean LeanRuntime,
        dylib_path: impl AsRef<Path>,
        package: impl Into<String>,
        module: impl Into<String>,
        dependencies: impl IntoIterator<Item = LeanLibraryDependency>,
        export_signatures: impl IntoIterator<Item = LeanExportSignature>,
    ) -> LeanResult<Self> {
        let package = package.into();
        let module = module.into();
        // SAFETY: the caller established the complete loaded artifact/dependency contract.
        let bundle = unsafe { LeanLibraryBundle::open(runtime, dylib_path, dependencies) }?;
        let _module = bundle.initialize_module(&package, &module)?;
        let export_signatures = export_signatures
            .into_iter()
            .map(|signature| (signature.symbol().to_owned(), signature))
            .collect();
        Ok(Self {
            bundle,
            package,
            module,
            export_signatures,
        })
    }

    /// Look up an exported symbol only if trusted manifest ABI metadata
    /// exactly matches the requested Rust call shape.
    ///
    /// C-slot metadata checks representation and ownership. It does not
    /// distinguish boxed value layouts or establish native semantic types.
    ///
    /// # Safety
    ///
    /// The native symbol must implement the complete Lean argument/result
    /// contracts selected by `Args` and `R`, including nested boxed layouts,
    /// scalar refinements, callbacks, runtime identity, and ownership. Each
    /// invocation must also satisfy [`LeanExported::call`]'s requirements.
    ///
    /// # Errors
    ///
    /// Returns [`LeanCheckedExportError`] when metadata is missing, the Rust
    /// shape disagrees with the manifest, the manifest's function/global kind
    /// disagrees with the loaded dylib, or the symbol cannot be resolved.
    pub unsafe fn exported<Args, R>(
        &self,
        name: &str,
    ) -> Result<LeanExported<'lean, '_, Args, R>, LeanCheckedExportError>
    where
        Args: LeanArgs<'lean>,
        R: DecodeCallResult<'lean>,
    {
        self.check_export_shape::<Args, R>(name)?;
        let module = self.module().map_err(LeanCheckedExportError::Module)?;
        // SAFETY: the caller established the complete native value contract.
        // Slot metadata and the loaded function/global kind were checked above.
        unsafe { module.exported_unchecked::<Args, R>(name) }.map_err(|source| LeanCheckedExportError::MissingSymbol {
            symbol: name.to_owned(),
            source,
        })
    }

    pub(super) fn check_export_shape<Args, R>(&self, name: &str) -> Result<(), LeanCheckedExportError>
    where
        Args: LeanArgs<'lean>,
        R: DecodeCallResult<'lean>,
    {
        let manifest =
            self.export_signatures
                .get(name)
                .ok_or_else(|| LeanCheckedExportError::MissingSignatureMetadata {
                    symbol: name.to_owned(),
                })?;
        let expected = match manifest.kind() {
            LeanExportSymbolKind::Function => {
                LeanExportSignature::function(name, Args::export_abi_args(), R::export_abi_return())
            }
            LeanExportSymbolKind::Global if Args::ARITY == 0 && !R::EXPECTS_IO_RESULT => {
                LeanExportSignature::global(name, R::export_abi_return())
            }
            LeanExportSymbolKind::Global => {
                LeanExportSignature::function(name, Args::export_abi_args(), R::export_abi_return())
            }
        };
        if &expected != manifest {
            return Err(LeanCheckedExportError::SignatureMismatch {
                symbol: name.to_owned(),
                expected: Box::new(expected),
                manifest: Box::new(manifest.clone()),
            });
        }

        let actual_kind = if self.library().globals().contains(name) {
            LeanExportSymbolKind::Global
        } else {
            LeanExportSymbolKind::Function
        };
        if manifest.kind() != actual_kind {
            return Err(LeanCheckedExportError::SymbolKindMismatch {
                symbol: name.to_owned(),
                manifest: manifest.kind(),
                actual: actual_kind,
            });
        }

        Ok(())
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

    /// Trusted export signatures parsed from the capability manifest.
    pub fn export_signatures(&self) -> impl Iterator<Item = &LeanExportSignature> {
        self.export_signatures.values()
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
  "schema_version": 2,
  "target_name": "Cap",
  "package": "pkg",
  "module": "Cap",
  "primary_dylib": "/tmp/libcap.so",
  "exports": [],
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
  "primary_dylib": "/tmp/libcap.so",
  "exports": []
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
