//! Static, link-free validation of Lean capability manifests.
//!
//! Lives in `lean-toolchain` so the worker parent crate can fast-fail bad
//! manifests on the client side without re-linking `libleanshared` through
//! `lean-rs`. The runtime preflight in `lean-rs` reuses the same parser and
//! diagnostic shapes and layers symbol-table inspection on top.

use std::path::{Path, PathBuf};

use crate::loader::{LeanLibraryDependency, LeanLoaderCheck, LeanLoaderDiagnosticCode, LeanLoaderReport};

/// Parsed Lean capability manifest.
///
/// The shape downstream `build.rs` emits through
/// `lean-toolchain::CargoLeanCapability`. The parser performs only schema-shape
/// validation; semantic checks live in [`check_static`] and the runtime
/// preflight in `lean-rs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityManifest {
    /// Path to the primary capability dylib.
    pub primary_dylib: PathBuf,
    /// Lake package name used by the root module initializer.
    pub package: String,
    /// Root Lean module name initialized by Rust.
    pub module: String,
    /// Dependency dylibs that must be opened before the primary.
    pub dependencies: Vec<LeanLibraryDependency>,
    /// Lean version recorded by the build script, if any.
    pub lean_version: Option<String>,
    /// Resolved Lean toolchain version, if any.
    pub resolved_lean_version: Option<String>,
    /// SHA-256 of the Lean header used to build the capability, if any.
    pub lean_header_sha256: Option<String>,
}

impl CapabilityManifest {
    /// Parse a Lean capability manifest from disk.
    ///
    /// # Errors
    ///
    /// Returns a `LeanLoaderCheck` describing the first parsing failure.
    pub fn read(path: &Path) -> Result<Self, LeanLoaderCheck> {
        let bytes = std::fs::read(path).map_err(|err| {
            let code = if err.kind() == std::io::ErrorKind::NotFound {
                LeanLoaderDiagnosticCode::MissingManifest
            } else {
                LeanLoaderDiagnosticCode::MalformedManifest
            };
            LeanLoaderCheck::error(
                code,
                path.display().to_string(),
                format!("failed to read Lean capability manifest '{}': {err}", path.display()),
                "rebuild the Lean capability through CargoLeanCapability and ensure the manifest file is packaged",
            )
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| {
            LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::MalformedManifest,
                path.display().to_string(),
                format!("Lean capability manifest '{}' is not valid JSON: {err}", path.display()),
                "rebuild the Lean capability through CargoLeanCapability",
            )
        })?;
        let schema_version = required_u64(&value, "schema_version", path)?;
        if schema_version != u64::from(crate::CAPABILITY_MANIFEST_SCHEMA_VERSION) {
            return Err(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::UnsupportedManifestSchema,
                path.display().to_string(),
                format!(
                    "unsupported Lean capability manifest schema {schema_version}; supported schema is {}",
                    crate::CAPABILITY_MANIFEST_SCHEMA_VERSION
                ),
                "rebuild the Lean capability with the same lean-rs version as the runtime crate",
            ));
        }
        let primary_dylib = PathBuf::from(required_string(&value, "primary_dylib", path)?);
        let package = required_string(&value, "package", path)?;
        let module = required_string(&value, "module", path)?;
        let dependencies = dependencies_from_manifest(&value, path)?;
        let fingerprint = value.get("toolchain_fingerprint").unwrap_or(&serde_json::Value::Null);
        let lean_version =
            optional_string(fingerprint, "lean_version").or_else(|| optional_string(&value, "lean_version"));
        let resolved_lean_version = optional_string(fingerprint, "resolved_version")
            .or_else(|| optional_string(&value, "resolved_lean_version"));
        let lean_header_sha256 =
            optional_string(fingerprint, "header_sha256").or_else(|| optional_string(&value, "lean_header_sha256"));
        Ok(Self {
            primary_dylib,
            package,
            module,
            dependencies,
            lean_version,
            resolved_lean_version,
            lean_header_sha256,
        })
    }
}

/// Static manifest validation: file-exists, JSON parse, schema check,
/// toolchain fingerprint match, and primary-dylib staleness.
///
/// This is the cheap, link-free preflight the worker parent runs before
/// spawning a child. The runtime preflight in `lean-rs` additionally inspects
/// the dylib's symbol table.
#[must_use]
pub fn check_static(manifest_path: &Path) -> LeanLoaderReport {
    let manifest = match CapabilityManifest::read(manifest_path) {
        Ok(manifest) => manifest,
        Err(check) => return LeanLoaderReport::new(Some(manifest_path.to_path_buf()), vec![check]),
    };

    let mut checks = Vec::new();
    check_fingerprint(&manifest, &mut checks);
    check_staleness(manifest_path, &manifest, &mut checks);
    check_dependency_paths(&manifest, &mut checks);
    check_primary_dylib_present(&manifest, &mut checks);

    LeanLoaderReport::new(Some(manifest_path.to_path_buf()), checks)
}

