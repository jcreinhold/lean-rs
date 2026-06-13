//! Static, link-free validation of Lean capability manifests.
//!
//! Lives in `lean-toolchain` so the worker parent crate can fast-fail bad
//! manifests on the client side without re-linking `libleanshared` through
//! `lean-rs`. The runtime preflight in `lean-rs` reuses the same parser and
//! diagnostic shapes and layers symbol-table inspection on top.

use std::path::{Path, PathBuf};

use std::collections::HashSet;

use crate::loader::{
    LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
    LeanExportSignature, LeanExportSymbolKind, LeanLibraryDependency, LeanLoaderCheck, LeanLoaderDiagnosticCode,
    LeanLoaderReport,
};

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
    /// Trusted ABI signatures for safe exported-symbol lookup.
    pub exports: Vec<LeanExportSignature>,
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
        let exports = exports_from_manifest(&value, path)?;
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
            exports,
        })
    }
}

/// Static manifest validation: file-exists, JSON parse, schema check,
/// toolchain fingerprint match, and primary-dylib staleness.
///
/// This is the cheap, link-free preflight the worker parent runs before
/// spawning a child. The runtime preflight in `lean-rs` also inspects
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

/// Validate the toolchain fingerprint against this `lean-toolchain` build.
pub fn check_fingerprint(manifest: &CapabilityManifest, checks: &mut Vec<LeanLoaderCheck>) {
    if let Some(digest) = manifest.lean_header_sha256.as_deref() {
        if digest != crate::LEAN_HEADER_DIGEST {
            checks.push(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint,
                digest,
                format!(
                    "manifest Lean header digest {digest} does not match this process digest {}",
                    crate::LEAN_HEADER_DIGEST
                ),
                "rebuild the Lean capability with the same Lean ABI used by this Rust binary",
            ));
        }
        return;
    }
    if let Some(version) = manifest.lean_version.as_deref()
        && lean_rs_abi::supported_for(version).is_none()
    {
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint,
            version,
            format!("legacy manifest was built with unsupported Lean toolchain {version}"),
            "rebuild the Lean capability so the manifest records a Lean header digest",
        ));
        return;
    }
    if let Some(resolved) = manifest.resolved_lean_version.as_deref()
        && resolved != crate::LEAN_RESOLVED_VERSION
    {
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint,
            resolved,
            format!(
                "legacy manifest resolved Lean version {resolved} does not match this process resolved version {}",
                crate::LEAN_RESOLVED_VERSION
            ),
            "rebuild the Lean capability so the manifest records a Lean header digest",
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

fn exports_from_manifest(value: &serde_json::Value, path: &Path) -> Result<Vec<LeanExportSignature>, LeanLoaderCheck> {
    let raw_exports = value.get("exports").ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            format!(
                "Lean capability manifest '{}' is missing required array field `exports`",
                path.display()
            ),
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })?;
    let exports = raw_exports.as_array().ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            format!(
                "Lean capability manifest '{}' field `exports` must be an array",
                path.display()
            ),
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })?;
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(exports.len());
    for export in exports {
        let symbol = required_string(export, "symbol", path)?;
        if !seen.insert(symbol.clone()) {
            return Err(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::MalformedManifest,
                symbol,
                "Lean capability manifest contains duplicate export signature metadata",
                "emit at most one signature entry for each exported symbol",
            ));
        }
        let kind = parse_export_kind(export, path)?;
        let args = export_args_from_manifest(export, path)?;
        let result = export_return_from_manifest(export, path)?;
        if kind == LeanExportSymbolKind::Global && !args.is_empty() {
            return Err(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::MalformedManifest,
                symbol,
                "Lean capability manifest describes a global export with function arguments",
                "record globals with an empty `args` array",
            ));
        }
        if kind == LeanExportSymbolKind::Global && result.convention() == LeanExportResultConvention::IoResult {
            return Err(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::MalformedManifest,
                symbol,
                "Lean capability manifest describes a global export as an IO result",
                "record globals with a pure return convention",
            ));
        }
        out.push(match kind {
            LeanExportSymbolKind::Function => LeanExportSignature::function(symbol, args, result),
            LeanExportSymbolKind::Global => LeanExportSignature::global(symbol, result),
        });
    }
    Ok(out)
}

fn parse_export_kind(value: &serde_json::Value, path: &Path) -> Result<LeanExportSymbolKind, LeanLoaderCheck> {
    let kind = required_string(value, "kind", path)?;
    LeanExportSymbolKind::from_str(&kind).ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            kind,
            "Lean capability manifest export `kind` must be `function` or `global`",
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })
}

