//! Lake project module discovery for higher-level planners.
//!
//! This module knows Lake source layout and module-name validation. It does
//! not know worker pools, downstream command names, or cache policy.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::ToolchainFingerprint;

/// A discovered Lean source module in a Lake project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanModuleDescriptor {
    pub module: String,
    pub path: PathBuf,
    pub source_root: String,
}

/// Stable facts about the discovered project source set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanModuleSetFingerprint {
    pub toolchain: ToolchainFingerprint,
    pub lakefile_sha256: String,
    pub manifest_sha256: Option<String>,
    pub source_count: u64,
    pub source_max_mtime_ns: u128,
}

/// A discovered Lake project and its Lean modules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanLakeProjectModules {
    pub requested_root: PathBuf,
    pub project_root: PathBuf,
    pub lakefile: PathBuf,
    pub module_roots: Vec<String>,
    pub selected_roots: Vec<String>,
    pub modules: Vec<LeanModuleDescriptor>,
    pub fingerprint: LeanModuleSetFingerprint,
}

/// Options for Lake module discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanModuleDiscoveryOptions {
    requested_root: PathBuf,
    selected_roots: Option<Vec<String>>,
    toolchain: ToolchainFingerprint,
}

impl LeanModuleDiscoveryOptions {
    /// Discover modules for the Lake project at or below `requested_root`.
    #[must_use]
    pub fn new(requested_root: impl Into<PathBuf>) -> Self {
        Self {
            requested_root: requested_root.into(),
            selected_roots: None,
            toolchain: ToolchainFingerprint::current(),
        }
    }

    /// Restrict discovery to these Lake module roots.
    #[must_use]
    pub fn selected_roots(mut self, roots: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.selected_roots = Some(roots.into_iter().map(Into::into).collect());
        self
    }

    /// Override the toolchain fingerprint used for validation and fingerprints.
    ///
    /// This is primarily useful for tests and external planners that compare
    /// a separately obtained toolchain identity.
    #[must_use]
    pub fn toolchain(mut self, toolchain: ToolchainFingerprint) -> Self {
        self.toolchain = toolchain;
        self
    }
}

/// Typed diagnostics for Lake module discovery.
#[non_exhaustive]
#[derive(Debug)]
pub enum LeanModuleDiscoveryDiagnostic {
    MissingLakeRoot {
        requested_root: PathBuf,
    },
    MissingModuleRoot {
        project_root: PathBuf,
        module_root: String,
    },
    InvalidModuleName {
        module: String,
        reason: String,
    },
    UnsupportedToolchain {
        active: String,
        supported_window: String,
    },
    Io {
        path: PathBuf,
        message: &'static str,
        source: std::io::Error,
    },
}

impl fmt::Display for LeanModuleDiscoveryDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLakeRoot { requested_root } => {
                write!(
                    f,
                    "lean-toolchain: no Lake project found at {} or {}/lean",
                    requested_root.display(),
                    requested_root.display()
                )
            }
            Self::MissingModuleRoot {
                project_root,
                module_root,
            } => {
                write!(
                    f,
                    "lean-toolchain: module root `{module_root}` was not found in {}",
                    project_root.display()
                )
            }
            Self::InvalidModuleName { module, reason } => {
                write!(f, "lean-toolchain: invalid Lean module name `{module}`: {reason}")
            }
            Self::UnsupportedToolchain {
                active,
                supported_window,
            } => {
                write!(
                    f,
                    "lean-toolchain: active Lean toolchain {active} is not in the supported window: {supported_window}"
                )
            }
            Self::Io { path, message, source } => {
                write!(f, "lean-toolchain: {message} at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for LeanModuleDiscoveryDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::MissingLakeRoot { .. }
            | Self::MissingModuleRoot { .. }
            | Self::InvalidModuleName { .. }
            | Self::UnsupportedToolchain { .. } => None,
        }
    }
}

