//! The bundled shim packages' sources, compiled into this crate.
//!
//! A worker built from a registry copy of this crate should not build in the
//! Cargo source cache, and an installed worker should not depend on that cache
//! staying put. [`write`](fn@write) puts the exact sources this build links
//! against into a fresh directory, so an installer can build them there.

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

macro_rules! shim_sources {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/shims/", $path)))),*]
    };
}

/// Each bundled shim file, by its path below `shims/`: the files Cargo packages
/// from there (see `include` in `Cargo.toml`).
const BUNDLED_SHIM_SOURCES: &[(&str, &[u8])] = shim_sources![
    "lean-rs-host-shims/LeanRsHostShims.lean",
    "lean-rs-host-shims/LeanRsHostShims/Elaboration.lean",
    "lean-rs-host-shims/LeanRsHostShims/Environment.lean",
    "lean-rs-host-shims/LeanRsHostShims/InfoTree.lean",
    "lean-rs-host-shims/LeanRsHostShims/Interop.lean",
    "lean-rs-host-shims/LeanRsHostShims/Meta.lean",
    "lean-rs-host-shims/lake-manifest.json",
    "lean-rs-host-shims/lakefile.lean",
    "lean-rs-host-shims/lean-toolchain",
    "lean-rs-interop-shims/LeanRsInterop.lean",
    "lean-rs-interop-shims/LeanRsInterop/Callback.lean",
    "lean-rs-interop-shims/LeanRsInterop/Callback/String.lean",
    "lean-rs-interop-shims/LeanRsInterop/Callback/Tick.lean",
    "lean-rs-interop-shims/c/interop_callback.c",
    "lean-rs-interop-shims/lake-manifest.json",
    "lean-rs-interop-shims/lakefile.lean",
    "lean-rs-interop-shims/lean-toolchain",
];

/// Write both shim packages into `destination`, which must not exist yet.
///
/// # Errors
/// Returns an existing `destination`, a missing parent, or a write failure.
pub(crate) fn write(destination: &Path) -> io::Result<()> {
    fs::create_dir(destination)?;
    for (path, bytes) in BUNDLED_SHIM_SOURCES {
        let file = destination.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)?
            .write_all(bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io;
    use std::path::{Path, PathBuf};

    /// Add the files Cargo packages from `dir`, a directory below `shims`.
    fn packaged(shims: &Path, dir: &Path, found: &mut BTreeSet<String>) -> io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let utf8 = |text: Option<&str>| {
                text.map(str::to_owned)
                    .ok_or_else(|| io::Error::other("non-UTF-8 path"))
            };
            let name = utf8(path.file_name().and_then(|name| name.to_str()))?;
            if path.is_dir() {
                if name != ".lake" {
                    packaged(shims, &path, found)?;
                }
                continue;
            }
            let relative = utf8(path.strip_prefix(shims).map_err(io::Error::other)?.to_str())?;
            let extension = Path::new(&name).extension().and_then(|ext| ext.to_str());
            let kept = matches!(name.as_str(), "lake-manifest.json" | "lakefile.lean" | "lean-toolchain")
                || extension == Some("lean")
                || (extension == Some("c") && relative.split('/').any(|part| part == "c"));
            if kept {
                found.insert(relative);
            }
        }
        Ok(())
    }

    #[test]
    fn the_table_lists_every_packaged_shim_file() -> io::Result<()> {
        let shims = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shims");
        let mut on_disk = BTreeSet::new();
        for package in ["lean-rs-host-shims", "lean-rs-interop-shims"] {
            packaged(&shims, &shims.join(package), &mut on_disk)?;
        }
        let listed: BTreeSet<String> = super::BUNDLED_SHIM_SOURCES
            .iter()
            .map(|(path, _)| (*path).to_owned())
            .collect();
        assert_eq!(listed, on_disk, "update BUNDLED_SHIM_SOURCES to match shims/");
        Ok(())
    }

    #[test]
    fn write_copies_every_file_and_refuses_an_existing_destination() -> io::Result<()> {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-state")
            .join(format!("shim-sources-{}", std::process::id()));
        std::fs::create_dir_all(&parent)?;
        let destination = parent.join("shims");
        super::write(&destination)?;
        for (path, bytes) in super::BUNDLED_SHIM_SOURCES {
            assert_eq!(std::fs::read(destination.join(path))?, *bytes, "{path}");
        }
        assert!(super::write(&destination).is_err(), "a second write must refuse");
        std::fs::remove_dir_all(&parent)
    }
}
