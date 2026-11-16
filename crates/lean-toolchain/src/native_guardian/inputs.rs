//! Literal generation-owned input assets; no second ownership namespace.
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, mkdirat, open, openat, statat, unlinkat};
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 32;
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn generation(directory: &Path) -> io::Result<OwnedFd> {
    if !directory.is_absolute() || directory.canonicalize()? != directory {
        return Err(super::invalid("generation inputs require a canonical owner directory"));
    }
    let fd = open(directory, DIRECTORY, Mode::empty())?;
    let metadata = fstat(&fd)?;
    if metadata.st_uid != rustix::process::getuid().as_raw() || metadata.st_mode & 0o077 != 0 {
        return Err(super::invalid("generation input owner directory must be private"));
    }
    Ok(fd)
}
fn checked_inputs(directory: &Path, create: bool) -> io::Result<PathBuf> {
    let parent = generation(directory)?;
    if create {
        match mkdirat(&parent, "inputs", Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let inputs = openat(&parent, "inputs", DIRECTORY, Mode::empty())?;
    let metadata = fstat(&inputs)?;
    if metadata.st_uid != rustix::process::getuid().as_raw() || metadata.st_mode & 0o077 != 0 {
        return Err(super::invalid("generation inputs must be a private directory"));
    }
    Ok(directory.join("inputs"))
}
/// Create the literal private input directory before native body admission.
/// # Errors
/// Refuses aliases, non-directory or non-private input locations.
#[doc(hidden)]
pub fn create_generation_inputs(directory: &Path) -> io::Result<PathBuf> {
    checked_inputs(directory, true)
}
/// Return the existing literal private input directory without creating it.
/// # Errors
/// Refuses missing, aliased or non-private input locations.
#[doc(hidden)]
pub fn generation_inputs(directory: &Path) -> io::Result<PathBuf> {
    checked_inputs(directory, false)
}

struct Budget {
    entries: usize,
    until: Instant,
}
impl Budget {
    fn entry(&mut self, depth: usize) -> io::Result<()> {
        if depth > MAX_DEPTH || self.entries >= MAX_ENTRIES || Instant::now() >= self.until {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "generation input cleanup budget exceeded",
            ));
        }
        self.entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| super::invalid("input cleanup count overflow"))?;
        Ok(())
    }
}
fn contents(directory: &OwnedFd, depth: usize, budget: &mut Budget) -> io::Result<()> {
    let mut entries = Dir::read_from(directory)?;
    for entry in &mut entries {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        budget.entry(depth)?;
        let metadata = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => {
                let child = openat(directory, name, DIRECTORY, Mode::empty())?;
                contents(
                    &child,
                    depth
                        .checked_add(1)
                        .ok_or_else(|| super::invalid("input depth overflow"))?,
                    budget,
                )?;
                unlinkat(directory, name, AtFlags::REMOVEDIR)?;
            }
            FileType::RegularFile | FileType::Symlink => unlinkat(directory, name, AtFlags::empty())?,
            FileType::Fifo
            | FileType::Socket
            | FileType::CharacterDevice
            | FileType::BlockDevice
            | FileType::Unknown => return Err(super::invalid("unexpected special file in generation inputs")),
        }
    }
    Ok(())
}
/// Remove only the literal `inputs` child after the caller proves native closure.
///
/// Relative descriptor operations never follow symlinks. Work is bounded by
/// 4096 entries, depth 32 and a two-second userspace budget; syscall latency is
/// not preemptible. Errors preserve the owner's record for the next admission.
/// # Errors
/// Returns unavailable ownership, malformed input or bounded-cleanup failure.
#[doc(hidden)]
pub fn cleanup_generation_inputs(directory: &Path) -> io::Result<()> {
    let parent = match generation(directory) {
        Ok(parent) => parent,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    // Unknown generation children are not owned input assets. Refuse before
    // removing anything, so later owner-record collection stays recoverable.
    let mut layout = Dir::read_from(&parent)?;
    let mut count = 0_usize;
    for entry in &mut layout {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| super::invalid("generation child count overflow"))?;
        if count > 4
            || !matches!(
                name,
                b"owner.json" | b"owner.write" | b"owner.guardian-write" | b"inputs"
            )
        {
            return Err(super::invalid("unexpected child beside generation inputs"));
        }
    }
    let metadata = match statat(&parent, "inputs", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    match FileType::from_raw_mode(metadata.st_mode) {
        FileType::Directory => {
            let inputs = openat(&parent, "inputs", DIRECTORY, Mode::empty())?;
            let until = Instant::now()
                .checked_add(CLEANUP_BUDGET)
                .ok_or_else(|| super::invalid("cleanup deadline overflow"))?;
            contents(&inputs, 0, &mut Budget { entries: 0, until })?;
            unlinkat(&parent, "inputs", AtFlags::REMOVEDIR)?;
        }
        FileType::RegularFile | FileType::Symlink => unlinkat(&parent, "inputs", AtFlags::empty())?,
        FileType::Fifo | FileType::Socket | FileType::CharacterDevice | FileType::BlockDevice | FileType::Unknown => {
            return Err(super::invalid("unexpected generation input root type"));
        }
    }
    Ok(())
}
