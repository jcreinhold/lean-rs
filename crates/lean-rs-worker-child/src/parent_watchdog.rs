//! The parent lifeline: the read end of a pipe whose write end only the parent holds.
//!
//! The parent passes the descriptor number in `LEAN_RS_WORKER_PARENT_LIFELINE`.
//! When the parent exits, the kernel closes the write end, the read returns end
//! of file, and this process exits at once, even while another thread is inside
//! a Lean call. Without the variable, the child relies on stdin end of file.
#![allow(
    unsafe_code,
    reason = "fcntl checks and flags the inherited descriptor; _exit ends only this process"
)]

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{FromRawFd as _, RawFd};
use std::os::unix::fs::FileTypeExt as _;

use lean_rs_worker_protocol::WORKER_PARENT_LIFELINE_ENV as LIFELINE_ENV;

/// Exit status of a child whose parent lifeline closed.
const PARENT_LOST_EXIT: i32 = 125;

/// Start the thread that exits this process when the parent lifeline closes.
///
/// # Errors
/// Returns an error if the variable does not name an inherited read end of a
/// pipe, or if the thread cannot start.
pub(crate) fn install_parent_watchdog() -> io::Result<()> {
    let Some(value) = std::env::var_os(LIFELINE_ENV) else {
        return Ok(());
    };
    let descriptor = value
        .to_str()
        .and_then(|text| text.parse::<RawFd>().ok())
        .filter(|descriptor| *descriptor > 2)
        .ok_or_else(|| io::Error::other("invalid parent lifeline descriptor"))?;
    let lifeline = adopt(descriptor)?;
    std::thread::Builder::new()
        .name("lean-parent-lifeline".to_owned())
        .spawn(move || {
            let _observed = std::panic::catch_unwind(move || wait_for_close(lifeline));
            // SAFETY: `_exit` ends only this process and runs no destructors or
            // atexit handlers, so it cannot wait on a thread stuck in Lean.
            unsafe { libc::_exit(PARENT_LOST_EXIT) }
        })?;
    Ok(())
}

/// Take ownership of the inherited read end after checking what it is.
fn adopt(descriptor: RawFd) -> io::Result<File> {
    // SAFETY: F_GETFL only reads the status flags of a descriptor number and
    // fails with EBADF if none is open there.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_GETFL succeeded, so the descriptor is open. It arrived with
    // exec, so no other handle in this process owns it. ManuallyDrop leaves it
    // open if the checks below refuse it.
    let file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(descriptor) });
    if flags & libc::O_ACCMODE != libc::O_RDONLY || !file.metadata()?.file_type().is_fifo() {
        return Err(io::Error::other("parent lifeline is not the read end of a pipe"));
    }
    // Keep processes this child starts, such as Lake, from holding the lifeline.
    // SAFETY: F_SETFD changes only the close-on-exec flag of this open descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(std::mem::ManuallyDrop::into_inner(file))
}

/// Block until the lifeline reaches end of file, carries data, or fails.
fn wait_for_close(mut lifeline: File) {
    let mut byte = [0_u8; 1];
    loop {
        match lifeline.read(&mut byte) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Ok(_) | Err(_) => return,
        }
    }
}

#[cfg(test)]
#[path = "parent_watchdog/tests.rs"]
mod tests;