/// Discover Lean modules in a Lake project.
///
/// # Errors
///
/// Returns typed diagnostics when the Lake root cannot be found, the active
/// toolchain is unsupported, a selected module root is missing, a module name
/// cannot be represented safely, or source traversal fails.
pub fn discover_lake_modules(
    options: LeanModuleDiscoveryOptions,
) -> Result<LeanLakeProjectModules, LeanModuleDiscoveryDiagnostic> {
    if !options.toolchain.is_supported() {
        return Err(LeanModuleDiscoveryDiagnostic::UnsupportedToolchain {
            active: options.toolchain.lean_version.to_owned(),
            supported_window: supported_window(),
        });
    }

    let requested_root = normalize_existing(&options.requested_root)?;
    let (project_root, lakefile) = lake_root_for(&requested_root)?;
    let module_roots = discover_module_roots(&project_root, &lakefile)?;
    let selected_roots = options.selected_roots.unwrap_or_else(|| module_roots.clone());
    for root in &selected_roots {
        validate_module_name(root)?;
        if !module_root_exists(&project_root, root) {
            return Err(LeanModuleDiscoveryDiagnostic::MissingModuleRoot {
                project_root,
                module_root: root.clone(),
            });
        }
    }

    let modules = enumerate_sources(&project_root, &selected_roots)?;
    let fingerprint = fingerprint_source_set(&project_root, &lakefile, &modules, options.toolchain)?;
    Ok(LeanLakeProjectModules {
        requested_root,
        project_root,
        lakefile,
        module_roots,
        selected_roots,
        modules,
        fingerprint,
    })
}

/// Return whether a Lake `lean_lib` target is declared in a project.
///
/// # Errors
///
/// Returns discovery diagnostics if the Lake root or lakefile cannot be read.
pub fn lake_target_declared(project_root: &Path, target_name: &str) -> Result<bool, LeanModuleDiscoveryDiagnostic> {
    let requested_root = normalize_existing(project_root)?;
    let (_project_root, lakefile) = lake_root_for(&requested_root)?;
    let contents = read_to_string(&lakefile, "could not read Lake file")?;
    let quoted = format!("lean_lib «{target_name}»");
    let bare = format!("lean_lib {target_name}");
    let string = format!("lean_lib \"{target_name}\"");
    Ok(contents.contains(&quoted) || contents.contains(&bare) || contents.contains(&string))
}

fn normalize_existing(path: &Path) -> Result<PathBuf, LeanModuleDiscoveryDiagnostic> {
    let expanded = expand_home(path);
    if !expanded.exists() {
        return Err(LeanModuleDiscoveryDiagnostic::MissingLakeRoot {
            requested_root: expanded,
        });
    }
    fs::canonicalize(&expanded).map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
        path: expanded,
        message: "could not canonicalize path",
        source,
    })
}

fn expand_home(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if text == "~" {
        return home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn lake_root_for(path: &Path) -> Result<(PathBuf, PathBuf), LeanModuleDiscoveryDiagnostic> {
    if let Some(lakefile) = lakefile_path(path) {
        return Ok((path.to_path_buf(), lakefile));
    }
    let nested = path.join("lean");
    if let Some(lakefile) = lakefile_path(&nested) {
        return Ok((nested, lakefile));
    }
    Err(LeanModuleDiscoveryDiagnostic::MissingLakeRoot {
        requested_root: path.to_path_buf(),
    })
}

fn lakefile_path(root: &Path) -> Option<PathBuf> {
    let toml = root.join("lakefile.toml");
    if toml.is_file() {
        return Some(toml);
    }
    let lean = root.join("lakefile.lean");
    lean.is_file().then_some(lean)
}

fn discover_module_roots(project_root: &Path, lakefile: &Path) -> Result<Vec<String>, LeanModuleDiscoveryDiagnostic> {
    let mut roots = if lakefile.file_name().and_then(|name| name.to_str()) == Some("lakefile.toml") {
        discover_toml_lakefile_roots(lakefile)?
    } else {
        discover_lean_lakefile_roots(lakefile)?
    };
    if roots.is_empty() {
        roots = discover_top_level_roots(project_root)?;
    }
    roots.sort();
    roots.dedup();
    for root in &roots {
        validate_module_name(root)?;
    }
    Ok(roots)
}

fn discover_toml_lakefile_roots(lakefile: &Path) -> Result<Vec<String>, LeanModuleDiscoveryDiagnostic> {
    let text = read_to_string(lakefile, "could not read Lake TOML file")?;
    let mut roots = Vec::new();
    let mut in_lean_lib = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[[lean_lib]]" {
            in_lean_lib = true;
            continue;
        }
        if trimmed.starts_with("[[") || trimmed.starts_with('[') {
            in_lean_lib = false;
            continue;
        }
        if in_lean_lib
            && let Some(raw) = trimmed.strip_prefix("name")
            && let Some((_eq, value)) = raw.split_once('=')
        {
            let root = normalize_lake_identifier(value);
            if !root.is_empty() {
                roots.push(root);
            }
        }
    }
    Ok(roots)
}

