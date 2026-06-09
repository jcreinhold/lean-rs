//! Source-package materialization for package-owned Lean payload crates.
//!
//! This module owns the cache mechanics that are easy to get subtly wrong in
//! downstream `build.rs` helpers: digest-keyed directories, per-entry locking,
//! atomic installation, generated `lean-toolchain` files, provenance sidecars,
//! and zero-dependency Lake manifest validation. It does not know how to build
//! Lake targets; callers pass the returned root to [`CargoLeanCapability`] or
//! another build helper.
//!
//! [`CargoLeanCapability`]: crate::CargoLeanCapability

use std::error::Error as StdError;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const CACHE_SCHEMA_VERSION: u32 = 1;
const SIDECAR_FILE_NAME: &str = "source-package.json";

/// Request to materialize a package-owned Lean source payload.
///
/// The source digest is the cache identity. The other provenance fields make a
/// warm entry self-describing and force rematerialization when caller-visible
/// ownership metadata changes while the payload bytes happen to remain stable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePackageMaterializationRequest {
    /// Directory containing the packaged source payload.
    pub source_root: PathBuf,
    /// Caller-owned cache root. The helper owns the layout below it.
    pub cache_root: PathBuf,
    /// Public upstream Lake package name.
    pub package_name: String,
    /// Lake package identifier in the materialized root.
    pub materialized_package_name: String,
    /// Lean library/root module name.
    pub library_name: String,
    /// Digest of the payload source rule chosen by the caller.
    pub source_digest: String,
    /// Source revision, vendoring revision, or another stable source identity.
    pub source_revision: String,
    /// Rust crate that owns this payload.
    pub crate_name: String,
    /// Rust crate version that owns this payload.
    pub crate_version: String,
    /// Lean toolchain label to write into the generated `lean-toolchain`.
    pub toolchain_label: String,
    /// Source-root-relative files or directories to copy recursively.
    pub include_paths: Vec<PathBuf>,
    /// Files written after copying. These can override copied files when the
    /// caller owns generated Lake metadata.
    pub generated_files: Vec<GeneratedSourceFile>,
    /// Files that must exist in a valid warm cache entry.
    pub sentinel_files: Vec<PathBuf>,
}

/// Source-root-relative generated file written during materialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedSourceFile {
    /// Destination path relative to the materialized source root.
    pub relative_path: PathBuf,
    /// Complete file contents.
    pub contents: Vec<u8>,
}

/// Materialized source package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedSourcePackage {
    /// Materialized Lake project root.
    pub project_root: PathBuf,
    /// Provenance recorded beside the materialized source root.
    pub provenance: SourcePackageProvenance,
}

/// Provenance sidecar for a materialized source package.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourcePackageProvenance {
    /// Sidecar schema version.
    pub schema_version: u32,
    /// Public upstream Lake package name.
    pub package_name: String,
    /// Lake package identifier in the materialized root.
    pub materialized_package_name: String,
    /// Lean library/root module name.
    pub library_name: String,
    /// Caller-supplied payload digest.
    pub source_digest: String,
    /// Caller-supplied source identity.
    pub source_revision: String,
    /// Rust crate that owns the payload.
    pub crate_name: String,
    /// Rust crate version that owns the payload.
    pub crate_version: String,
    /// Requested Lean toolchain label.
    pub toolchain_label: String,
    /// Whether the helper generated the materialized `lean-toolchain`.
    pub generated_toolchain_file: bool,
}

impl SourcePackageProvenance {
    fn new(input: &SourcePackageMaterializationRequest) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            package_name: input.package_name.clone(),
            materialized_package_name: input.materialized_package_name.clone(),
            library_name: input.library_name.clone(),
            source_digest: input.source_digest.clone(),
            source_revision: input.source_revision.clone(),
            crate_name: input.crate_name.clone(),
            crate_version: input.crate_version.clone(),
            toolchain_label: input.toolchain_label.clone(),
            generated_toolchain_file: true,
        }
    }
}

