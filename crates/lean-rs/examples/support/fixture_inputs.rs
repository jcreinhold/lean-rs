//! Private input checks for exclusively provisioned fixture processes.
use sha2::{Digest, Sha256};
use std::{fs, io, path::Path};
pub(super) fn check_digest(path: &Path, approved: Option<&str>) -> io::Result<()> {
    let expected =
        approved.ok_or_else(|| io::Error::other("compile this profiling target with its reviewed fixture digests"))?;
    let hash = Sha256::digest(fs::read(path)?);
    use std::fmt::Write as _;
    let mut actual = String::with_capacity(64);
    for byte in hash {
        write!(actual, "{byte:02x}").map_err(io::Error::other)?;
    }
    if actual != expected {
        return Err(io::Error::other(format!("profiling input changed: {}", path.display())));
    }
    Ok(())
}

pub(super) fn check_prefix() -> io::Result<()> {
    let prefix = Path::new(lean_toolchain::LEAN_HEADER_PATH)
        .ancestors()
        .nth(3)
        .ok_or_else(|| io::Error::other("missing build prefix"))?
        .canonicalize()?;
    if std::env::var_os("LEAN_SYSROOT").as_deref() != Some(prefix.as_os_str())
        || std::env::var_os("LEAN_PATH").is_some()
    {
        return Err(io::Error::other(
            "profiling launcher must pin its build sysroot and clear LEAN_PATH",
        ));
    }
    Ok(())
}

/// Check the same open file that we retain; never infer stability from a path alone.
#[cfg(all(test, unix))]
pub(super) fn open_selected(path: &Path, device: u64, inode: u64, expected: &str) -> io::Result<fs::File> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    if path.canonicalize()? != path {
        return Err(io::Error::other("selected initializer input is aliased"));
    }
    let selected = path.metadata()?;
    if !selected.is_file() || (selected.dev(), selected.ino()) != (device, inode) || selected.len() > 1024 * 1024 * 1024
    {
        return Err(io::Error::other(
            "selected initializer input is not a bounded regular file",
        ));
    }
    let mut file = fs::File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || (before.dev(), before.ino()) != (device, inode) || before.len() > 1024 * 1024 * 1024 {
        return Err(io::Error::other("selected initializer input identity changed"));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut remaining = before.len();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        remaining = remaining
            .checked_sub(u64::try_from(count).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("selected input grew during hashing"))?;
        digest.update(
            buffer
                .get(..count)
                .ok_or_else(|| io::Error::other("invalid read length"))?,
        );
    }
    use std::fmt::Write as _;
    let mut actual = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(&mut actual, "{byte:02x}").map_err(io::Error::other)?;
    }
    let after = file.metadata()?;
    if actual != expected
        || before.len() != after.len()
        || (before.mtime(), before.mtime_nsec(), before.ctime(), before.ctime_nsec())
            != (after.mtime(), after.mtime_nsec(), after.ctime(), after.ctime_nsec())
        || (path.metadata()?.dev(), path.metadata()?.ino()) != (device, inode)
    {
        return Err(io::Error::other("selected initializer input changed"));
    }
    Ok(file)
}

#[cfg(all(test, unix))]
#[test]
fn selected_input_rejects_missing_changed_and_wrong_identity() -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::fs::MetadataExt as _;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _removed = fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir()
        .canonicalize()?
        .join(format!("lean-scalar-input-{}-{nonce}", std::process::id()));
    // Atomic creation refuses collisions; only a successfully created directory is removed.
    fs::DirBuilder::new().mode(0o700).create(&root)?;
    let directory = Scratch(root);
    let path = directory.0.join("image");
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let directory_metadata = directory.0.metadata()?;
    assert!(open_selected(&directory.0, directory_metadata.dev(), directory_metadata.ino(), digest).is_err());
    assert!(open_selected(&path, 0, 0, digest).is_err());
    fs::write(&path, b"abc")?;
    let metadata = path.metadata()?;
    assert!(open_selected(&path, metadata.dev(), metadata.ino(), digest).is_ok());
    assert!(open_selected(&path, metadata.dev(), metadata.ino(), "invalid").is_err());
    assert!(open_selected(&path, metadata.dev(), metadata.ino().wrapping_add(1), digest).is_err());
    fs::write(&path, b"abd")?;
    assert!(open_selected(&path, metadata.dev(), metadata.ino(), digest).is_err());
    fs::remove_file(&path)?;
    fs::write(&path, b"abc")?;
    // Replacement inode reuse is possible; the explicit wrong-identity case above is deterministic.
    assert!(open_selected(&path, metadata.dev(), metadata.ino(), "invalid").is_err());
    Ok(())
}