/// Validate the toolchain fingerprint against the current `lean-rs-sys` build.
pub fn check_fingerprint(manifest: &CapabilityManifest, checks: &mut Vec<LeanLoaderCheck>) {
    if let Some(version) = manifest.lean_version.as_deref()
        && lean_rs_sys::supported_for(version).is_none()
    {
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint,
            version,
            format!("manifest was built with unsupported Lean toolchain {version}"),
            "rebuild the Lean capability with a Lean version supported by this lean-rs release",
        ));
        return;
    }
    if let Some(digest) = manifest.lean_header_sha256.as_deref()
        && digest != lean_rs_sys::LEAN_HEADER_DIGEST
    {
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint,
            digest,
            format!(
                "manifest Lean header digest {digest} does not match this process digest {}",
                lean_rs_sys::LEAN_HEADER_DIGEST
            ),
            "rebuild the Lean capability with the same Lean toolchain used by this Rust binary",
        ));
        return;
    }
    if let Some(resolved) = manifest.resolved_lean_version.as_deref()
        && resolved != lean_rs_sys::LEAN_RESOLVED_VERSION
    {
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint,
            resolved,
            format!(
                "manifest resolved Lean version {resolved} does not match this process resolved version {}",
                lean_rs_sys::LEAN_RESOLVED_VERSION
            ),
            "rebuild the Lean capability with the same Lean toolchain used by this Rust binary",
        ));
    }
}

/// Warn if the manifest is older than its primary dylib.
pub fn check_staleness(manifest_path: &Path, manifest: &CapabilityManifest, checks: &mut Vec<LeanLoaderCheck>) {
    let Ok(manifest_modified) = std::fs::metadata(manifest_path).and_then(|metadata| metadata.modified()) else {
        return;
    };
    let Ok(primary_modified) = std::fs::metadata(&manifest.primary_dylib).and_then(|metadata| metadata.modified())
    else {
        return;
    };
    if primary_modified > manifest_modified {
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::StaleManifest,
            manifest_path.display().to_string(),
            format!(
                "manifest '{}' is older than primary dylib '{}'",
                manifest_path.display(),
                manifest.primary_dylib.display()
            ),
            "rebuild the Lean capability through CargoLeanCapability so the manifest matches the dylib",
        ));
    }
}

fn check_dependency_paths(manifest: &CapabilityManifest, checks: &mut Vec<LeanLoaderCheck>) {
    for dependency in &manifest.dependencies {
        if !dependency.path_ref().is_file() {
            checks.push(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::MissingTransitiveDependency,
                dependency.path_ref().display().to_string(),
                format!(
                    "Lean dependency dylib '{}' named by the manifest is missing",
                    dependency.path_ref().display()
                ),
                "rebuild the Lean capability through CargoLeanCapability and package every manifest dependency",
            ));
        }
    }
}

fn check_primary_dylib_present(manifest: &CapabilityManifest, checks: &mut Vec<LeanLoaderCheck>) {
    if !manifest.primary_dylib.is_file() {
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MissingPrimaryDylib,
            manifest.primary_dylib.display().to_string(),
            format!(
                "primary Lean capability dylib '{}' named by the manifest is missing",
                manifest.primary_dylib.display()
            ),
            "rebuild the Lean capability through CargoLeanCapability and package the primary dylib",
        ));
    }
}

fn dependencies_from_manifest(
    value: &serde_json::Value,
    path: &Path,
) -> Result<Vec<LeanLibraryDependency>, LeanLoaderCheck> {
    let Some(raw_dependencies) = value.get("dependencies") else {
        return Ok(Vec::new());
    };
    let dependencies = raw_dependencies.as_array().ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            format!(
                "Lean capability manifest '{}' field `dependencies` must be an array",
                path.display()
            ),
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })?;
    let mut out = Vec::with_capacity(dependencies.len());
    for dependency in dependencies {
        let dylib = required_string(dependency, "dylib_path", path)?;
        let mut descriptor = LeanLibraryDependency::path(dylib);
        if dependency
            .get("export_symbols_for_dependents")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            descriptor = descriptor.export_symbols_for_dependents();
        }
        if let Some(initializer) = dependency.get("initializer") {
            let package = required_string(initializer, "package", path)?;
            let module = required_string(initializer, "module", path)?;
            descriptor = descriptor.initializer(package, module);
        }
        out.push(descriptor);
    }
    Ok(out)
}

fn required_string(value: &serde_json::Value, field: &str, path: &Path) -> Result<String, LeanLoaderCheck> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::MalformedManifest,
                path.display().to_string(),
                format!(
                    "Lean capability manifest '{}' is missing non-empty string field `{field}`",
                    path.display()
                ),
                "rebuild the Lean capability through CargoLeanCapability",
            )
        })
}

fn optional_string(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn required_u64(value: &serde_json::Value, field: &str, path: &Path) -> Result<u64, LeanLoaderCheck> {
    value.get(field).and_then(serde_json::Value::as_u64).ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            format!(
                "Lean capability manifest '{}' is missing unsigned integer field `{field}`",
                path.display()
            ),
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })
}
