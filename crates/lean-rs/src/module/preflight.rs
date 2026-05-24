//! Manifest-backed loader preflight for shipped Lean capabilities.
//!
//! This module turns avoidable loader failures into stable, actionable
//! diagnostics before callers hit platform-specific dynamic-loader text. It
//! does not expose `ldd`, `otool`, `readelf`, `nm`, loader flags, or raw object
//! parser details. The public contract is a report: what failed, which artifact
//! it concerned, and what the caller should rebuild or package.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use object::{Object, ObjectSymbol};

use super::initializer::InitializerName;
use super::{LeanBuiltCapability, LeanLibraryDependency};
use crate::error::{LeanError, bound_message};

/// Stable preflight diagnostic codes for manifest-backed capability loading.
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
    fn error(
        code: LeanLoaderDiagnosticCode,
        subject: impl Into<String>,
        message: impl Into<String>,
        repair_hint: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: LeanLoaderSeverity::Error,
            subject: bound_message(subject.into()),
            message: bound_message(message.into()),
            repair_hint: bound_message(repair_hint.into()),
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
    fn new(manifest_path: Option<PathBuf>, checks: Vec<LeanLoaderCheck>) -> Self {
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

    pub(crate) fn into_error(self) -> LeanError {
        let Some(first) = self
            .checks
            .into_iter()
            .find(|check| check.severity == LeanLoaderSeverity::Error)
        else {
            return LeanError::module_init("Lean capability preflight failed without a recorded finding");
        };
        LeanError::module_init(format!(
            "{}: {}. repair: {}",
            first.code.as_str(),
            first.message,
            first.repair_hint
        ))
    }
}

/// Preflight runner for manifest-backed Lean capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanCapabilityPreflight {
    spec: LeanBuiltCapability,
}

impl LeanCapabilityPreflight {
    /// Create a preflight runner for a build-script capability descriptor.
    #[must_use]
    pub fn new(spec: LeanBuiltCapability) -> Self {
        Self { spec }
    }

