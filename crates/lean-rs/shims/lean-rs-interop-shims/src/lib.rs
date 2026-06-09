//! Package-owned Lean source payload for the generic `lean-rs` interop shims.
//!
//! Downstream build scripts use this crate when they need a Lake dependency on
//! `LeanRsInterop` without assuming a sibling `lean-rs` checkout. The helper
//! copies only the Lean/C payload into a caller-owned root and writes a
//! generated `lean-toolchain` for the downstream toolchain.

use std::path::{Path, PathBuf};

use lean_toolchain::{
    SourcePackageError, SourcePackageManifestPolicy, SourcePackageMaterializationRequest,
    materialize_source_package as materialize_with_toolchain,
};
use sha2::{Digest, Sha256};

const SOURCE_ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// Lake package name for the generic interop shims.
pub const PACKAGE_NAME: &str = "lean_rs_interop_shims";
/// Lean library/root module name for the generic interop shims.
pub const LIBRARY_NAME: &str = "LeanRsInterop";

/// Request to materialize the packaged interop shim source for one toolchain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanRsInteropShimsSourcePackageRequest {
    /// Caller-owned root below which the materialized source package is stored.
    pub cache_root: PathBuf,
    /// Lean toolchain label to write into the generated `lean-toolchain`.
    pub toolchain_label: String,
}

/// Materialized interop shim source package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanRsInteropShimsSourcePackage {
    /// Materialized Lake project root.
    pub project_root: PathBuf,
}

/// Materialize the packaged interop shim source package.
///
/// # Errors
///
/// Returns [`SourcePackageError`] if the source payload cannot be copied, the
/// generated `lean-toolchain` cannot be written, the provenance sidecar cannot
/// be validated, or the materialized root cannot be installed.
pub fn materialize_source_package(
    input: LeanRsInteropShimsSourcePackageRequest,
) -> Result<LeanRsInteropShimsSourcePackage, SourcePackageError> {
    let LeanRsInteropShimsSourcePackageRequest {
        cache_root,
        toolchain_label,
    } = input;
    let request = SourcePackageMaterializationRequest {
        source_root: PathBuf::from(SOURCE_ROOT),
        cache_root,
        package_name: PACKAGE_NAME.to_owned(),
        materialized_package_name: PACKAGE_NAME.to_owned(),
        library_name: LIBRARY_NAME.to_owned(),
        source_digest: compute_source_digest()?,
        source_revision: env!("CARGO_PKG_VERSION").to_owned(),
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        crate_version: env!("CARGO_PKG_VERSION").to_owned(),
        toolchain_label,
        include_paths: payload_include_paths(),
        generated_files: Vec::new(),
        sentinel_files: vec![
            PathBuf::from("LeanRsInterop/Worker/Stream.lean"),
            PathBuf::from("c/interop_callback.c"),
        ],
        manifest_policy: SourcePackageManifestPolicy::ZeroPackages,
    };
    let package = materialize_with_toolchain(&request)?;
    Ok(LeanRsInteropShimsSourcePackage {
        project_root: package.project_root,
    })
}

