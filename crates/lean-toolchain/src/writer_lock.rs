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
    #[cfg(unix)]
    #[allow(
        dead_code,
        reason = "every returned artifact guard retains the complete compatible writer demand"
    )]
    demand: Option<std::sync::Arc<crate::writer_demand::WriterDemand>>,
}

// Never flock, clone or lend this initial file: it pins the original inode
// while independently opened, ungranted EX candidates are closed between tries.
struct LockTarget {
    path: PathBuf,
    anchor: Option<File>,
}
impl LockTarget {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            anchor: None,
        }
    }
    fn candidate(&mut self) -> io::Result<File> {
        if self.anchor.is_none() {
            self.anchor = Some(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&self.path)?,
            );
        }
        // Missing retry targets are errors, never newly created replacements.
        let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        self.validate(&file)?;
        Ok(file)
    }
    fn validate(&self, file: &File) -> io::Result<()> {
        #[cfg(unix)]
        {
            let anchored = self
                .anchor
                .as_ref()
                .ok_or_else(|| io::Error::other("writer lock anchor missing"))?
                .metadata()?;
            let opened = file.metadata()?;
            let named = std::fs::symlink_metadata(&self.path)?;
            if !opened.is_file()
                || opened.nlink() != 1
                || !named.is_file()
                || named.nlink() != 1
                || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
                || (opened.dev(), opened.ino()) != (anchored.dev(), anchored.ino())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "writer lock inode identity changed",
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = file;
        Ok(())
    }
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

    /// Single project-domain admission for source publication. Cache-entry
    /// locks continue to use acquire directly and never announce project demand.
    pub(crate) fn acquire_project(
        path: &Path,
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let started = Instant::now();
            let demand = std::sync::Arc::new(
                crate::writer_demand::WriterDemand::announce(&[path.to_path_buf()], timeout, cancelled).map_err(
                    |error| crate::shared_writer::failure(crate::shared_writer::WriterDisposition::NotStarted, error),
                )?,
            );
            let guard = Self::acquire(path, timeout.saturating_sub(started.elapsed()), cancelled)?;
            Ok(Self {
                demand: Some(demand),
                ..guard
            })
        }
        #[cfg(not(unix))]
        Self::acquire(path, timeout, cancelled)
    }

    pub(crate) fn acquire_projects(
        root: &Path,
        prepared: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<(Vec<Self>, Vec<PathBuf>)> {
        Self::acquire_projects_inner(root, prepared, timeout, cancelled)
            .map_err(|error| crate::shared_writer::failure(crate::shared_writer::WriterDisposition::NotStarted, error))
    }

    fn acquire_projects_inner(
        root: &Path,
        prepared: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<(Vec<Self>, Vec<PathBuf>)> {
        let started = Instant::now();
        let mut targets = std::collections::BTreeMap::new();
        loop {
            let roots = crate::write_domains::workspace_union(root, prepared)?;
            let mut paths = roots
                .iter()
                .map(|root| crate::shared_writer::project_lock_path(root))
                .collect::<io::Result<Vec<_>>>()?;
            paths.sort();
            paths.dedup();
            // Keep original unlocked inode anchors for continuously present
            // domains, without accumulating anchors from former graphs.
            targets.retain(|path, _| paths.binary_search(path).is_ok());
            for path in &paths {
                targets.entry(path.clone()).or_insert_with(|| LockTarget::new(path));
            }
            // Compatible demand covers the complete union, including every
            // retry; it never lends artifact EX to an ungranted attempt.
            #[cfg(unix)]
            let demand = std::sync::Arc::new(crate::writer_demand::WriterDemand::announce(
                &paths,
                timeout.saturating_sub(started.elapsed()),
                cancelled,
            )?);
            let mut guards = loop {
                let mut candidates = Vec::new();
                let mut complete = true;
                for target in targets.values_mut() {
                    match Self::try_acquire(target, cancelled)? {
                        Some(guard) => candidates.push(guard),
                        None => {
                            complete = false;
                            break;
                        }
                    }
                }
                if complete {
                    break candidates;
                }
                // Closing the refused candidate inside try_acquire is not
                // enough: earlier domains must also close before any wait.
                drop(candidates);
                wait_for_retry(started, timeout, cancelled)?;
            };
            if crate::write_domains::workspace_union(root, prepared)? == roots {
                if cancelled() {
                    return Err(cancelled_error());
                }
                #[cfg(unix)]
                for guard in &mut guards {
                    guard.demand = Some(std::sync::Arc::clone(&demand));
                }
                let primary = crate::shared_writer::project_lock_path(root)?;
                guards.sort_by_key(|guard| guard.path() != primary);
                return Ok((guards, roots));
            }
            drop(guards);
            #[cfg(unix)]
            drop(demand);
            if cancelled() {
                return Err(cancelled_error());
            }
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
        let mut target = LockTarget::new(path);
        loop {
            if let Some(guard) = Self::try_acquire(&mut target, cancelled)? {
                return Ok(guard);
            }
            wait_for_retry(started, timeout, cancelled)?;
        }
    }

    // A single fresh-open attempt. Only actual flock contention or an explicit
    // false owner check means busy; record, identity and OS errors propagate.
    fn try_acquire(target: &mut LockTarget, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<Option<Self>> {
        if cancelled() {
            return Err(cancelled_error());
        }
        let file = target.candidate()?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(fs4::TryLockError::Error(error)) => return Err(error),
            Err(fs4::TryLockError::WouldBlock) => {
                #[cfg(all(test, unix))]
                tests::admission_wait(&target.path, "kernel-busy");
                return Ok(None);
            }
        }
        if cancelled() {
            return Err(cancelled_error());
        }
        target.validate(&file)?;
        #[cfg(unix)]
        {
            let metadata = file.metadata()?;
            if !crate::shared_writer::previous_writer_stopped(&file)?
                || !crate::native_guardian::protected_native_reads_stopped(
                    &crate::native_guardian::registry_directory()?,
                    &target.path,
                    (metadata.dev(), metadata.ino()),
                )?
            {
                #[cfg(test)]
                tests::admission_wait(&target.path, "owner-refused");
                return Ok(None);
            }
        }
        target.validate(&file)?;
        if cancelled() {
            return Err(cancelled_error());
        }
        Ok(Some(Self {
            file,
            path: target.path.clone(),
            #[cfg(unix)]
            demand: None,
        }))
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn wait_for_retry(started: Instant, timeout: Duration, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<()> {
    if cancelled() {
        return Err(cancelled_error());
    }
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "writer lock acquisition timed out",
        ));
    }
    thread::sleep(remaining.min(POLL));
    if cancelled() {
        return Err(cancelled_error());
    }
    if started.elapsed() >= timeout {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "writer lock acquisition timed out",
        ));
    }
    Ok(())
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "writer lock acquisition cancelled")
}

#[cfg(test)]
mod tests;
