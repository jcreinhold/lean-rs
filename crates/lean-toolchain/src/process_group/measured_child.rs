//! Sole wait ownership and exact waited-payload resource facts.
use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::time::Instant;

/// Kernel accounting returned by the wait that reaped this actual payload.
///
/// Accounting can include descendants the payload itself waited for. It does
/// not describe the complete native process group or live resident memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitedChildUsage {
    /// Time from the spawn attempt until the successful wait observation.
    pub wall_millis: u64,
    /// Waited-payload user CPU time, in microseconds.
    pub user_cpu_micros: u64,
    /// Waited-payload system CPU time, in microseconds.
    pub system_cpu_micros: u64,
    /// Kernel peak RSS normalized to bytes on macOS and Linux.
    pub max_rss_bytes: Option<u64>,
}

/// One actual payload child, with one wait owner and cached exit accounting.
///
/// No other waiter or SIGCHLD handler may reap this child. Retain this value
/// through errors and use the existing caller's reaper; dropping it does not
/// terminate or reap the payload. No mutable raw Child is exposed.
#[derive(Debug)]
#[must_use = "retain the actual payload until its exit is observed"]
pub struct MeasuredChild {
    child: Child,
    started: Instant,
    status: Option<ExitStatus>,
    usage: Option<WaitedChildUsage>,
    wait_revoked: bool,
}
impl MeasuredChild {
    /// Spawn the payload without changing its inherited process group.
    /// # Errors
    /// Returns command setup or spawn failure.
    pub fn spawn(mut command: Command) -> io::Result<Self> {
        let started = Instant::now();
        let child = command.spawn()?;
        Ok(Self {
            child,
            started,
            status: None,
            usage: None,
            wait_revoked: false,
        })
    }
    /// The actual child PID; this number alone grants no signal authority.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    /// Take the payload's piped stdout.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }
    /// Take the payload's piped stderr.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }
    /// Resource facts are available only after an actual successful wait.
    #[must_use]
    pub fn usage(&self) -> Option<&WaitedChildUsage> {
        self.usage.as_ref()
    }
    /// Poll once without blocking. Subsequent successful polls use cached facts.
    /// # Errors
    /// Returns the kernel wait error. ECHILD permanently revokes signal authority.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        if self.wait_revoked {
            return Err(io::Error::from_raw_os_error(libc::ECHILD));
        }
        match raw::poll(&self.child) {
            Ok(Some((status, usage))) => {
                self.status = Some(status);
                self.usage = usage_from_wait(usage, self.started);
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.wait_revoked = true;
                }
                Err(error)
            }
        }
    }
    /// Request termination only while this actual child remains wait-owned.
    /// # Errors
    /// Returns wait or signal failure. A successful signal is not exit evidence.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        self.child.kill()
    }
}
fn micros(value: libc::timeval) -> Option<u64> {
    let seconds = u64::try_from(value.tv_sec).ok()?;
    let fraction = u64::try_from(value.tv_usec).ok()?;
    if fraction >= 1_000_000 {
        return None;
    }
    seconds.checked_mul(1_000_000)?.checked_add(fraction)
}
fn usage_from_wait(usage: libc::rusage, started: Instant) -> Option<WaitedChildUsage> {
    let raw_rss = u64::try_from(usage.ru_maxrss).ok();
    #[cfg(target_os = "macos")]
    let max_rss_bytes = raw_rss;
    #[cfg(target_os = "linux")]
    let max_rss_bytes = raw_rss.and_then(|value| value.checked_mul(1024));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let max_rss_bytes = {
        let _ = raw_rss;
        None
    };
    Some(WaitedChildUsage {
        wall_millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        user_cpu_micros: micros(usage.ru_utime)?,
        system_cpu_micros: micros(usage.ru_stime)?,
        max_rss_bytes,
    })
}
mod raw {
    #![allow(
        unsafe_code,
        reason = "one wait4 owner reaps its private actual Child and receives initialized kernel accounting"
    )]
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Child, ExitStatus};
    pub(super) fn poll(child: &Child) -> io::Result<Option<(ExitStatus, libc::rusage)>> {
        let pid = libc::pid_t::try_from(child.id())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "payload PID out of range"))?;
        let mut status = MaybeUninit::<libc::c_int>::uninit();
        let mut usage = MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: MeasuredChild exclusively owns this spawned, unreaped Child;
        // neither its API nor this helper exposes another wait handle. Both
        // output pointers are writable and only read after wait4 returns pid.
        let result = unsafe { libc::wait4(pid, status.as_mut_ptr(), libc::WNOHANG, usage.as_mut_ptr()) };
        if result == 0 {
            return Ok(None);
        }
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        if result != pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wait4 returned another payload",
            ));
        }
        // SAFETY: successful wait4 for this exact pid initialized status.
        let status = unsafe { status.assume_init() };
        // A traced payload may report a stop even without WUNTRACED. That is
        // not a reap and cannot publish terminal status or resource facts.
        if !libc::WIFEXITED(status) && !libc::WIFSIGNALED(status) {
            return Ok(None);
        }
        // SAFETY: successful terminal wait4 initialized the resource output.
        let usage = unsafe { usage.assume_init() };
        Ok(Some((ExitStatus::from_raw(status), usage)))
    }
}

#[cfg(test)]
mod tests;
