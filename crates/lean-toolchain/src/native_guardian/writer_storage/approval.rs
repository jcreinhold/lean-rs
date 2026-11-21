//! The existing admission mutex retains its inode. Only the operator writes
//! its optional payload: one enable byte followed by a 32-digit generation.
use super::{StorageRoot, deadline, invalid, ownership, registry_directory};
use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::time::{Duration, Instant};

fn checked_size(lock: &File) -> io::Result<u64> {
    let meta = lock.metadata()?;
    if meta.mode() & 0o077 != 0 || meta.len() > 33 {
        return Err(invalid("unrecognized writer approval record on admission mutex"));
    }
    Ok(meta.len())
}

fn active(lock: &File) -> io::Result<bool> {
    let len = checked_size(lock)?;
    if len == 0 {
        return Ok(false);
    }
    let mut flag = [0];
    lock.read_exact_at(&mut flag, 0)?;
    match (flag, len) {
        ([0], _) => Ok(false),
        ([1], 33) => Ok(true),
        _ => Err(invalid("incomplete writer approval record")),
    }
}

pub(super) fn require_generation(lock: &File, requested: &str) -> io::Result<()> {
    if active(lock)?
        && requested.len() == 32
        && requested
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        let mut issued = [0; 32];
        lock.read_exact_at(&mut issued, 1)?;
        if issued == requested.as_bytes() {
            return Ok(());
        }
    }
    Err(invalid("writer handoff approval is absent or stale"))
}

fn operator_lock(registry: &StorageRoot, until: Instant) -> io::Result<(File, File)> {
    if registry_directory()?.as_os_str() != registry.path.as_os_str() {
        return Err(invalid("writer approval registry differs from configured registry"));
    }
    let root = registry.open()?;
    let lock = ownership::lock_admission_at(&root, until, true)?;
    registry.open()?;
    Ok((root, lock))
}

/// Issue a fresh generation for this private registry. Only the operator may
/// distribute it to the approved launcher cohort. Ordinary launches never mint it.
/// # Errors
/// Refuses an active approval, changed registry, contention or unconfirmed publication.
pub fn approve_writer_handoff(registry: &StorageRoot, timeout: Duration) -> io::Result<String> {
    let (root, lock) = operator_lock(registry, deadline(timeout)?)?;
    if active(&lock)? {
        return Err(invalid(
            "revoke the current writer handoff approval before enabling another",
        ));
    }
    let mut nonce = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    let generation = format!("{:032x}", u128::from_be_bytes(nonce));
    // Persist the disabled state and new generation before publishing enable.
    lock.write_all_at(&[0], 0)?;
    lock.write_all_at(generation.as_bytes(), 1)?;
    lock.sync_all()?;
    root.sync_all()?;
    lock.write_all_at(&[1], 0)?;
    lock.sync_all()?;
    Ok(generation)
}

/// Durably disable source-bound admission before checking the existing native
/// owner and storage drain predicates, while retaining the same admission lock.
/// # Errors
/// A failed drain leaves approval revoked. Retry after custody has closed.
pub fn revoke_writer_handoff(registry: &StorageRoot, timeout: Duration) -> io::Result<()> {
    let until = deadline(timeout)?;
    let (root, lock) = operator_lock(registry, until)?;
    checked_size(&lock)?;
    lock.write_all_at(&[0], 0)?;
    lock.sync_all()?;
    root.sync_all()?;
    ownership::check_drained_locked(&registry.path, until)?;
    registry.open()?;
    Ok(())
}
