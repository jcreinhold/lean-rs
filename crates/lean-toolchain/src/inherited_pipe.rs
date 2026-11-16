//! Validated inherited control pipes, without fabricating borrowed FD ownership.
use rustix::fs::{FileType, OFlags, fcntl_getfl, fstat};
use std::io;
use std::os::fd::{OwnedFd, RawFd};

/// Duplicate an explicitly supplied private cancellation read end.
///
/// The caller transfers exclusive use of a pipe read end already marked
/// nonblocking. Both the inherited descriptor and the fresh duplicate become
/// CLOEXEC. The original is never closed and shared open-description flags are
/// never changed. Exclusivity is the caller's transfer contract, not a kernel
/// reference-count claim.
/// # Errors
/// Refuses standard/invalid descriptors, non-pipes, write ends or blocking pipes.
pub fn duplicate_inherited_read_pipe(raw: RawFd) -> io::Result<OwnedFd> {
    if raw <= 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private cancellation FD must exceed 2",
        ));
    }
    let fd = raw::duplicate_cloexec(raw)?;
    let metadata = fstat(&fd)?;
    let flags = fcntl_getfl(&fd)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Fifo
        || flags & OFlags::ACCMODE != OFlags::RDONLY
        || !flags.contains(OFlags::NONBLOCK)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cancellation FD requires a private nonblocking pipe read end",
        ));
    }
    Ok(fd)
}
mod raw {
    #![allow(
        unsafe_code,
        reason = "numeric fcntl validates supplied FD and creates one fresh owned CLOEXEC duplicate"
    )]
    use std::io;
    use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
    pub(super) fn duplicate_cloexec(raw: RawFd) -> io::Result<OwnedFd> {
        // SAFETY: numeric fcntl safely rejects invalid integers. Flags affect
        // only this supplied descriptor, preserving every preexisting flag.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: no descriptor ownership is fabricated or released; this closes
        // the inheritance path before any later helper or payload exec.
        if unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful numeric duplication creates a distinct owned FD.
        let duplicate = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful fcntl returned this unique new open descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
    }
}