fn discover_lean_lakefile_roots(lakefile: &Path) -> Result<Vec<String>, LeanModuleDiscoveryDiagnostic> {
    let text = read_to_string(lakefile, "could not read Lake file")?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let rest = trimmed.strip_prefix("lean_lib ")?;
            let raw = rest.split_whitespace().next()?;
            let root = normalize_lake_identifier(raw);
            (!root.is_empty()).then_some(root)
        })
        .collect())
}

fn discover_top_level_roots(project_root: &Path) -> Result<Vec<String>, LeanModuleDiscoveryDiagnostic> {
    let mut roots = Vec::new();
    for entry in fs::read_dir(project_root).map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
        path: project_root.to_path_buf(),
        message: "could not read Lake project directory",
        source,
    })? {
        let entry = entry.map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
            path: project_root.to_path_buf(),
            message: "could not read Lake project directory entry",
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("lean")
            && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        {
            roots.push(stem.to_owned());
        }
    }
    Ok(roots)
}

fn module_root_exists(project_root: &Path, module_root: &str) -> bool {
    module_to_file(project_root, module_root).is_file() || module_to_dir(project_root, module_root).is_dir()
}

fn enumerate_sources(
    project_root: &Path,
    selected_roots: &[String],
) -> Result<Vec<LeanModuleDescriptor>, LeanModuleDiscoveryDiagnostic> {
    let mut modules = std::collections::BTreeMap::<String, LeanModuleDescriptor>::new();
    for source_root in selected_roots {
        let root_file = module_to_file(project_root, source_root);
        if root_file.is_file() {
            modules.insert(
                source_root.clone(),
                LeanModuleDescriptor {
                    module: source_root.clone(),
                    path: root_file,
                    source_root: source_root.clone(),
                },
            );
        }

        let module_dir = module_to_dir(project_root, source_root);
        if module_dir.is_dir() {
            collect_sources(project_root, &module_dir, source_root, &mut modules)?;
        }
    }
    Ok(modules.into_values().collect())
}

fn collect_sources(
    project_root: &Path,
    dir: &Path,
    source_root: &str,
    modules: &mut std::collections::BTreeMap<String, LeanModuleDescriptor>,
) -> Result<(), LeanModuleDiscoveryDiagnostic> {
    for entry in fs::read_dir(dir).map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
        path: dir.to_path_buf(),
        message: "could not read Lean source directory",
        source,
    })? {
        let entry = entry.map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
            path: dir.to_path_buf(),
            message: "could not read Lean source directory entry",
            source,
        })?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
            path: path.clone(),
            message: "could not stat Lean source path",
            source,
        })?;
        if metadata.is_dir() {
            collect_sources(project_root, &path, source_root, modules)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("lean") {
            let module = module_from_path(project_root, &path)?;
            validate_module_name(&module)?;
            modules.insert(
                module.clone(),
                LeanModuleDescriptor {
                    module,
                    path,
                    source_root: source_root.to_owned(),
                },
            );
        }
    }
    Ok(())
}

