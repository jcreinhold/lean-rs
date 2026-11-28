//! Test directories that remove themselves.
//!
//! Each loader test gets its own directory under the system temporary
//! directory, and dropping the guard removes it, so a test run leaves nothing
//! behind.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NONCE: AtomicU64 = AtomicU64::new(0);

/// A test's own directory under the system temporary directory.
///
/// Dropping it removes the directory and everything in it.
pub(super) struct TempDir(PathBuf);

impl TempDir {
    /// The directory's canonical path.
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.0));
    }
}

/// A fresh directory named for `name`, this process and a counter.
pub(super) fn temp_dir(name: &str) -> std::io::Result<TempDir> {
    let mut dir = TempDir(std::env::temp_dir().join(format!(
        "lean-rs-{name}-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    )));
    fs::create_dir_all(&dir.0)?;
    dir.0 = dir.0.canonicalize()?;
    Ok(dir)
}
