//! One owner for a command and its inherited process group.
//!
//! On macOS the owner keeps the actual child unreaped until a complete kernel
//! snapshot contains only that exited leader. Descendants must retain its PGID,
//! and no other thread or SIGCHLD handler may reap managed children. Keep this
//! value and poll it after errors; neither a sent signal nor Drop proves exit.
//!
//! Linux and other Unix systems currently use a weaker backend: after reaping
//! the leader they only observe group absence, never signal the stored PGID.
//! A surviving or late-forked descendant may therefore retain custody forever.

use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, ChildStdin, Command, ExitStatus};

/// An actual command child and its private inherited process group.
///
/// This value is not cloneable and exposes no raw child or mutable wait handle.
/// Its owner must retain associated resources and keep polling until `try_wait`
/// confirms closure. Descendants changing groups or sessions are unsupported.
#[derive(Debug)]
#[must_use = "retain the command group and associated resources until closure is confirmed"]
pub struct OwnedCommandGroup {
    child: Child,
    pid: u32,
    status: Option<ExitStatus>,
    custody: Custody,
    stop_requested: bool,
    cleanup_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Custody {
    Owned,
    Lost,
    Closed,
}

impl OwnedCommandGroup {
    /// Spawn a real child in a new process group, preserving its PID and stdio.
    ///
    /// # Errors
    /// Returns the command's spawn error. No command is retained on failure.
    pub fn spawn(command: Command) -> io::Result<Self> {
        Self::spawn_inheriting(command, &[])
    }