fn fingerprint_source_set(
    project_root: &Path,
    lakefile: &Path,
    modules: &[LeanModuleDescriptor],
    toolchain: ToolchainFingerprint,
) -> Result<LeanModuleSetFingerprint, LeanModuleDiscoveryDiagnostic> {
    let lakefile_sha256 = sha256_file(lakefile)?;
    let manifest = project_root.join("lake-manifest.json");
    let manifest_sha256 = if manifest.is_file() {
        Some(sha256_file(&manifest)?)
    } else {
        None
    };
    let mut source_max_mtime_ns = 0_u128;
    for module in modules {
        let metadata = fs::metadata(&module.path).map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
            path: module.path.clone(),
            message: "could not stat Lean module source",
            source,
        })?;
        let modified = metadata
            .modified()
            .map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
                path: module.path.clone(),
                message: "could not read Lean module source mtime",
                source,
            })?;
        let mtime_ns = modified
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        source_max_mtime_ns = source_max_mtime_ns.max(mtime_ns);
    }
    Ok(LeanModuleSetFingerprint {
        toolchain,
        lakefile_sha256,
        manifest_sha256,
        source_count: modules.len() as u64,
        source_max_mtime_ns,
    })
}

fn sha256_file(path: &Path) -> Result<String, LeanModuleDiscoveryDiagnostic> {
    fs::read(path)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
            path: path.to_path_buf(),
            message: "could not read file for fingerprinting",
            source,
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        use std::fmt::Write as _;
        if write!(out, "{byte:02x}").is_err() {
            return out;
        }
    }
    out
}

fn module_from_path(project_root: &Path, path: &Path) -> Result<String, LeanModuleDiscoveryDiagnostic> {
    let relative = path
        .strip_prefix(project_root)
        .map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
            path: path.to_path_buf(),
            message: "could not relativize Lean source path",
            source: std::io::Error::other(source),
        })?;
    let mut parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.last_mut()
        && let Some(stripped) = last.strip_suffix(".lean")
    {
        *last = stripped.to_owned();
    }
    Ok(parts.join("."))
}

fn module_to_file(project_root: &Path, module: &str) -> PathBuf {
    let mut path = module_to_dir(project_root, module);
    path.set_extension("lean");
    path
}

fn module_to_dir(project_root: &Path, module: &str) -> PathBuf {
    let mut path = project_root.to_path_buf();
    for part in module.split('.') {
        path.push(part);
    }
    path
}

fn normalize_lake_identifier(raw: &str) -> String {
    raw.trim()
        .trim_matches('`')
        .trim_matches('«')
        .trim_matches('»')
        .trim_matches('"')
        .trim()
        .to_owned()
}

fn validate_module_name(module: &str) -> Result<(), LeanModuleDiscoveryDiagnostic> {
    if module.is_empty() {
        return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
            module: module.to_owned(),
            reason: "module name is empty".to_owned(),
        });
    }
    for component in module.split('.') {
        if component.is_empty() {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module name contains an empty component".to_owned(),
            });
        }
        let mut chars = component.chars();
        let Some(first) = chars.next() else {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module name contains an empty component".to_owned(),
            });
        };
        if !(first == '_' || first.is_alphabetic()) {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module components must begin with a letter or underscore".to_owned(),
            });
        }
        if chars.any(|ch| !(ch == '_' || ch == '\'' || ch.is_alphanumeric())) {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module components may contain only letters, digits, underscores, or apostrophes".to_owned(),
            });
        }
    }
    Ok(())
}

fn read_to_string(path: &Path, message: &'static str) -> Result<String, LeanModuleDiscoveryDiagnostic> {
    fs::read_to_string(path).map_err(|source| LeanModuleDiscoveryDiagnostic::Io {
        path: path.to_path_buf(),
        message,
        source,
    })
}

