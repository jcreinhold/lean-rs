//! Full-turn disk admission for cooperating writers. One private subdirectory of
//! the native owner registry stores charges under the existing admission lock.
//! The launcher must pin the same registry and reserve floor for every participant,
//! select every writable root, and keep these directory entries under its custody.
//! A free-space observation is advisory. It cannot constrain unrelated writes.

use super::{MAX_OWNER_BYTES, MAX_OWNER_RECORDS, deadline, expired, invalid, ownership, registry_directory};
use rustix::fs::{AtFlags, Dir, Mode, OFlags, fstatvfs, mkdirat, open, openat, unlinkat};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CHARGES: &str = ".writer-storage-v1";
const MAX_ROOTS: usize = 96;
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Expected canonical directory from the launcher's independent storage policy.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRoot {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}

impl StorageRoot {
    fn open(&self) -> io::Result<File> {
        check_path(&self.path)?;
        let fd = File::from(open(&self.path, DIRECTORY, Mode::empty())?);
        let meta = fd.metadata()?;
        if (meta.dev(), meta.ino()) != (self.device, self.inode) {
            return Err(invalid("writer storage directory identity changed"));
        }
        Ok(fd)
    }
}

fn check_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.as_os_str().len() > 4096 || path.canonicalize()?.as_os_str() != path.as_os_str() {
        Err(invalid("writer storage requires a bounded canonical path"))
    } else {
        Ok(())
    }
}

pub(super) fn is_charge_directory(entry: &fs::DirEntry) -> io::Result<bool> {
    if entry.file_name() != CHARGES {
        return Ok(false);
    }
    if !entry.file_type()?.is_dir() {
        return Err(invalid("invalid writer storage directory"));
    }
    Ok(true)
}