fn export_args_from_manifest(value: &serde_json::Value, path: &Path) -> Result<Vec<LeanExportArgAbi>, LeanLoaderCheck> {
    let raw_args = value.get("args").ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            "Lean capability manifest export is missing required array field `args`",
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })?;
    let args = raw_args.as_array().ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            "Lean capability manifest export field `args` must be an array",
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })?;
    args.iter().map(|arg| export_arg_from_manifest(arg, path)).collect()
}

fn export_arg_from_manifest(value: &serde_json::Value, path: &Path) -> Result<LeanExportArgAbi, LeanLoaderCheck> {
    let repr = parse_export_repr(value, path)?;
    let ownership = parse_export_ownership(value, path)?;
    validate_ownership(repr, ownership, path)?;
    Ok(LeanExportArgAbi::new(repr, ownership))
}

fn export_return_from_manifest(value: &serde_json::Value, path: &Path) -> Result<LeanExportReturnAbi, LeanLoaderCheck> {
    let raw_return = value.get("return").ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            "Lean capability manifest export is missing required object field `return`",
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })?;
    let repr = parse_export_repr(raw_return, path)?;
    let ownership = parse_export_ownership(raw_return, path)?;
    validate_ownership(repr, ownership, path)?;
    let convention = parse_export_result_convention(raw_return, path)?;
    if convention == LeanExportResultConvention::IoResult
        && (repr != LeanExportAbiRepr::LeanObject || ownership != LeanExportOwnership::Owned)
    {
        return Err(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            path.display().to_string(),
            "Lean capability manifest IO result must use owned `lean_object` return ABI",
            "record IO exports with `repr: lean_object`, `ownership: owned`, and `convention: io_result`",
        ));
    }
    Ok(LeanExportReturnAbi::new(repr, ownership, convention))
}

fn parse_export_repr(value: &serde_json::Value, path: &Path) -> Result<LeanExportAbiRepr, LeanLoaderCheck> {
    let repr = required_string(value, "repr", path)?;
    LeanExportAbiRepr::from_str(&repr).ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            repr,
            "Lean capability manifest ABI `repr` is not supported",
            "use a supported Lean export ABI representation",
        )
    })
}

fn parse_export_ownership(value: &serde_json::Value, path: &Path) -> Result<LeanExportOwnership, LeanLoaderCheck> {
    let ownership = required_string(value, "ownership", path)?;
    LeanExportOwnership::from_str(&ownership).ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            ownership,
            "Lean capability manifest ABI `ownership` must be `none` or `owned`",
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })
}

fn parse_export_result_convention(
    value: &serde_json::Value,
    path: &Path,
) -> Result<LeanExportResultConvention, LeanLoaderCheck> {
    let convention = required_string(value, "convention", path)?;
    LeanExportResultConvention::from_str(&convention).ok_or_else(|| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MalformedManifest,
            convention,
            "Lean capability manifest return `convention` must be `pure` or `io_result`",
            "rebuild the Lean capability through CargoLeanCapability",
        )
    })
}