fn supported_window() -> String {
    lean_rs_sys::SUPPORTED_TOOLCHAINS
        .iter()
        .map(|entry| format!("{:?}", entry.versions))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::wildcard_enum_match_arm
)]
mod tests {
    use super::{
        LeanModuleDiscoveryDiagnostic, LeanModuleDiscoveryOptions, discover_lake_modules, lake_target_declared,
    };
    use crate::ToolchainFingerprint;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn discovers_lean_lakefile_modules_deterministically() {
        let root = temp_project("lean-lakefile");
        write_file(&root.join("lakefile.lean"), "package demo\nlean_lib Demo where\n");
        write_file(&root.join("Demo.lean"), "#check Nat\n");
        fs::create_dir(root.join("Demo")).unwrap();
        write_file(&root.join("Demo").join("B.lean"), "#check String\n");

        let first = discover_lake_modules(LeanModuleDiscoveryOptions::new(&root)).unwrap();
        let second = discover_lake_modules(LeanModuleDiscoveryOptions::new(&root)).unwrap();

        assert_eq!(first.modules, second.modules);
        assert_eq!(module_names(&first), vec!["Demo", "Demo.B"]);
        assert_eq!(first.module_roots, vec!["Demo"]);
        assert_eq!(first.fingerprint.source_count, 2);
    }

    #[test]
    fn discovers_toml_lakefile_roots() {
        let root = temp_project("toml-lakefile");
        write_file(
            &root.join("lakefile.toml"),
            r#"
name = "demo"
[[lean_lib]]
name = "Demo"
[[lean_lib]]
name = "Other"
"#,
        );
        write_file(&root.join("Demo.lean"), "#check Nat\n");
        write_file(&root.join("Other.lean"), "#check String\n");

        let project = discover_lake_modules(LeanModuleDiscoveryOptions::new(&root)).unwrap();
        assert_eq!(project.module_roots, vec!["Demo", "Other"]);
        assert_eq!(module_names(&project), vec!["Demo", "Other"]);
    }

    #[test]
    fn missing_selected_root_is_typed() {
        let root = temp_project("missing-root");
        write_file(&root.join("lakefile.lean"), "package demo\nlean_lib Demo where\n");
        write_file(&root.join("Demo.lean"), "#check Nat\n");

        let err = discover_lake_modules(LeanModuleDiscoveryOptions::new(&root).selected_roots(["Missing"]))
            .expect_err("missing selected root should be typed");
        match err {
            LeanModuleDiscoveryDiagnostic::MissingModuleRoot { module_root, .. } => {
                assert_eq!(module_root, "Missing");
            }
            other => panic!("expected missing module root, got {other:?}"),
        }
    }

    #[test]
    fn invalid_module_name_is_typed() {
        let root = temp_project("invalid-module");
        write_file(&root.join("lakefile.lean"), "package demo\nlean_lib Demo-Bad where\n");
        write_file(&root.join("Demo-Bad.lean"), "#check Nat\n");

        let err = discover_lake_modules(LeanModuleDiscoveryOptions::new(&root))
            .expect_err("invalid Lake module name should be typed");
        match err {
            LeanModuleDiscoveryDiagnostic::InvalidModuleName { module, .. } => {
                assert_eq!(module, "Demo-Bad");
            }
            other => panic!("expected invalid module name, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_toolchain_is_typed() {
        let root = temp_project("unsupported-toolchain");
        write_file(&root.join("lakefile.lean"), "package demo\nlean_lib Demo where\n");
        write_file(&root.join("Demo.lean"), "#check Nat\n");
        let mut fingerprint = ToolchainFingerprint::current();
        fingerprint.lean_version = "0.0.0-test";

        let err = discover_lake_modules(LeanModuleDiscoveryOptions::new(&root).toolchain(fingerprint))
            .expect_err("unsupported toolchain should be typed");
        assert!(matches!(
            err,
            LeanModuleDiscoveryDiagnostic::UnsupportedToolchain { .. }
        ));
    }

    #[test]
    fn target_declaration_probe_is_typed() {
        let root = temp_project("target-probe");
        write_file(
            &root.join("lakefile.lean"),
            "package demo\nlean_lib Demo where\nlean_lib Extra where\n",
        );
        assert!(lake_target_declared(&root, "Demo").unwrap());
        assert!(!lake_target_declared(&root, "Missing").unwrap());
    }

    fn module_names(project: &super::LeanLakeProjectModules) -> Vec<&str> {
        project.modules.iter().map(|module| module.module.as_str()).collect()
    }

    fn temp_project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lean-toolchain-modules-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
}
