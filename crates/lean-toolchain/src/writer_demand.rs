//! Close-only writer demand on stable sibling files, separate from artifact locks.
//!
//! Every waiting writer holds compatible SH demand over its complete domain
//! union before taking any artifact EX. A reader may briefly hold EX demand to
//! try its actual artifact SH locks, but must drop this gate before waiting on
//! an artifact lock or helper. Demand files are never unlinked or unlocked.
use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(10);

/// Compatible writer demand retained until every contained descriptor closes.
#[derive(Debug)]
#[must_use = "retain demand throughout writer admission and owned execution"]
pub struct WriterDemand {
    #[allow(
        dead_code,
        reason = "the open descriptions retain SH demand until close-only guard drop"
    )]
    files: Vec<File>,
}
/// A transient reader gate; never retain it across blocking artifact admission.
#[derive(Debug)]
#[must_use = "retain only while trying actual SH locks or cloning a cached reader"]
pub struct ReadAdmission {
    #[allow(
        dead_code,
        reason = "the open descriptions retain EX admission gates until close-only guard drop"
    )]
    files: Vec<File>,
}
impl WriterDemand {
    /// Announce all canonical project-lock domains before taking any artifact EX.
    ///
    /// Every writer uses SH, so waiting writers cannot exclude one another.
    /// The shared budget covers transient reader-gate contention across all
    /// sorted domains. Dropping closes files without an explicit unlock.
    /// # Errors
    /// Returns invalid path/inode, cancellation, timeout or lock errors.
    pub fn announce(
        lock_paths: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        let started = Instant::now();
        if cancelled() {
            return Err(cancelled_error());
        }
        let paths = demand_paths(lock_paths)?;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            if cancelled() {
                return Err(cancelled_error());
            }
            let file = open(&path)?;
            loop {
                if cancelled() {
                    return Err(cancelled_error());
                }
                match fs4::FileExt::try_lock_shared(&file) {
                    Ok(()) => {
                        validate(&file, &path)?;
                        if cancelled() {
                            return Err(cancelled_error());
                        }
                        files.push(file);
                        break;
                    }
                    Err(fs4::TryLockError::Error(error)) => return Err(error),
                    Err(fs4::TryLockError::WouldBlock) => {}
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "writer demand admission timed out",
                    ));
                }
                thread::sleep(remaining.min(POLL));
            }
        }
        Ok(Self { files })
    }
}
impl ReadAdmission {
    /// Try every gate once, releasing partial gates if any writer has demand.
    ///
    /// Lock attempts never wait; ordinary filesystem calls retain their OS
    /// latency. None conservatively reports compatible writer demand. Another
    /// transient EX reader gate returns `WouldBlock`, permitting bounded retry
    /// after every partial gate has closed.
    /// While returned Some is held, acquire artifact SH only with nonblocking
    /// operations; drop this value before any busy wait.
    /// # Errors
    /// Returns `WouldBlock` for another transient reader gate, or invalid
    /// path/inode and other lock errors rather than treating them idle.
    pub fn try_acquire(lock_paths: &[PathBuf]) -> io::Result<Option<Self>> {
        Self::try_acquire_with(lock_paths, open)
    }

    pub(crate) fn try_acquire_existing(lock_paths: &[PathBuf]) -> io::Result<Option<Self>> {
        Self::try_acquire_with(lock_paths, open_existing)
    }

    fn try_acquire_with(
        lock_paths: &[PathBuf],
        open_gate: impl Fn(&Path) -> io::Result<File>,
    ) -> io::Result<Option<Self>> {
        let paths = demand_paths(lock_paths)?;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let file = open_gate(&path)?;
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => {
                    validate(&file, &path)?;
                    files.push(file);
                }
                Err(fs4::TryLockError::WouldBlock) => {
                    // SH writer demand is compatible with this probe; another
                    // EX reader gate is not. Close this probe and all prior
                    // gates on either return; never explicitly unlock aliases.
                    return match fs4::FileExt::try_lock_shared(&file) {
                        Ok(()) => {
                            validate(&file, &path)?;
                            Ok(None)
                        }
                        Err(fs4::TryLockError::WouldBlock) => Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "another transient reader admission gate is active",
                        )),
                        Err(fs4::TryLockError::Error(error)) => Err(error),
                    };
                }
                Err(fs4::TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(Some(Self { files }))
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "writer demand admission cancelled")
}
fn demand_paths(lock_paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    if lock_paths.is_empty() || lock_paths.len() > crate::write_domains::MAX_ROOTS {
        return Err(invalid("writer demand requires one to thirty-two artifact lock paths"));
    }
    lock_paths
        .iter()
        .map(|path| demand_path(path))
        .collect::<io::Result<BTreeSet<_>>>()
        .map(|paths| paths.into_iter().collect())
}
fn demand_path(lock: &Path) -> io::Result<PathBuf> {
    let parent = lock
        .parent()
        .ok_or_else(|| invalid("artifact lock has no stable parent"))?;
    let filename = lock
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("artifact lock name invalid"))?;
    let digest = filename
        .strip_prefix(".lean-rs-writer-")
        .and_then(|name| name.strip_suffix(".lock"))
        .ok_or_else(|| invalid("writer demand requires a project artifact lock path"))?;
    if !lock.is_absolute()
        || parent.canonicalize()? != parent
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("writer demand requires a canonical stable project lock path"));
    }
    match std::fs::symlink_metadata(lock) {
        Ok(metadata) if !metadata.is_file() || metadata.nlink() != 1 => {
            return Err(invalid("artifact lock is not a unique regular file"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(parent.join(format!(".lean-rs-writer-{digest}-demand.lock")))
}
fn open(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    // NOFOLLOW/NONBLOCK avoid symlink redirection or a malformed FIFO blocking
    // before descriptor validation. Only this literal stable sibling is created.
    let file = File::from(rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::from_raw_mode(0o600),
    )?);
    validate(&file, path)?;
    Ok(file)
}
fn open_existing(path: &Path) -> io::Result<File> {
    let file = open_read_only_lock(path)?;
    validate(&file, path)?;
    Ok(file)
}

pub(crate) fn open_read_only_lock(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    Ok(File::from(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?))
}
fn validate(file: &File, path: &Path) -> io::Result<()> {
    let opened = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    if !opened.is_file()
        || opened.nlink() != 1
        || !named.is_file()
        || named.nlink() != 1
        || opened.uid() != rustix::process::geteuid().as_raw()
        || opened.mode() & 0o077 != 0
        || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
    {
        return Err(invalid("writer demand inode identity or private file mode changed"));
    }
    Ok(())
}
#[cfg(test)]
mod tests;
