//! Close-only controller custody for locks shared with the writer helper.

use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(20);

/// Closing a guard never explicitly unlocks a helper's duplicated description.
pub(crate) struct WriterLock {
    file: File,
    path: PathBuf,
}

impl WriterLock {
    pub(crate) fn acquire(
        path: &Path,
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        Self::acquire_inner(path, timeout, cancelled)
            .map_err(|error| crate::shared_writer::failure(crate::shared_writer::WriterDisposition::NotStarted, error))
    }

    pub(crate) fn acquire_projects(
        root: &Path,
        prepared: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<(Vec<Self>, Vec<PathBuf>)> {
        let started = Instant::now();
        loop {
            let roots = crate::write_domains::workspace_union(root, prepared)?;
            let mut paths = roots
                .iter()
                .map(|root| crate::shared_writer::project_lock_path(root))
                .collect::<io::Result<Vec<_>>>()?;
            paths.sort();
            paths.dedup();
            let mut guards = Vec::new();
            for path in paths {
                guards.push(Self::acquire(
                    &path,
                    timeout.saturating_sub(started.elapsed()),
                    cancelled,
                )?);
            }
            if crate::write_domains::workspace_union(root, prepared)? == roots {
                let primary = crate::shared_writer::project_lock_path(root)?;
                guards.sort_by_key(|guard| guard.path() != primary);
                return Ok((guards, roots));
            }
            drop(guards);
            if started.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "writer dependency graph changed during admission",
                ));
            }
        }
    }

    fn acquire_inner(path: &Path, timeout: Duration, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<Self> {
        let started = Instant::now();
        if cancelled() {
            return Err(cancelled_error());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        loop {
            if cancelled() {
                return Err(cancelled_error());
            }
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => {
                    if cancelled() {
                        return Err(cancelled_error());
                    }
                    #[cfg(unix)]
                    {
                        let opened = file.metadata()?;
                        let named = std::fs::symlink_metadata(path)?;
                        if !opened.is_file()
                            || opened.nlink() != 1
                            || !named.is_file()
                            || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "writer lock inode identity changed",
                            ));
                        }
                    }
                    #[cfg(unix)]
                    loop {
                        let metadata = file.metadata()?;
                        if crate::shared_writer::previous_writer_stopped(&file)?
                            && crate::native_guardian::protected_native_reads_stopped(
                                &crate::native_guardian::registry_directory()?,
                                path,
                                (metadata.dev(), metadata.ino()),
                            )?
                        {
                            break;
                        }
                        if cancelled() {
                            return Err(cancelled_error());
                        }
                        let remaining = timeout.saturating_sub(started.elapsed());
                        if remaining.is_zero() {
                            return Err(io::Error::new(io::ErrorKind::TimedOut, "previous writer remains live"));
                        }
                        thread::sleep(remaining.min(POLL));
                    }
                    return Ok(Self {
                        file,
                        path: path.to_path_buf(),
                    });
                }
                Err(fs4::TryLockError::Error(error)) => return Err(error),
                Err(fs4::TryLockError::WouldBlock) => {}
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "writer lock acquisition timed out",
                ));
            }
            thread::sleep(remaining.min(POLL));
        }
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "writer lock acquisition cancelled")
}

#[cfg(test)]
mod tests;