fn payload_include_paths() -> Vec<PathBuf> {
    [
        "lakefile.lean",
        "lake-manifest.json",
        "LeanRsInterop.lean",
        "LeanRsInterop",
        "c",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

fn compute_source_digest() -> Result<String, SourcePackageError> {
    let source_root = Path::new(SOURCE_ROOT);
    let mut entries = Vec::new();
    for include in payload_include_paths() {
        collect_digest_entries(source_root, &include, &mut entries)?;
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut outer = Sha256::new();
    for (canonical_path, digest) in entries {
        outer.update(digest.as_bytes());
        outer.update(b"  ");
        outer.update(canonical_path.as_bytes());
        outer.update(b"\n");
    }
    Ok(hex_lower(&outer.finalize()))
}

fn collect_digest_entries(
    source_root: &Path,
    relative: &Path,
    entries: &mut Vec<(String, String)>,
) -> Result<(), SourcePackageError> {
    let source = source_root.join(relative);
    let metadata = std::fs::symlink_metadata(&source).map_err(|source_error| SourcePackageError::Io {
        action: "stat lean-rs interop shim payload path",
        path: source.clone(),
        source: source_error,
    })?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let dir_entries = std::fs::read_dir(&source).map_err(|source_error| SourcePackageError::Io {
            action: "read lean-rs interop shim payload directory",
            path: source.clone(),
            source: source_error,
        })?;
        for entry in dir_entries {
            let entry = entry.map_err(|source_error| SourcePackageError::Io {
                action: "read lean-rs interop shim payload directory entry",
                path: source.clone(),
                source: source_error,
            })?;
            collect_digest_entries(source_root, &relative.join(entry.file_name()), entries)?;
        }
    } else if metadata.is_file() {
        let bytes = std::fs::read(&source).map_err(|source_error| SourcePackageError::Io {
            action: "read lean-rs interop shim payload file for digest",
            path: source,
            source: source_error,
        })?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        entries.push((relative.to_string_lossy().into_owned(), hex_lower(&hasher.finalize())));
    }
    Ok(())
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "hex encoding indexes a fixed 16-byte table with masked nibbles"
)]
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
}

#[cfg(test)]
fn unique_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{LeanRsInteropShimsSourcePackageRequest, materialize_source_package};

    #[test]
    fn materializes_source_package_with_generated_toolchain() -> Result<(), String> {
        let temp = std::env::temp_dir().join(format!(
            "lean-rs-interop-shims-test-{}-{}",
            std::process::id(),
            super::unique_nanos()
        ));
        drop(remove_path_if_exists(&temp));
        fs::create_dir_all(&temp).map_err(|error| error.to_string())?;
        let toolchain = "leanprover/lean4:v4.31.0-rc1".to_owned();
        let package = materialize_source_package(LeanRsInteropShimsSourcePackageRequest {
            cache_root: temp.clone(),
            toolchain_label: toolchain.clone(),
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(
            fs::read_to_string(package.project_root.join("lean-toolchain"))
                .map_err(|error| error.to_string())?
                .trim(),
            toolchain
        );
        assert!(package.project_root.join("LeanRsInterop/Worker/Stream.lean").is_file());
        assert!(package.project_root.join("c/interop_callback.c").is_file());
        assert!(package.project_root.join("source-package.json").is_file());
        assert!(!package.project_root.join("Cargo.toml").exists());
        assert!(!package.project_root.join("src/lib.rs").exists());

        let warm = materialize_source_package(LeanRsInteropShimsSourcePackageRequest {
            cache_root: temp.clone(),
            toolchain_label: toolchain,
        })
        .map_err(|error| error.to_string())?;
        assert_eq!(warm.project_root, package.project_root);

        drop(remove_path_if_exists(&temp));
        Ok(())
    }

    #[test]
    fn package_list_contains_runtime_payload_and_license_texts() -> Result<(), String> {
        let output = Command::new("cargo")
            .args(["package", "--allow-dirty", "--list"])
            .current_dir(super::SOURCE_ROOT)
            .output()
            .map_err(|error| format!("run cargo package --list: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo package --list failed with status {}:\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let stdout = String::from_utf8(output.stdout).map_err(|error| format!("package list is UTF-8: {error}"))?;

        for expected in [
            "LeanRsInterop.lean",
            "LeanRsInterop/Worker/Stream.lean",
            "c/interop_callback.c",
            "lakefile.lean",
            "lake-manifest.json",
            "LICENSE-APACHE",
            "LICENSE-MIT",
        ] {
            if !stdout.lines().any(|line| line == expected) {
                return Err(format!("package list should include {expected}, got:\n{stdout}"));
            }
        }
        Ok(())
    }

    fn remove_path_if_exists(path: &Path) -> Result<(), String> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(path).map_err(|error| error.to_string())
            }
            Ok(_) => fs::remove_file(path).map_err(|error| error.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}
