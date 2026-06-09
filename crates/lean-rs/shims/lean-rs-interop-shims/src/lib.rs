//! Package-owned Lean source payload for the generic `lean-rs` interop shims.
//!
//! Downstream build scripts use this crate when they need a Lake dependency on
//! `LeanRsInterop` without assuming a sibling `lean-rs` checkout. The helper
//! copies only the Lean/C payload into a caller-owned root and writes a
//! generated `lean-toolchain` for the downstream toolchain.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
/// Returns an I/O error if the source payload cannot be copied, the generated
/// `lean-toolchain` cannot be written, or the materialized root cannot be
/// installed.
pub fn materialize_source_package(
    input: LeanRsInteropShimsSourcePackageRequest,
) -> io::Result<LeanRsInteropShimsSourcePackage> {
    let LeanRsInteropShimsSourcePackageRequest {
        cache_root,
        toolchain_label,
    } = input;
    let root = cache_root.join(sanitize_toolchain_label(&toolchain_label));
    if entry_matches(&root, &toolchain_label) {
        return Ok(LeanRsInteropShimsSourcePackage { project_root: root });
    }

    remove_path_if_exists(&root)?;
    fs::create_dir_all(&cache_root)?;
    let temp = cache_root.join(format!(
        ".lean-rs-interop-shims-{}-{}",
        std::process::id(),
        unique_nanos()
    ));
    remove_path_if_exists(&temp)?;
    fs::create_dir_all(&temp)?;

    copy_file("lakefile.lean", &temp)?;
    copy_file("lake-manifest.json", &temp)?;
    copy_file("LeanRsInterop.lean", &temp)?;
    copy_dir("LeanRsInterop", &temp)?;
    copy_dir("c", &temp)?;
    fs::write(temp.join("lean-toolchain"), format!("{toolchain_label}\n"))?;

    fs::rename(&temp, &root).inspect_err(|_| {
        drop(remove_path_if_exists(&temp));
    })?;
    Ok(LeanRsInteropShimsSourcePackage { project_root: root })
}

fn entry_matches(root: &Path, toolchain_label: &str) -> bool {
    root.join("lakefile.lean").is_file()
        && root.join("lake-manifest.json").is_file()
        && root.join("LeanRsInterop.lean").is_file()
        && root.join("LeanRsInterop/Worker/Stream.lean").is_file()
        && root.join("c/interop_callback.c").is_file()
        && fs::read_to_string(root.join("lean-toolchain")).is_ok_and(|toolchain| toolchain.trim() == toolchain_label)
}

fn copy_file(relative: &str, dest_root: &Path) -> io::Result<()> {
    let dest = dest_root.join(relative);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(Path::new(SOURCE_ROOT).join(relative), dest)?;
    Ok(())
}

fn copy_dir(relative: &str, dest_root: &Path) -> io::Result<()> {
    let source = Path::new(SOURCE_ROOT).join(relative);
    let dest = dest_root.join(relative);
    copy_dir_recursive(&source, &dest)
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &dest_path)?;
        } else if source_path.is_file() {
            fs::copy(&source_path, &dest_path)?;
        }
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
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

    use super::{LeanRsInteropShimsSourcePackageRequest, materialize_source_package};

    #[test]
    fn materializes_source_package_with_generated_toolchain() -> Result<(), String> {
        let temp = std::env::temp_dir().join(format!(
            "lean-rs-interop-shims-test-{}-{}",
            std::process::id(),
            super::unique_nanos()
        ));
        drop(super::remove_path_if_exists(&temp));
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
        assert!(!package.project_root.join("Cargo.toml").exists());
        assert!(!package.project_root.join("src/lib.rs").exists());

        let warm = materialize_source_package(LeanRsInteropShimsSourcePackageRequest {
            cache_root: temp.clone(),
            toolchain_label: toolchain,
        })
        .map_err(|error| error.to_string())?;
        assert_eq!(warm.project_root, package.project_root);

        drop(super::remove_path_if_exists(&temp));
        Ok(())
    }
}
