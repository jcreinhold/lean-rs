//! Manifest-backed loader preflight for shipped Lean capabilities.
//!
//! This module turns avoidable loader failures into stable, actionable
//! diagnostics before callers hit platform-specific dynamic-loader text. It
//! does not expose `ldd`, `otool`, `readelf`, `nm`, loader flags, or raw object
//! parser details. The public contract is a report: what failed, which artifact
//! it concerned, and what the caller should rebuild or package.
//!
//! The cheap, link-free static checks (file/JSON/schema/fingerprint/staleness)
//! live in [`lean_toolchain::manifest_validation`] so the worker parent crate
//! can fast-fail bad manifests without re-linking `libleanshared`. This module
//! layers symbol-table inspection on top using the `object` crate, which is
//! the half of preflight that genuinely needs the runtime crate.

use std::collections::HashSet;

use object::{Object, ObjectSymbol};

use super::LeanBuiltCapability;
use super::initializer::InitializerName;
use crate::error::{LeanError, bound_message};

// Loader data types live in `lean-toolchain` (below `lean-rs`) so the worker
// wire protocol can reference them without re-linking `libleanshared`. The
// runtime preflight in this module layers on top of the same data types.
// Re-exported here for callers using the historical paths.
pub(crate) use lean_toolchain::CapabilityManifest;
pub use lean_toolchain::{LeanLoaderCheck, LeanLoaderDiagnosticCode, LeanLoaderReport, LeanLoaderSeverity};

/// Runtime preflight runner for manifest-backed Lean capabilities.
///
/// Layers symbol-table inspection on top of
/// [`lean_toolchain::manifest_validation::check_static`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanRuntimePreflight {
    spec: LeanBuiltCapability,
}

/// Backwards-compatible alias for [`LeanRuntimePreflight`].
///
/// The struct was renamed when the static manifest-validation half moved to
/// `lean-toolchain`; this alias preserves the historical public path.
pub type LeanCapabilityPreflight = LeanRuntimePreflight;

impl LeanRuntimePreflight {
    /// Create a runtime preflight runner for a build-script capability descriptor.
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
        lean_toolchain::manifest_validation::check_fingerprint(&manifest, &mut checks);
        lean_toolchain::manifest_validation::check_staleness(&manifest_path, &manifest, &mut checks);

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

pub(crate) fn report_into_error(report: LeanLoaderReport) -> LeanError {
    let Some(first) = report
        .into_checks()
        .into_iter()
        .find(|check| check.severity() == LeanLoaderSeverity::Error)
    else {
        return LeanError::module_init("Lean capability preflight failed without a recorded finding");
    };
    LeanError::module_init(bound_message(format!(
        "{}: {}. repair: {}",
        first.code().as_str(),
        first.message(),
        first.repair_hint()
    )))
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

fn inspect_artifact(path: &std::path::Path, role: ArtifactRole) -> Result<ArtifactInfo, LeanLoaderCheck> {
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
    report_into_error(LeanLoaderReport::new(None, vec![check]))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        ArtifactInfo, CapabilityManifest, LeanLoaderDiagnosticCode, LeanRuntimePreflight, check_imported_symbols,
        inspect_artifact,
    };
    use crate::{LeanBuiltCapability, LeanCapability, LeanRuntime};
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn missing_manifest_reports_stable_code() {
        let path = temp_dir("missing_manifest").join("missing.json");
        let report = LeanRuntimePreflight::new(LeanBuiltCapability::manifest_path(path)).check();

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

        let report = LeanRuntimePreflight::new(LeanBuiltCapability::manifest_path(&manifest)).check();

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

        let report = LeanRuntimePreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

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

        let report = LeanRuntimePreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

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

        let report = LeanRuntimePreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

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

        let report = LeanRuntimePreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

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

        lean_toolchain::manifest_validation::check_staleness(&manifest, &parsed, &mut checks);

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

        let report = LeanRuntimePreflight::new(LeanBuiltCapability::manifest_path(manifest)).check();

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