/// Source package materialization errors.
#[derive(Debug)]
pub enum SourcePackageError {
    /// Filesystem operation failed.
    Io {
        /// Operation being attempted.
        action: &'static str,
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// JSON sidecar or manifest operation failed.
    Json {
        /// Operation being attempted.
        action: &'static str,
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying JSON error.
        source: serde_json::Error,
    },
    /// The caller supplied an invalid materialization request or payload.
    InvalidPayload(String),
}

impl fmt::Display for SourcePackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { action, path, source } => {
                write!(f, "{action} {}: {source}", path.display())
            }
            Self::Json { action, path, source } => {
                write!(f, "{action} {}: {source}", path.display())
            }
            Self::InvalidPayload(message) => write!(f, "invalid source package payload: {message}"),
        }
    }
}

impl StdError for SourcePackageError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::InvalidPayload(_) => None,
        }
    }
}

/// Materialize a package-owned Lean source payload into a digest-keyed cache.
///
/// # Errors
///
/// Returns [`SourcePackageError`] when the request is invalid, the payload is
/// not a zero-dependency Lake package, a filesystem operation fails, or the
/// provenance sidecar cannot be decoded.
pub fn materialize_source_package(
    input: &SourcePackageMaterializationRequest,
) -> Result<MaterializedSourcePackage, SourcePackageError> {
    validate_request(input)?;
    let provenance = SourcePackageProvenance::new(input);
    let cache = SourcePackageCache::new(input.cache_root.clone(), input.source_digest.clone());
    let _lock = cache.lock_entry(&input.toolchain_label)?;
    cache.ensure_materialized_locked(input, provenance)
}

struct SourcePackageCache {
    cache_root: PathBuf,
    source_digest: String,
}

impl SourcePackageCache {
    fn new(cache_root: PathBuf, source_digest: String) -> Self {
        Self {
            cache_root,
            source_digest,
        }
    }

    fn digest_root(&self) -> PathBuf {
        self.cache_root.join(&self.source_digest)
    }

    fn entry_root(&self, toolchain_label: &str) -> PathBuf {
        self.digest_root().join(sanitize_toolchain_label(toolchain_label))
    }

    fn lock_entry(&self, toolchain_label: &str) -> Result<EntryLock, SourcePackageError> {
        let digest_root = self.digest_root();
        create_dir_all(&digest_root, "create source package cache digest directory")?;
        let path = digest_root.join(format!("{}.lock", sanitize_toolchain_label(toolchain_label)));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| SourcePackageError::Io {
                action: "open source package cache lock",
                path: path.clone(),
                source,
            })?;
        fs4::FileExt::lock(&file).map_err(|source| SourcePackageError::Io {
            action: "lock source package cache entry",
            path,
            source,
        })?;
        Ok(EntryLock { _file: file })
    }

    fn ensure_materialized_locked(
        &self,
        input: &SourcePackageMaterializationRequest,
        provenance: SourcePackageProvenance,
    ) -> Result<MaterializedSourcePackage, SourcePackageError> {
        let root = self.entry_root(&input.toolchain_label);
        if entry_matches(&root, input, &provenance)? {
            return Ok(MaterializedSourcePackage {
                project_root: root,
                provenance,
            });
        }

        remove_path_if_exists(&root, "remove stale source package cache entry")?;
        clean_stale_temps(&self.digest_root())?;

        let temp = self
            .digest_root()
            .join(format!(".source-package-{}-{}", std::process::id(), unique_nanos()));
        remove_path_if_exists(&temp, "remove stale source package temp directory")?;
        create_dir_all(&temp, "create source package temp directory")?;

        copy_payload(input, &temp)?;
        for generated in &input.generated_files {
            write_file(
                &temp.join(&generated.relative_path),
                &generated.contents,
                "write generated source package file",
            )?;
        }
        write_file(
            &temp.join("lean-toolchain"),
            format!("{}\n", input.toolchain_label).as_bytes(),
            "write generated source package lean-toolchain",
        )?;
        write_sidecar(&temp, &provenance)?;
        ensure_zero_package_manifest(&temp.join("lake-manifest.json"))?;
        for sentinel in &input.sentinel_files {
            let path = temp.join(sentinel);
            if !path.is_file() {
                return Err(SourcePackageError::InvalidPayload(format!(
                    "materialized source package missing sentinel file {}",
                    sentinel.display()
                )));
            }
        }

        fs::rename(&temp, &root).map_err(|source| {
            drop(remove_path_if_exists(
                &temp,
                "remove failed source package temp directory",
            ));
            SourcePackageError::Io {
                action: "install source package cache entry",
                path: root.clone(),
                source,
            }
        })?;
        Ok(MaterializedSourcePackage {
            project_root: root,
            provenance,
        })
    }
}