    /// Spawn with child-only inheritance of close-only ownership descriptors.
    ///
    /// The caller retains every borrowed descriptor. The consumed command
    /// cannot later reuse a stored callback after those borrows expire.
    ///
    /// # Errors
    /// Returns invalid descriptor, child setup, or command spawn errors.
    pub fn spawn_inheriting(mut command: Command, descriptors: &[BorrowedFd<'_>]) -> io::Result<Self> {
        command.process_group(0);
        let child = spawn_inheriting(command, descriptors)?;
        Ok(Self {
            pid: child.id(),
            child,
            status: None,
            custody: Custody::Owned,
            stop_requested: false,
            cleanup_pending: false,
        })
    }

    /// Adopt this actual, solely owned and still-unreaped child after its
    /// controlled bootstrap established the authenticated private session.
    ///
    /// On macOS the wait-authority probe uses WNOWAIT, preserving the root PID
    /// pin before any group signal. An error returns the actual Child unchanged.
    /// Other Unix backends retain their documented weaker post-reap behavior.
    /// # Errors
    /// Returns changed birth/scope or revoked wait authority with the Child.
    pub fn adopt_private_session(
        child: Child,
        expected: &crate::native_guardian::NativeProcessIdentity,
    ) -> Result<Self, (io::Error, Child)> {
        let checked = (|| {
            if child.id() != expected.pid
                || expected.pid != expected.session
                || expected.pid != expected.process_group
                || expected.pid <= 1
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "adopted child private scope differs",
                ));
            }
            #[cfg(target_os = "macos")]
            let _exited = macos::peek_exit(child.id())?;
            if crate::native_guardian::NativeProcessIdentity::capture(child.id())?.as_ref() != Some(expected) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "adopted child birth differs",
                ));
            }
            Ok(())
        })();
        if let Err(error) = checked {
            return Err((error, child));
        }
        Ok(Self {
            pid: child.id(),
            child,
            status: None,
            custody: Custody::Owned,
            stop_requested: false,
            cleanup_pending: false,
        })
    }

    /// The actual command PID, also its private process-group ID.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Take the command's stdin, for example to release a blocked exec gate.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Whether retirement or an observed leader exit still needs closure.
    ///
    /// This is readiness information, not proof that execution has stopped.
    #[must_use]
    pub fn cleanup_pending(&self) -> bool {
        self.cleanup_pending && self.custody != Custody::Closed
    }

    /// Whether the owner requested intervention rather than natural completion.
    #[must_use]
    pub fn intervened(&self) -> bool {
        self.stop_requested
    }

    /// Request cleanup and perform one nonblocking custody step.
    ///
    /// A successful return does not prove that a signal was delivered or that
    /// execution stopped. Continue calling `try_wait`, retaining resources.
    ///
    /// # Errors
    /// Returns observation or signaling failure without relinquishing custody.
    pub fn request_stop(&mut self) -> io::Result<()> {
        if self.custody == Custody::Closed {
            return Ok(());
        }
        self.stop_requested = true;
        self.cleanup_pending = true;
        self.try_wait().map(|_status| ())
    }

    /// Poll complete owned-group closure and return the actual leader status.
    ///
    /// Natural leader exit does not kill descendants. It returns `None` with
    /// `cleanup_pending()` set, allowing the owner to impose its own grace budget.
    ///
    /// # Errors
    /// Returns an OS observation or kill error. `ECHILD` permanently revokes
    /// signaling authority; later numeric PID reuse cannot restore it.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.custody == Custody::Closed {
            return Ok(self.status);
        }
        if self.custody == Custody::Lost {
            return Err(io::Error::from_raw_os_error(rustix::io::Errno::CHILD.raw_os_error()));
        }
        #[cfg(target_os = "macos")]
        {
            self.poll_macos_with(macos::peek_exit, macos::members, macos::kill_group)
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.poll_passive()
        }
    }

    #[cfg(target_os = "macos")]
    fn reap(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.status = Some(status);
                self.custody = Custody::Closed;
                self.cleanup_pending = false;
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.lose_custody_if_needed(&error);
                Err(error)
            }
        }
    }

    fn lose_custody_if_needed(&mut self, error: &io::Error) {
        if error.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error()) {
            self.custody = Custody::Lost;
            self.cleanup_pending = true;
        }
    }

    #[cfg(target_os = "macos")]
    fn poll_macos_with(
        &mut self,
        mut peek: impl FnMut(u32) -> io::Result<bool>,
        mut observe: impl FnMut(u32) -> io::Result<macos::Members>,
        kill: impl FnOnce(u32) -> io::Result<()>,
    ) -> io::Result<Option<ExitStatus>> {
        if self.custody == Custody::Closed {
            return Ok(self.status);
        }
        if self.custody == Custody::Lost {
            return Err(io::Error::from_raw_os_error(rustix::io::Errno::CHILD.raw_os_error()));
        }
        let exited = peek(self.pid).inspect_err(|error| self.lose_custody_if_needed(error))?;
        if !exited && !self.stop_requested {
            return Ok(None);
        }
        self.cleanup_pending |= exited;
        let members = observe(self.pid)?;
        if exited && members == macos::Members::OnlyLeader {
            return self.reap();
        }
        self.cleanup_pending = true;
        if !self.stop_requested {
            return Ok(None);
        }
        match kill(self.pid) {
            Ok(()) => Ok(None),
            Err(error) => {
                let exited = peek(self.pid).inspect_err(|error| self.lose_custody_if_needed(error))?;
                let members = observe(self.pid)?;
                if exited && members == macos::Members::OnlyLeader {
                    self.reap()?.map_or(Err(error), |status| Ok(Some(status)))
                } else if error.raw_os_error() == Some(rustix::io::Errno::PERM.raw_os_error()) {
                    // A last process can reject signals before becoming waitable.
                    // Permission failure remains pending, never a closure witness.
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn poll_passive(&mut self) -> io::Result<Option<ExitStatus>> {
        use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
        let group = Pid::from_raw(i32::try_from(self.pid).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("invalid owned process group"))?;
        if self.status.is_none() {
            self.status = self
                .child
                .try_wait()
                .inspect_err(|error| self.lose_custody_if_needed(error))?;
            if self.status.is_none() {
                if self.stop_requested {
                    // A successful None observation leaves the sole owner's
                    // child unreaped, pinning this group for this signal only.
                    kill_process_group(group, Signal::KILL)?;
                }
                return Ok(None);
            }
            self.cleanup_pending = true;
        }
        match test_kill_process_group(group) {
            Err(error) if error == rustix::io::Errno::SRCH => {
                self.custody = Custody::Closed;
                self.cleanup_pending = false;
                Ok(self.status)
            }
            Ok(()) | Err(rustix::io::Errno::PERM) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// Consume the command so its pre-exec callback cannot outlive the descriptor
/// borrows. Shared-writer helpers use this without becoming group leaders.
#[allow(
    unsafe_code,
    reason = "child-only inherited descriptor flags require pre_exec and bounded raw-FD borrows"
)]
pub(crate) fn spawn_inheriting(mut command: Command, descriptors: &[BorrowedFd<'_>]) -> io::Result<Child> {
    let inherited = descriptors
        .iter()
        .map(std::os::fd::AsRawFd::as_raw_fd)
        .collect::<Vec<_>>();
    if inherited.iter().any(|descriptor| *descriptor <= 2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ownership descriptors must not replace stdio",
        ));
    }
    if !inherited.is_empty() {
        let inherit = move || {
            for descriptor in &inherited {
                // SAFETY: numeric fcntl accepts any descriptor and safely fails
                // with EBADF if an earlier pre-exec callback closed it. No Rust
                // borrowed handle is fabricated from the child's mutable table.
                if unsafe { libc::fcntl(*descriptor, libc::F_SETFD, 0) } < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        };
        // SAFETY: after fork this callback only iterates existing storage and
        // performs async-signal-safe fcntl calls. The consumed Command is dropped
        // before the descriptor borrows expire, including when spawn fails.
        unsafe { command.pre_exec(inherit) };
    }
    command.spawn()
}

/// Passively observe whether a recorded process group currently exists.
///
/// This never signals a process and grants no signaling authority for stored
/// IDs. macOS uses a complete kernel group census; other Unix systems use the
/// weaker signal-zero existence query. Errors never establish absence.
///
/// # Errors
/// Returns invalid-group, denied, malformed, or unavailable observations.
pub fn group_present(group: u32) -> io::Result<bool> {
    if group <= 1 || i32::try_from(group).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recorded process group",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        macos::group_present(group)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let group = rustix::process::Pid::from_raw(i32::try_from(group).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("invalid process group"))?;
        match rustix::process::test_kill_process_group(group) {
            Ok(()) | Err(rustix::io::Errno::PERM) => Ok(true),
            Err(error) if error == rustix::io::Errno::SRCH => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

/// Observe whether this live private-group leader is its only member.
///
/// The current process pins its own group identity. This read-only observation
/// grants no authority to signal any stored identity. External native bootstraps
/// use it after reaping their direct payload, before requesting guardian cleanup.
///
/// # Errors
/// Refuses nonleaders and platforms without a complete atomic group census.
pub fn current_group_is_singleton() -> io::Result<bool> {
    let current = std::process::id();
    if rustix::process::getpgrp().as_raw_nonzero().get() != i32::try_from(current).map_err(io::Error::other)? {
        return Err(io::Error::other("private current-group leader required"));
    }
    #[cfg(target_os = "macos")]
    {
        Ok(macos::members(current)? == macos::Members::OnlyLeader)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "complete native-reader group census unavailable",
        ))
    }
}

impl Drop for OwnedCommandGroup {
    fn drop(&mut self) {
        // Best effort only. The caller's explicit drain must keep both this
        // owner and its resource guards alive until confirmed closure.
        let _stop = self.request_stop();
    }
}

#[cfg(target_os = "macos")]
mod macos {
    #![allow(
        unsafe_code,
        reason = "owned-child waitid, atomic libproc group census and group signaling"
    )]
    use std::io;

    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_listpids(kind: u32, info: u32, buffer: *mut libc::c_void, size: libc::c_int) -> libc::c_int;
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum Members {
        OnlyLeader,
        Others,
    }

    pub(super) fn peek_exit(pid: u32) -> io::Result<bool> {
        // SAFETY: zeroed siginfo_t is valid writable output storage; WNOHANG
        // leaves si_pid zero when no terminal event is available.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: PID is the sole owner's still-unreaped Child. WNOWAIT keeps
        // that identity pinned, and WNOHANG never waits for command execution.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if info.si_pid == 0 {
            return Ok(false);
        }
        if u32::try_from(info.si_pid).ok() != Some(pid) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "waitid returned another child",
            ));
        }
        Ok(true)
    }

    fn snapshot(pid: u32) -> io::Result<([libc::pid_t; 2], i32)> {
        let mut pids: [libc::pid_t; 2] = [0; 2];
        // SAFETY: this thread's errno storage remains valid; libproc may turn
        // syscall failure into zero bytes, so stale errno cannot be retained.
        let errno = unsafe { libc::__error() };
        // SAFETY: errno belongs to this calling thread.
        unsafe { *errno = 0 };
        // SAFETY: the fixed array is writable for exactly the supplied byte
        // size. This kernel operation only observes, never signals or reaps.
        let count = unsafe { proc_listpids(2, pid, pids.as_mut_ptr().cast(), std::mem::size_of_val(&pids) as i32) };
        // SAFETY: read the same thread-local errno immediately after libproc.
        let error = unsafe { *errno };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        Ok((pids, count))
    }

    pub(super) fn members(pid: u32) -> io::Result<Members> {
        let (pids, count) = snapshot(pid)?;
        decode_members(pid, pids, count, 0)
    }

    pub(super) fn group_present(group: u32) -> io::Result<bool> {
        let (pids, count) = snapshot(group)?;
        decode_presence(pids, count, 0)
    }

    pub(super) fn decode_presence(pids: [libc::pid_t; 2], count: i32, error: i32) -> io::Result<bool> {
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        if count == 0 {
            return Ok(false);
        }
        let width = std::mem::size_of::<libc::pid_t>() as i32;
        if count == width && pids.first().is_some_and(|pid| *pid > 0)
            || count == std::mem::size_of_val(&pids) as i32 && pids.iter().all(|pid| *pid > 0)
        {
            return Ok(true);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed process-group observation",
        ))
    }

    pub(super) fn decode_members(pid: u32, pids: [libc::pid_t; 2], count: i32, error: i32) -> io::Result<Members> {
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        if count == std::mem::size_of_val(&pids) as i32 {
            return Ok(Members::Others);
        }
        if count == std::mem::size_of::<libc::pid_t>() as i32
            && pids.first().and_then(|found| u32::try_from(*found).ok()) == Some(pid)
        {
            return Ok(Members::OnlyLeader);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owned group leader missing from complete snapshot",
        ))
    }

    pub(super) fn kill_group(pid: u32) -> io::Result<()> {
        let group = i32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 1)
            .and_then(i32::checked_neg)
            .ok_or_else(|| io::Error::other("invalid owned process group"))?;
        // SAFETY: the sole owner just verified its unreaped direct child and
        // created this private group. No foreign waiter may release that pin.
        if unsafe { libc::kill(group, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "process_group/tests.rs"]
mod tests;