fn validate_ownership(
    repr: LeanExportAbiRepr,
    ownership: LeanExportOwnership,
    path: &Path,
) -> Result<(), LeanLoaderCheck> {
    let expected = if repr == LeanExportAbiRepr::LeanObject {
        LeanExportOwnership::Owned
    } else {
        LeanExportOwnership::None
    };
    if ownership == expected {
        return Ok(());
    }
    Err(LeanLoaderCheck::error(
        LeanLoaderDiagnosticCode::MalformedManifest,
        path.display().to_string(),
        format!(
            "Lean capability manifest ABI ownership `{}` does not match representation `{}`",
            ownership.as_str(),
            repr.as_str()
        ),
        "use `owned` for `lean_object` slots and `none` for scalar slots",
    ))
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::{CapabilityManifest, check_fingerprint};
    use crate::{
        CAPABILITY_MANIFEST_SCHEMA_VERSION, LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership,
        LeanExportResultConvention, LeanExportReturnAbi, LeanExportSignature, LeanExportSymbolKind,
        LeanLoaderDiagnosticCode,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn manifest_round_trips_export_signatures() {
        let path = temp_manifest_path("manifest_round_trips_export_signatures");
        let signature = LeanExportSignature::function(
            "cap_u8_identity",
            vec![LeanExportArgAbi::new(LeanExportAbiRepr::U8, LeanExportOwnership::None)],
            LeanExportReturnAbi::new(
                LeanExportAbiRepr::U8,
                LeanExportOwnership::None,
                LeanExportResultConvention::Pure,
            ),
        );
        write_manifest(&path, CAPABILITY_MANIFEST_SCHEMA_VERSION, &[signature.to_json()]);

        let manifest = CapabilityManifest::read(&path).expect("manifest parses");

        assert_eq!(manifest.exports, vec![signature]);
        let Some(parsed) = manifest.exports.first() else {
            panic!("expected export signature");
        };
        assert_eq!(parsed.symbol(), "cap_u8_identity");
        assert_eq!(parsed.kind(), LeanExportSymbolKind::Function);
        assert_eq!(parsed.args().len(), 1);
    }

    #[test]
    fn old_manifest_schema_is_rejected() {
        let path = temp_manifest_path("old_manifest_schema_is_rejected");
        write_manifest(&path, 1, &[]);

        let err = CapabilityManifest::read(&path).expect_err("schema v1 is obsolete");

        assert_eq!(err.code(), LeanLoaderDiagnosticCode::UnsupportedManifestSchema);
    }

    #[test]
    fn fingerprint_accepts_header_identical_resolved_alias() {
        let Some(alias) = header_identical_alias() else {
            return;
        };
        let manifest = manifest_with_fingerprint(alias, alias, Some(crate::LEAN_HEADER_DIGEST.to_owned()));
        let mut checks = Vec::new();

        check_fingerprint(&manifest, &mut checks);

        assert!(
            checks.is_empty(),
            "header-identical version aliases should pass: {checks:?}"
        );
    }

    #[test]
    fn fingerprint_rejects_mismatched_header_digest() {
        let manifest = manifest_with_fingerprint(
            crate::LEAN_VERSION,
            crate::LEAN_RESOLVED_VERSION,
            Some("0000000000000000000000000000000000000000000000000000000000000000".to_owned()),
        );
        let mut checks = Vec::new();

        check_fingerprint(&manifest, &mut checks);

        assert_eq!(
            checks.first().map(crate::LeanLoaderCheck::code),
            Some(LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint)
        );
    }

    #[test]
    fn fingerprint_with_digest_treats_version_as_metadata() {
        let manifest = manifest_with_fingerprint(
            "4.31.0-local-dev",
            "4.31.0-other-alias",
            Some(crate::LEAN_HEADER_DIGEST.to_owned()),
        );
        let mut checks = Vec::new();

        check_fingerprint(&manifest, &mut checks);

        assert!(checks.is_empty(), "matching digest should be authoritative: {checks:?}");
    }

    #[test]
    fn legacy_fingerprint_without_digest_checks_resolved_version() {
        let Some(alias) = header_identical_alias() else {
            return;
        };
        let manifest = manifest_with_fingerprint(alias, alias, None);
        let mut checks = Vec::new();

        check_fingerprint(&manifest, &mut checks);

        assert_eq!(
            checks.first().map(crate::LeanLoaderCheck::code),
            Some(LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint)
        );
    }

    fn temp_manifest_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lean-toolchain-manifest-{}-{name}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).expect("create manifest test dir");
        dir.join("capability.json")
    }

    fn write_manifest(path: &Path, schema_version: u32, exports: &[serde_json::Value]) {
        let manifest = serde_json::json!({
            "schema_version": schema_version,
            "target_name": "Cap",
            "package": "pkg",
            "module": "Cap",
            "primary_dylib": "/tmp/libcap.so",
            "dependencies": [],
            "exports": exports,
        });
        std::fs::write(path, serde_json::to_vec_pretty(&manifest).expect("encode manifest")).expect("write manifest");
    }

    fn manifest_with_fingerprint(
        lean_version: &str,
        resolved_lean_version: &str,
        lean_header_sha256: Option<String>,
    ) -> CapabilityManifest {
        CapabilityManifest {
            primary_dylib: PathBuf::from("/tmp/libcap.so"),
            package: "pkg".to_owned(),
            module: "Cap".to_owned(),
            dependencies: Vec::new(),
            lean_version: Some(lean_version.to_owned()),
            resolved_lean_version: Some(resolved_lean_version.to_owned()),
            lean_header_sha256,
            exports: Vec::new(),
        }
    }

    fn header_identical_alias() -> Option<&'static str> {
        match crate::LEAN_VERSION {
            "4.31.0-rc1" => Some("4.31.0-rc2"),
            "4.31.0-rc2" => Some("4.31.0-rc1"),
            _ => None,
        }
    }
}