struct EntryLock {
    _file: File,
}

fn validate_request(input: &SourcePackageMaterializationRequest) -> Result<(), SourcePackageError> {
    if input.source_digest.is_empty() {
        return Err(SourcePackageError::InvalidPayload(
            "source digest must be non-empty".to_owned(),
        ));
    }
    if !input
        .source_digest
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        return Err(SourcePackageError::InvalidPayload(
            "source digest must be a single cache-safe path component".to_owned(),
        ));
    }
    for (field, value) in [
        ("package name", &input.package_name),
        ("materialized package name", &input.materialized_package_name),
        ("library name", &input.library_name),
        ("source revision", &input.source_revision),
        ("crate name", &input.crate_name),
        ("crate version", &input.crate_version),
        ("toolchain label", &input.toolchain_label),
    ] {
        if value.is_empty() {
            return Err(SourcePackageError::InvalidPayload(format!("{field} must be non-empty")));
        }
    }
    if input.include_paths.is_empty() {
        return Err(SourcePackageError::InvalidPayload(
            "at least one include path is required".to_owned(),
        ));
    }
    for relative in input
        .include_paths
        .iter()
        .chain(input.sentinel_files.iter())
        .chain(input.generated_files.iter().map(|file| &file.relative_path))
    {
        validate_relative_path(relative)?;
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), SourcePackageError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(SourcePackageError::InvalidPayload(format!(
            "source package path must be relative: {}",
            path.display()
        )));
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(SourcePackageError::InvalidPayload(format!(
                "source package path must stay inside the source root: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn entry_matches(
    root: &Path,
    input: &SourcePackageMaterializationRequest,
    expected: &SourcePackageProvenance,
) -> Result<bool, SourcePackageError> {
    if !root.is_dir() {
        return Ok(false);
    }
    for relative in input.sentinel_files.iter().map(PathBuf::as_path).chain([
        Path::new("lake-manifest.json"),
        Path::new("lean-toolchain"),
        Path::new(SIDECAR_FILE_NAME),
    ]) {
        if !root.join(relative).is_file() {
            return Ok(false);
        }
    }
    let sidecar_path = root.join(SIDECAR_FILE_NAME);
    let provenance: SourcePackageProvenance =
        match serde_json::from_slice(&read_file(&sidecar_path, "read source package provenance sidecar")?) {
            Ok(provenance) => provenance,
            Err(_) => return Ok(false),
        };
    if &provenance != expected {
        return Ok(false);
    }
    let toolchain = read_to_string(
        &root.join("lean-toolchain"),
        "read generated source package lean-toolchain",
    )?;
    if toolchain.trim() != input.toolchain_label {
        return Ok(false);
    }
    for generated in &input.generated_files {
        let path = root.join(&generated.relative_path);
        let bytes = read_file(&path, "read generated source package file")?;
        if bytes != generated.contents {
            return Ok(false);
        }
    }
    ensure_zero_package_manifest(&root.join("lake-manifest.json"))?;
    Ok(true)
}

fn copy_payload(input: &SourcePackageMaterializationRequest, dest_root: &Path) -> Result<(), SourcePackageError> {
    for include in &input.include_paths {
        let source = input.source_root.join(include);
        if !source.exists() {
            return Err(SourcePackageError::InvalidPayload(format!(
                "include path {} does not exist under {}",
                include.display(),
                input.source_root.display()
            )));
        }
        let dest = dest_root.join(include);
        copy_path_recursive(&source, &dest)?;
    }
    Ok(())
}

fn copy_path_recursive(source: &Path, dest: &Path) -> Result<(), SourcePackageError> {
    let metadata = fs::symlink_metadata(source).map_err(|source_error| SourcePackageError::Io {
        action: "stat source package payload path",
        path: source.to_path_buf(),
        source: source_error,
    })?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        create_dir_all(dest, "create source package payload directory")?;
        let entries = fs::read_dir(source).map_err(|source_error| SourcePackageError::Io {
            action: "read source package payload directory",
            path: source.to_path_buf(),
            source: source_error,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source_error| SourcePackageError::Io {
                action: "read source package payload directory entry",
                path: source.to_path_buf(),
                source: source_error,
            })?;
            copy_path_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        if let Some(parent) = dest.parent() {
            create_dir_all(parent, "create source package payload parent directory")?;
        }
        fs::copy(source, dest).map_err(|source_error| SourcePackageError::Io {
            action: "copy source package payload file",
            path: dest.to_path_buf(),
            source: source_error,
        })?;
    } else {
        return Err(SourcePackageError::InvalidPayload(format!(
            "source package payload path is not a regular file or directory: {}",
            source.display()
        )));
    }
    Ok(())
}

fn write_sidecar(root: &Path, provenance: &SourcePackageProvenance) -> Result<(), SourcePackageError> {
    let path = root.join(SIDECAR_FILE_NAME);
    let bytes = serde_json::to_vec_pretty(provenance).map_err(|source| SourcePackageError::Json {
        action: "encode source package provenance sidecar",
        path: path.clone(),
        source,
    })?;
    write_file(&path, &bytes, "write source package provenance sidecar")
}

fn ensure_zero_package_manifest(path: &Path) -> Result<(), SourcePackageError> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&read_file(path, "read source package lake-manifest.json")?).map_err(|source| {
            SourcePackageError::Json {
                action: "decode source package lake-manifest.json",
                path: path.to_path_buf(),
                source,
            }
        })?;
    let packages = manifest
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            SourcePackageError::InvalidPayload("lake-manifest.json must contain an array `packages`".to_owned())
        })?;
    if !packages.is_empty() {
        return Err(SourcePackageError::InvalidPayload(
            "lake-manifest.json must remain zero-dependency (`packages: []`)".to_owned(),
        ));
    }
    Ok(())
}