    /// Check manifest, artifact, toolchain, dependency, and initializer facts.
    #[must_use]
    pub fn check(&self) -> LeanLoaderReport {
        let manifest_path = match self.spec.resolved_manifest_path() {
            Ok(path) => path,
            Err(err) => {
                return LeanLoaderReport::new(
                    None,
                    vec![LeanLoaderCheck::error(
                        LeanLoaderDiagnosticCode::MissingManifest,
                        "manifest",
                        err.to_string(),
                        "rebuild the Lean capability through CargoLeanCapability and embed the manifest env var",
                    )],
                );
            }
        };

        let manifest = match CapabilityManifest::read(&manifest_path) {
            Ok(manifest) => manifest,
            Err(check) => return LeanLoaderReport::new(Some(manifest_path), vec![check]),
        };

        let mut checks = Vec::new();
        check_fingerprint(&manifest, &mut checks);
        check_staleness(&manifest_path, &manifest, &mut checks);

        let mut dependency_exports = HashSet::new();
        for dependency in &manifest.dependencies {
            match inspect_artifact(dependency.path_ref(), ArtifactRole::Dependency) {
                Ok(info) => {
                    dependency_exports.extend(info.defined_symbols);
                }
                Err(check) => checks.push(check),
            }
        }

        match inspect_artifact(&manifest.primary_dylib, ArtifactRole::Primary) {
            Ok(info) => {
                check_initializer(&manifest, &info, &mut checks);
                check_imported_symbols(&manifest, &info, &dependency_exports, &mut checks);
            }
            Err(check) => checks.push(check),
        }

        LeanLoaderReport::new(Some(manifest_path), checks)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapabilityManifest {
    pub(crate) primary_dylib: PathBuf,
    pub(crate) package: String,
    pub(crate) module: String,
    pub(crate) dependencies: Vec<LeanLibraryDependency>,
    pub(crate) lean_version: Option<String>,
    pub(crate) resolved_lean_version: Option<String>,
    pub(crate) lean_header_sha256: Option<String>,
}

impl CapabilityManifest {
    pub(crate) fn read(path: &Path) -> Result<Self, LeanLoaderCheck> {
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
        if schema_version != u64::from(lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION) {
            return Err(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::UnsupportedManifestSchema,
                path.display().to_string(),
                format!(
                    "unsupported Lean capability manifest schema {schema_version}; supported schema is {}",
                    lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION
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

#[derive(Clone, Debug)]
struct ArtifactInfo {
    defined_symbols: HashSet<String>,
    undefined_symbols: HashSet<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ArtifactRole {
    Primary,
    Dependency,
}

fn inspect_artifact(path: &Path, role: ArtifactRole) -> Result<ArtifactInfo, LeanLoaderCheck> {
    let missing_code = match role {
        ArtifactRole::Primary => LeanLoaderDiagnosticCode::MissingPrimaryDylib,
        ArtifactRole::Dependency => LeanLoaderDiagnosticCode::MissingTransitiveDependency,
    };
    let repair = match role {
        ArtifactRole::Primary => {
            "rebuild the Lean capability through CargoLeanCapability and package the primary dylib"
        }
        ArtifactRole::Dependency => {
            "rebuild the Lean capability through CargoLeanCapability and package every manifest dependency"
        }
    };
    let bytes = std::fs::read(path).map_err(|err| {
        let code = if err.kind() == std::io::ErrorKind::NotFound {
            missing_code
        } else {
            LeanLoaderDiagnosticCode::UnsupportedArchitecture
        };
        LeanLoaderCheck::error(
            code,
            path.display().to_string(),
            format!("failed to read Lean dylib '{}': {err}", path.display()),
            repair,
        )
    })?;
    let file = object::File::parse(&*bytes).map_err(|err| {
        LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::UnsupportedArchitecture,
            path.display().to_string(),
            format!(
                "Lean dylib '{}' is not a supported native object for this platform: {err}",
                path.display()
            ),
            "rebuild the Lean capability for the current target architecture",
        )
    })?;
    if !architecture_matches_host(file.architecture()) {
        return Err(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::UnsupportedArchitecture,
            path.display().to_string(),
            format!(
                "Lean dylib '{}' has architecture {:?}, but this process is {}",
                path.display(),
                file.architecture(),
                std::env::consts::ARCH
            ),
            "rebuild the Lean capability for the current target architecture",
        ));
    }
    let strip_underscore = matches!(file.format(), object::BinaryFormat::MachO | object::BinaryFormat::Wasm);
    let mut defined_symbols = HashSet::new();
    let mut undefined_symbols = HashSet::new();
    for symbol in file.symbols().chain(file.dynamic_symbols()) {
        let Ok(name) = symbol.name() else {
            continue;
        };
        let normalised = normalise_symbol_name(name, strip_underscore);
        if normalised.is_empty() {
            continue;
        }
        if symbol.is_undefined() {
            undefined_symbols.insert(normalised.to_owned());
        } else if symbol.is_global() {
            defined_symbols.insert(normalised.to_owned());
        }
    }
    Ok(ArtifactInfo {
        defined_symbols,
        undefined_symbols,
    })
}

fn check_initializer(manifest: &CapabilityManifest, primary: &ArtifactInfo, checks: &mut Vec<LeanLoaderCheck>) {
    let initializer = match InitializerName::from_lake_names(&manifest.package, &manifest.module) {
        Ok(initializer) => initializer,
        Err(err) => {
            checks.push(LeanLoaderCheck::error(
                LeanLoaderDiagnosticCode::MalformedManifest,
                format!("{}/{}", manifest.package, manifest.module),
                err.to_string(),
                "rebuild the manifest with valid Lake package and module names",
            ));
            return;
        }
    };
    if primary.defined_symbols.contains(initializer.symbol_str())
        || primary.defined_symbols.contains(initializer.legacy_symbol_str())
    {
        return;
    }
    checks.push(LeanLoaderCheck::error(
        LeanLoaderDiagnosticCode::MissingInitializer,
        format!("{}/{}", manifest.package, manifest.module),
        format!(
            "primary dylib '{}' does not export initializer '{}' or '{}'",
            manifest.primary_dylib.display(),
            initializer.symbol_str(),
            initializer.legacy_symbol_str()
        ),
        "check the package/module names and rebuild the Lean capability shared target",
    ));
}

fn check_imported_symbols(
    manifest: &CapabilityManifest,
    primary: &ArtifactInfo,
    dependency_exports: &HashSet<String>,
    checks: &mut Vec<LeanLoaderCheck>,
) {
    for symbol in primary
        .undefined_symbols
        .iter()
        .filter(|symbol| is_lean_dependency_symbol(symbol))
    {
        if dependency_exports.contains(symbol) {
            continue;
        }
        checks.push(LeanLoaderCheck::error(
            LeanLoaderDiagnosticCode::MissingImportedSymbol,
            symbol.clone(),
            format!(
                "primary dylib '{}' imports Lean symbol '{symbol}' that is not provided by the manifest dependencies",
                manifest.primary_dylib.display()
            ),
            "rebuild the Lean capability through CargoLeanCapability so dependency dylibs are recorded in the manifest",
        ));
        return;
    }
}

fn check_fingerprint(manifest: &CapabilityManifest, checks: &mut Vec<LeanLoaderCheck>) {
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

fn check_staleness(manifest_path: &Path, manifest: &CapabilityManifest, checks: &mut Vec<LeanLoaderCheck>) {
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

fn architecture_matches_host(architecture: object::Architecture) -> bool {
    matches!(
        (std::env::consts::ARCH, architecture),
        ("x86_64", object::Architecture::X86_64)
            | ("aarch64", object::Architecture::Aarch64)
            | ("arm", object::Architecture::Arm)
            | ("x86", object::Architecture::I386)
    )
}

fn normalise_symbol_name(name: &str, strip_underscore: bool) -> &str {
    if strip_underscore {
        name.strip_prefix('_').unwrap_or(name)
    } else {
        name
    }
}

fn is_lean_dependency_symbol(symbol: &str) -> bool {
    symbol.starts_with("LeanRs") || symbol.starts_with("lean_rs_") || symbol.starts_with("initialize_LeanRs")
}

pub(crate) fn manifest_error_to_lean_error(check: LeanLoaderCheck) -> LeanError {
    LeanLoaderReport::new(None, vec![check]).into_error()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        ArtifactInfo, CapabilityManifest, LeanCapabilityPreflight, LeanLoaderDiagnosticCode, check_imported_symbols,
        check_staleness, inspect_artifact,
    };
    use crate::{LeanBuiltCapability, LeanCapability, LeanRuntime};
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn missing_manifest_reports_stable_code() {
        let path = temp_dir("missing_manifest").join("missing.json");
        let report = LeanCapabilityPreflight::new(LeanBuiltCapability::manifest_path(path)).check();

        assert!(!report.is_ok());
        assert_eq!(
            report.first_error().map(crate::LeanLoaderCheck::code),
            Some(LeanLoaderDiagnosticCode::MissingManifest)
        );
    }

    #[test]
    fn malformed_manifest_reports_stable_code() {
        let dir = temp_dir("malformed_manifest");
        let manifest = dir.join("capability.json");
        fs::write(&manifest, "{").expect("write malformed manifest");

        let report = LeanCapabilityPreflight::new(LeanBuiltCapability::manifest_path(&manifest)).check();

        assert_eq!(
            report.first_error().map(crate::LeanLoaderCheck::code),
            Some(LeanLoaderDiagnosticCode::MalformedManifest)
        );
        assert_eq!(report.manifest_path(), Some(manifest.as_path()));
    }

    #[test]
    fn unsupported_manifest_schema_reports_stable_code() {
        let dir = temp_dir("unsupported_manifest_schema");
        let manifest = dir.join("capability.json");
        fs::write(
            &manifest,
            r#"{
  "schema_version": 999,
  "target_name": "Cap",
  "package": "pkg",
  "module": "Cap",
  "primary_dylib": "/tmp/libcap.so"
}"#,
        )
        .expect("write unsupported manifest schema");

        let report = LeanCapabilityPreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

        assert_eq!(
            report.first_error().map(crate::LeanLoaderCheck::code),
            Some(LeanLoaderDiagnosticCode::UnsupportedManifestSchema)
        );
    }

    #[test]
    fn missing_primary_dylib_reports_stable_code() {
        let dir = temp_dir("missing_primary_dylib");
        let missing = dir.join("missing-capability.dylib");
        let manifest = write_manifest(&dir, &missing, "", "");

        let report = LeanCapabilityPreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

        assert!(contains_code(&report, LeanLoaderDiagnosticCode::MissingPrimaryDylib));
    }

    #[test]
    fn missing_dependency_reports_stable_code() {
        let dir = temp_dir("missing_dependency");
        let missing = dir.join("missing-dependency.dylib");
        let dependency = format!(
            r#""dependencies":[{{"dylib_path":"{}","export_symbols_for_dependents":true}}],"#,
            json_path(&missing)
        );
        let manifest = write_manifest(&dir, current_exe(), &dependency, "");

        let report = LeanCapabilityPreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

        assert!(contains_code(
            &report,
            LeanLoaderDiagnosticCode::MissingTransitiveDependency
        ));
    }

    #[test]
    fn invalid_object_reports_unsupported_architecture() {
        let dir = temp_dir("invalid_object");
        let bad = dir.join("not-a-dylib.so");
        fs::write(&bad, b"not an object").expect("write invalid object");

        let err = inspect_artifact(&bad, super::ArtifactRole::Primary).expect_err("invalid object should fail");

        assert_eq!(err.code(), LeanLoaderDiagnosticCode::UnsupportedArchitecture);
    }

    #[test]
    fn unsupported_toolchain_fingerprint_reports_stable_code() {
        let dir = temp_dir("unsupported_toolchain_fingerprint");
        let manifest = dir.join("capability.json");
        fs::write(
            &manifest,
            format!(
                r#"{{
  "schema_version": 1,
  "target_name": "Cap",
  "package": "pkg",
  "module": "Cap",
  "primary_dylib": "{}",
  "toolchain_fingerprint": {{
    "lean_version": "0.0.0",
    "resolved_version": "0.0.0",
    "header_sha256": "0000"
  }}
}}"#,
                json_path(&current_exe())
            ),
        )
        .expect("write unsupported fingerprint manifest");

        let report = LeanCapabilityPreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

        assert!(contains_code(
            &report,
            LeanLoaderDiagnosticCode::UnsupportedToolchainFingerprint
        ));
    }

    #[test]
    fn stale_manifest_reports_stable_code() {
        let dir = temp_dir("stale_manifest");
        let primary = dir.join("libcap.so");
        fs::write(&primary, b"old").expect("write old primary");
        let manifest = write_manifest(&dir, &primary, "", "");
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&primary, b"new").expect("write newer primary");
        let parsed = CapabilityManifest::read(&manifest).expect("manifest parses");
        let mut checks = Vec::new();

        check_staleness(&manifest, &parsed, &mut checks);

        assert!(
            checks
                .iter()
                .any(|check| check.code() == LeanLoaderDiagnosticCode::StaleManifest)
        );
    }

    #[test]
    fn missing_initializer_reports_stable_code() {
        let dir = temp_dir("missing_initializer");
        let manifest = write_manifest(&dir, current_exe(), "", "");

        let report = LeanCapabilityPreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

        assert!(contains_code(&report, LeanLoaderDiagnosticCode::MissingInitializer));
    }

    #[test]
    fn missing_imported_symbol_reports_stable_code() {
        let primary = ArtifactInfo {
            defined_symbols: HashSet::new(),
            undefined_symbols: HashSet::from(["LeanRsInterop_missing".to_owned()]),
        };
        let manifest = CapabilityManifest {
            primary_dylib: PathBuf::from("/tmp/libcap.so"),
            package: "pkg".to_owned(),
            module: "Cap".to_owned(),
            dependencies: Vec::new(),
            lean_version: None,
            resolved_lean_version: None,
            lean_header_sha256: None,
        };
        let mut checks = Vec::new();

        check_imported_symbols(&manifest, &primary, &HashSet::new(), &mut checks);

        assert!(
            checks
                .iter()
                .any(|check| check.code() == LeanLoaderDiagnosticCode::MissingImportedSymbol)
        );
    }

    #[test]
    fn open_failure_uses_preflight_code_in_error_message() {
        let dir = temp_dir("open_failure_preflight_code");
        let missing = dir.join("missing-capability.dylib");
        let manifest = write_manifest(&dir, &missing, "", "");
        let runtime = LeanRuntime::init().expect("runtime init");

        let Err(err) = LeanCapability::from_build_manifest(runtime, LeanBuiltCapability::manifest_path(manifest))
        else {
            panic!("missing primary should fail before open");
        };

        assert_eq!(err.code(), crate::LeanDiagnosticCode::ModuleInit);
        assert!(
            err.to_string()
                .contains(LeanLoaderDiagnosticCode::MissingPrimaryDylib.as_str())
        );
    }

    fn contains_code(report: &crate::LeanLoaderReport, code: LeanLoaderDiagnosticCode) -> bool {
        report.checks().iter().any(|check| check.code() == code)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lean-rs-preflight-{}-{name}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("create preflight test dir");
        dir
    }

    fn current_exe() -> PathBuf {
        std::env::current_exe().expect("current test executable path")
    }

    fn write_manifest(dir: &Path, primary: impl AsRef<Path>, dependencies: &str, extra: &str) -> PathBuf {
        let manifest = dir.join("capability.json");
        let contents = format!(
            r#"{{
  {extra}
  "schema_version": 1,
  "target_name": "Cap",
  "package": "pkg",
  "module": "Cap",
  "primary_dylib": "{}",
  {dependencies}
  "toolchain_fingerprint": {{
    "lean_version": "{}",
    "resolved_version": "{}",
    "header_sha256": "{}"
  }}
}}"#,
            json_path(primary.as_ref()),
            lean_rs_sys::LEAN_VERSION,
            lean_rs_sys::LEAN_RESOLVED_VERSION,
            lean_rs_sys::LEAN_HEADER_DIGEST,
        );
        fs::write(&manifest, contents).expect("write manifest");
        manifest
    }

    fn json_path(path: &Path) -> String {
        path.display().to_string().replace('\\', "\\\\")
    }
}