fn charge_directory(registry: &File, create: bool) -> io::Result<Option<File>> {
    if create {
        match mkdirat(registry, CHARGES, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
    }
    match openat(registry, CHARGES, DIRECTORY, Mode::empty()) {
        Err(rustix::io::Errno::NOENT) if !create => Ok(None),
        Err(error) => Err(error.into()),
        Ok(fd) => {
            let fd = File::from(fd);
            let meta = fd.metadata()?;
            if meta.uid() != rustix::process::getuid().as_raw() || meta.mode() & 0o077 != 0 {
                return Err(invalid("writer storage directory must be private"));
            }
            Ok(Some(fd))
        }
    }
}

pub(super) fn charges_remain(registry: &Path) -> io::Result<bool> {
    let Some(dir) = charge_directory(&File::open(registry)?, false)? else {
        return Ok(false);
    };
    for entry in Dir::read_from(dir)? {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Charge {
    primary: PathBuf,
    device: u64,
    inode: u64,
    budget: u64,
    roots: Vec<StorageRoot>,
}

impl Charge {
    fn read(directory: &File, turn: &str) -> io::Result<Self> {
        if turn.len() != 32 || !turn.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("invalid writer storage turn"));
        }
        let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = File::from(openat(directory, turn, flags, Mode::empty())?);
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != rustix::process::getuid().as_raw()
            || meta.mode() & 0o077 != 0
            || meta.len() > MAX_OWNER_BYTES as u64
        {
            return Err(invalid("invalid writer storage charge file"));
        }
        let mut bytes = Vec::new();
        file.take((MAX_OWNER_BYTES as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_OWNER_BYTES {
            return Err(invalid("writer storage charge too large"));
        }
        let entry: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if entry.budget == 0 || entry.roots.is_empty() || entry.roots.len() > MAX_ROOTS || !entry.primary.is_absolute()
        {
            return Err(invalid("invalid writer storage charge"));
        }
        Ok(entry)
    }
}

/// Live writer's only settlement handle. Drop, process death, and compute release
/// leave the entire charge in the registry, including death before the first GO.
#[derive(Debug)]
#[must_use]
pub struct WriterStorageCharge {
    registry: StorageRoot,
    turn: String,
    charge: Charge,
    writer: u32,
}

impl WriterStorageCharge {
    /// Unique component used by the writer for this turn's scratch directory.
    pub fn turn(&self) -> &str {
        &self.turn
    }

    /// Settle while the same writer still holds EX on the primary loan. Supply
    /// its actual combined proof of payload closure, record finalization and final
    /// scratch cleanup, including completion of any caller-owned writes.
    /// # Errors
    /// A failed closure or cleanup, unavailable custody, or changed charge
    /// refuses settlement. An interrupted write or sync requires reconciliation.
    pub fn settle_after_cleanup(self, primary: &File, closure: io::Result<()>, timeout: Duration) -> io::Result<()> {
        closure?;
        if self.writer != std::process::id() {
            return Err(invalid("writer storage settlement belongs to another turn"));
        }
        let root = self.registry.open()?;
        let _lock = ownership::lock_admission_at(&root, deadline(timeout)?, false)?;
        crate::writer_lock::validate_artifact_lock(primary, &self.charge.primary, None)?;
        let meta = primary.metadata()?;
        let dir = charge_directory(&root, false)?.ok_or_else(|| invalid("writer storage directory missing"))?;
        if (meta.dev(), meta.ino()) != (self.charge.device, self.charge.inode)
            || Charge::read(&dir, &self.turn)? != self.charge
        {
            return Err(invalid("writer storage settlement identity changed"));
        }
        unlinkat(&dir, self.turn.as_str(), AtFlags::empty())?;
        dir.sync_all()?;
        match unlinkat(&root, CHARGES, AtFlags::REMOVEDIR) {
            Ok(()) => root.sync_all(),
            Err(rustix::io::Errno::NOTEMPTY | rustix::io::Errno::EXIST) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Reserve one full-turn charge under the native registry admission lock.
///
/// The writer holds its entire EX lock union, primary first, before publishing a
/// new owner or creating scratch. Supply registry and roots from trusted launcher policy independently
/// of any request or registry override. Each root must already exist.
/// # Errors
/// Refuses alternate registries, changed identities, outstanding custody,
/// malformed charges, insufficient measured space, timeout and publication failures.
pub fn reserve_writer_storage(
    registry: &StorageRoot,
    locks: &[(PathBuf, File)],
    roots: &[StorageRoot],
    floor: u64,
    budget: u64,
    timeout: Duration,
) -> io::Result<WriterStorageCharge> {
    reserve(
        &registry_directory()?,
        registry,
        locks,
        roots,
        (floor, budget),
        deadline(timeout)?,
        |file| {
            let stat = fstatvfs(file)?;
            stat.f_bavail
                .checked_mul(stat.f_frsize)
                .ok_or_else(|| invalid("writer free-space overflow"))
        },
    )
}

fn reserve(
    selected: &Path,
    registry: &StorageRoot,
    locks: &[(PathBuf, File)],
    approved: &[StorageRoot],
    (floor, budget): (u64, u64),
    until: Instant,
    available: impl Fn(&File) -> io::Result<u64>,
) -> io::Result<WriterStorageCharge> {
    if selected.as_os_str() != registry.path.as_os_str()
        || budget == 0
        || approved.is_empty()
        || approved.len() > MAX_ROOTS
        || locks.is_empty()
        || locks.len() > crate::shared_writer::MAX_WRITER_LOCK_LOANS
    {
        return Err(invalid("invalid writer registry, budget, lock or root selection"));
    }
    ownership::validate_registry(selected)?;
    let root = registry.open()?;
    let _lock = ownership::lock_admission_at(&root, until, true)
        .map_err(|e| io::Error::new(e.kind(), format!("locking writer storage registry: {e}")))?;
    for approved_root in approved {
        check_path(&approved_root.path)?;
    }
    let mut roots: BTreeSet<_> = approved.iter().cloned().collect();
    roots.insert(registry.clone());
    let mut custody = Vec::new();
    for (path, file) in locks {
        check_path(path)?;
        crate::writer_lock::validate_artifact_lock(file, path, None)?;
        let meta = file.metadata()?;
        custody.push((path, meta.dev(), meta.ino()));
        let parent = path.parent().ok_or_else(|| invalid("writer lock parent missing"))?;
        let dir = fs::metadata(parent)?;
        if dir.dev() != meta.dev() {
            return Err(invalid("writer lock mounted separately from its directory"));
        }
        roots.insert(StorageRoot {
            path: parent.to_owned(),
            device: dir.dev(),
            inode: dir.ino(),
        });
    }
    let &(path, device, inode) = custody
        .first()
        .ok_or_else(|| invalid("primary writer custody missing"))?;
    if roots.len() > MAX_ROOTS {
        return Err(invalid("writer storage root count exceeds limit"));
    }
    let charge = Charge {
        primary: path.clone(),
        device,
        inode,
        budget,
        roots: roots.iter().cloned().collect(),
    };
    let mut required = floor
        .checked_add(budget)
        .ok_or_else(|| invalid("writer storage budget overflow"))?;
    if let Some(dir) = charge_directory(&root, false)? {
        let entries = Dir::read_from(&dir)?;
        let mut seen = 0_usize;
        for item in entries {
            if Instant::now() >= until {
                return Err(expired());
            }
            let entry = item?;
            let name = entry.file_name().to_str().map_err(io::Error::other)?;
            if matches!(name, "." | "..") {
                continue;
            }
            seen = seen.saturating_add(1);
            if seen >= MAX_OWNER_RECORDS {
                return Err(io::Error::other("writer storage turn limit reached"));
            }
            let old = Charge::read(&dir, name)
                .map_err(|e| io::Error::new(e.kind(), format!("reading writer storage charge {name}: {e}")))?;
            if custody
                .iter()
                .any(|&(p, d, i)| *p == old.primary || (d, i) == (old.device, old.inode))
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "prior writer storage custody remains open",
                ));
            }
            required = required
                .checked_add(old.budget)
                .ok_or_else(|| invalid("writer storage budget overflow"))?;
            roots.extend(old.roots);
        }
    }
    for item in roots {
        if Instant::now() >= until {
            return Err(expired());
        }
        let measured = item.open().and_then(|fd| available(&fd)).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("measuring writer storage at {}: {e}", item.path.display()),
            )
        })?;
        if measured < required {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "insufficient space for outstanding full-turn writer budgets",
            ));
        }
    }
    let mut random = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let turn = format!("{:032x}", u128::from_ne_bytes(random));
    let bytes = serde_json::to_vec(&charge).map_err(io::Error::other)?;
    if bytes.len() > MAX_OWNER_BYTES {
        return Err(invalid("writer storage charge too large"));
    }
    if Instant::now() >= until {
        return Err(expired());
    }
    let dir = charge_directory(&root, true)?.ok_or_else(|| invalid("writer storage directory missing"))?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut record = File::from(openat(&dir, turn.as_str(), flags, Mode::RUSR | Mode::WUSR)?);
    // A torn file blocks subsequent admissions. No command may start until
    // the record, its directory and the registry entry are all durable.
    record.write_all(&bytes)?;
    record.sync_all()?;
    dir.sync_all()?;
    root.sync_all()?;
    Ok(WriterStorageCharge {
        registry: registry.clone(),
        turn,
        charge,
        writer: std::process::id(),
    })
}

#[cfg(test)]
mod tests;