fn clean_stale_temps(parent: &Path) -> Result<(), SourcePackageError> {
    if !parent.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(parent).map_err(|source| SourcePackageError::Io {
        action: "read source package cache directory",
        path: parent.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SourcePackageError::Io {
            action: "read source package cache directory entry",
            path: parent.to_path_buf(),
            source,
        })?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name.starts_with(".source-package-") {
            remove_path_if_exists(&entry.path(), "remove stale source package temp path")?;
        }
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path, action: &'static str) -> Result<(), SourcePackageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|source| SourcePackageError::Io {
                action,
                path: path.to_path_buf(),
                source,
            })
        }
        Ok(_) => fs::remove_file(path).map_err(|source| SourcePackageError::Io {
            action,
            path: path.to_path_buf(),
            source,
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SourcePackageError::Io {
            action,
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn create_dir_all(path: &Path, action: &'static str) -> Result<(), SourcePackageError> {
    fs::create_dir_all(path).map_err(|source| SourcePackageError::Io {
        action,
        path: path.to_path_buf(),
        source,
    })
}

fn read_file(path: &Path, action: &'static str) -> Result<Vec<u8>, SourcePackageError> {
    fs::read(path).map_err(|source| SourcePackageError::Io {
        action,
        path: path.to_path_buf(),
        source,
    })
}

fn read_to_string(path: &Path, action: &'static str) -> Result<String, SourcePackageError> {
    fs::read_to_string(path).map_err(|source| SourcePackageError::Io {
        action,
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8], action: &'static str) -> Result<(), SourcePackageError> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent, "create source package output parent directory")?;
    }
    fs::write(path, bytes).map_err(|source| SourcePackageError::Io {
        action,
        path: path.to_path_buf(),
        source,
    })
}

fn sanitize_toolchain_label(label: &str) -> String {
    label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn unique_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread;

    use super::{
        GeneratedSourceFile, SourcePackageError, SourcePackageMaterializationRequest, materialize_source_package,
        remove_path_if_exists,
    };

    fn temp_root(name: &str) -> Result<PathBuf, String> {
        let root = std::env::temp_dir().join(format!(
            "lean-toolchain-source-package-{name}-{}-{}",
            std::process::id(),
            super::unique_nanos()
        ));
        drop(remove_path_if_exists(&root, "remove old source package test temp root"));
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(root)
    }

    fn write(path: &Path, contents: &str) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(path, contents).map_err(|error| error.to_string())
    }

    fn source_root(name: &str) -> Result<PathBuf, String> {
        let root = temp_root(name)?;
        write(
            &root.join("lakefile.lean"),
            "import Lake\nopen Lake DSL\npackage test_pkg\n",
        )?;
        write(
            &root.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"test_pkg","lakeDir":".lake"}"#,
        )?;
        write(&root.join("Test.lean"), "def answer := 42\n")?;
        write(&root.join("Test/Extra.lean"), "def extra := 43\n")?;
        Ok(root)
    }

    fn request(source_root: PathBuf, cache_root: PathBuf, digest: &str) -> SourcePackageMaterializationRequest {
        SourcePackageMaterializationRequest {
            source_root,
            cache_root,
            package_name: "test-pkg".to_owned(),
            materialized_package_name: "test_pkg".to_owned(),
            library_name: "Test".to_owned(),
            source_digest: digest.to_owned(),
            source_revision: "test-revision".to_owned(),
            crate_name: "test-crate".to_owned(),
            crate_version: "0.0.0".to_owned(),
            toolchain_label: "leanprover/lean4:v4.31.0-rc1".to_owned(),
            include_paths: vec![
                PathBuf::from("lakefile.lean"),
                PathBuf::from("lake-manifest.json"),
                PathBuf::from("Test.lean"),
                PathBuf::from("Test"),
            ],
            generated_files: Vec::new(),
            sentinel_files: vec![PathBuf::from("Test/Extra.lean")],
        }
    }

    fn read_string(path: &Path) -> Result<String, String> {
        fs::read_to_string(path).map_err(|error| error.to_string())
    }

    #[test]
    fn materialization_writes_generated_toolchain_and_provenance() -> Result<(), String> {
        let source = source_root("writes")?;
        let cache = temp_root("cache-writes")?;
        let package =
            materialize_source_package(&request(source, cache, "digest-a")).map_err(|error| error.to_string())?;

        assert_eq!(
            read_string(&package.project_root.join("lean-toolchain"))?.trim(),
            "leanprover/lean4:v4.31.0-rc1"
        );
        assert!(package.project_root.join("Test/Extra.lean").is_file());
        assert_eq!(package.provenance.source_digest, "digest-a");
        assert_eq!(package.provenance.package_name, "test-pkg");
        assert!(package.provenance.generated_toolchain_file);
        assert!(package.project_root.join(super::SIDECAR_FILE_NAME).is_file());
        drop(remove_path_if_exists(
            &package.project_root,
            "remove source package test output",
        ));
        Ok(())
    }

    #[test]
    fn cache_path_changes_when_digest_changes() -> Result<(), String> {
        let source = source_root("digest")?;
        let cache = temp_root("cache-digest")?;
        let first = materialize_source_package(&request(source.clone(), cache.clone(), "digest-a"))
            .map_err(|error| error.to_string())?;
        let second =
            materialize_source_package(&request(source, cache, "digest-b")).map_err(|error| error.to_string())?;
        assert_ne!(first.project_root, second.project_root);
        assert!(
            first
                .project_root
                .components()
                .any(|component| component.as_os_str() == "digest-a")
        );
        assert!(
            second
                .project_root
                .components()
                .any(|component| component.as_os_str() == "digest-b")
        );
        Ok(())
    }

    #[test]
    fn warm_materialization_reuses_valid_entry() -> Result<(), String> {
        let source = source_root("warm")?;
        let cache = temp_root("cache-warm")?;
        let input = request(source, cache, "digest-warm");
        let first = materialize_source_package(&input).map_err(|error| error.to_string())?;
        let marker = first.project_root.join("warm-marker");
        fs::write(&marker, b"warm").map_err(|error| error.to_string())?;

        let second = materialize_source_package(&input).map_err(|error| error.to_string())?;
        assert_eq!(first.project_root, second.project_root);
        assert!(marker.is_file(), "warm cache entry should not be recopied");
        Ok(())
    }

    #[test]
    fn stale_sidecar_rematerializes_entry() -> Result<(), String> {
        let source = source_root("sidecar")?;
        let cache = temp_root("cache-sidecar")?;
        let input = request(source, cache, "digest-sidecar");
        let first = materialize_source_package(&input).map_err(|error| error.to_string())?;
        let marker = first.project_root.join("warm-marker");
        fs::write(&marker, b"warm").map_err(|error| error.to_string())?;
        fs::write(first.project_root.join(super::SIDECAR_FILE_NAME), b"{}").map_err(|error| error.to_string())?;

        let second = materialize_source_package(&input).map_err(|error| error.to_string())?;
        assert_eq!(first.project_root, second.project_root);
        assert!(!marker.exists(), "stale sidecar should force rematerialization");
        Ok(())
    }

    #[test]
    fn mismatched_toolchain_rematerializes_entry() -> Result<(), String> {
        let source = source_root("toolchain")?;
        let cache = temp_root("cache-toolchain")?;
        let input = request(source, cache, "digest-toolchain");
        let first = materialize_source_package(&input).map_err(|error| error.to_string())?;
        let marker = first.project_root.join("warm-marker");
        fs::write(&marker, b"warm").map_err(|error| error.to_string())?;
        fs::write(first.project_root.join("lean-toolchain"), b"other-toolchain\n")
            .map_err(|error| error.to_string())?;

        let second = materialize_source_package(&input).map_err(|error| error.to_string())?;
        assert_eq!(first.project_root, second.project_root);
        assert!(!marker.exists(), "mismatched toolchain should force rematerialization");
        Ok(())
    }

    #[test]
    fn zero_package_manifest_invariant_is_enforced() -> Result<(), String> {
        let source = source_root("nonzero-manifest")?;
        write(
            &source.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[{"name":"dep"}],"name":"test_pkg","lakeDir":".lake"}"#,
        )?;
        let cache = temp_root("cache-nonzero-manifest")?;
        let error = match materialize_source_package(&request(source, cache, "digest-nonzero")) {
            Ok(package) => {
                return Err(format!(
                    "nonzero Lake packages should fail, materialized {}",
                    package.project_root.display()
                ));
            }
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("zero-dependency"),
            "error should explain the zero-package invariant: {error}"
        );
        Ok(())
    }

    #[test]
    fn concurrent_first_materialization_serializes_same_entry() -> Result<(), String> {
        let source = source_root("concurrent")?;
        let cache = temp_root("cache-concurrent")?;
        let handles = (0..8)
            .map(|_| {
                let input = request(source.clone(), cache.clone(), "digest-concurrent");
                thread::spawn(move || {
                    materialize_source_package(&input)
                        .map_err(|error| error.to_string())
                        .map(|package| package.project_root)
                })
            })
            .collect::<Vec<_>>();

        let mut roots = Vec::new();
        for handle in handles {
            let root = handle
                .join()
                .map_err(|_| "materialization thread panicked".to_owned())??;
            roots.push(root);
        }
        let first = roots
            .first()
            .ok_or_else(|| "expected at least one materialized root".to_owned())?;
        assert!(roots.iter().all(|root| root == first));
        assert!(first.join("Test/Extra.lean").is_file());
        assert!(first.join(super::SIDECAR_FILE_NAME).is_file());
        Ok(())
    }

    #[test]
    fn generated_files_can_override_lake_metadata() -> Result<(), String> {
        let source = source_root("generated")?;
        let cache = temp_root("cache-generated")?;
        let mut input = request(source, cache, "digest-generated");
        input.generated_files = vec![
            GeneratedSourceFile {
                relative_path: PathBuf::from("lakefile.lean"),
                contents: b"import Lake\nopen Lake DSL\npackage generated_pkg\n".to_vec(),
            },
            GeneratedSourceFile {
                relative_path: PathBuf::from("lake-manifest.json"),
                contents: br#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"generated_pkg","lakeDir":".lake"}"#.to_vec(),
            },
        ];
        let package = materialize_source_package(&input).map_err(|error| error.to_string())?;
        assert!(read_string(&package.project_root.join("lakefile.lean"))?.contains("generated_pkg"));
        Ok(())
    }

    #[test]
    fn generated_file_mismatch_rematerializes_entry() -> Result<(), String> {
        let source = source_root("generated-mismatch")?;
        let cache = temp_root("cache-generated-mismatch")?;
        let mut input = request(source, cache, "digest-generated-mismatch");
        input.generated_files = vec![GeneratedSourceFile {
            relative_path: PathBuf::from("generated.txt"),
            contents: b"expected".to_vec(),
        }];
        let first = materialize_source_package(&input).map_err(|error| error.to_string())?;
        let marker = first.project_root.join("warm-marker");
        fs::write(&marker, b"warm").map_err(|error| error.to_string())?;
        fs::write(first.project_root.join("generated.txt"), b"stale").map_err(|error| error.to_string())?;

        let second = materialize_source_package(&input).map_err(|error| error.to_string())?;
        assert_eq!(first.project_root, second.project_root);
        assert!(
            !marker.exists(),
            "generated file mismatch should force rematerialization"
        );
        assert_eq!(read_string(&second.project_root.join("generated.txt"))?, "expected");
        Ok(())
    }

    #[test]
    fn error_messages_include_action_and_path() -> Result<(), String> {
        let source = source_root("missing")?;
        let cache = temp_root("cache-missing")?;
        let mut input = request(source, cache, "digest-missing");
        input.include_paths.push(PathBuf::from("Missing.lean"));
        let error = match materialize_source_package(&input) {
            Ok(package) => {
                return Err(format!(
                    "missing include path should fail, materialized {}",
                    package.project_root.display()
                ));
            }
            Err(error) => error,
        };
        assert!(
            matches!(error, SourcePackageError::InvalidPayload(_)),
            "missing include path should be an invalid payload error: {error}"
        );

        let bad_cache_file = temp_root("cache-file")?.join("not-a-directory");
        fs::write(&bad_cache_file, b"not a directory").map_err(|error| error.to_string())?;
        let source = source_root("io-path")?;
        let error = match materialize_source_package(&request(source, bad_cache_file, "digest-io")) {
            Ok(package) => {
                return Err(format!(
                    "cache root file should fail, materialized {}",
                    package.project_root.display()
                ));
            }
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(
            text.contains("create source package cache digest directory") && text.contains("not-a-directory"),
            "I/O error should include action and path: {text}"
        );
        Ok(())
    }
}
