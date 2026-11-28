//! Samples of the child's RSS and CPU time. On Linux both come from `/proc`;
//! on macOS CPU time comes from `proc_pidinfo` and RSS from `ps`; elsewhere
//! both come from `ps`.

#[cfg(not(target_os = "linux"))]
use std::process::Command;

use super::LeanWorker;

impl LeanWorker {
    /// Measure the current child RSS in KiB when supported by the platform.
    ///
    /// This is an observability hook for restart policy and memory-cycling
    /// workloads. A `None` result means the platform did not provide a usable
    /// sample; it is not a worker failure.
    pub fn rss_kib(&mut self) -> Option<u64> {
        match self.child_rss_kib() {
            Some(value) => {
                self.stats.last_rss_kib = Some(value);
                Some(value)
            }
            None => {
                self.stats.rss_samples_unavailable = self.stats.rss_samples_unavailable.saturating_add(1);
                None
            }
        }
    }

    /// Measure the current child CPU time (user + system) in milliseconds.
    ///
    /// Cumulative since the child started; take a delta around a unit of work
    /// for a contention-immune cost measure. A `None` result means the platform
    /// did not provide a usable sample; it is not a worker failure.
    pub fn cumulative_cpu_millis(&mut self) -> Option<u64> {
        self.child_cpu_millis()
    }

    pub(super) fn hard_rss_limit_exceeded(&mut self) -> Option<(u64, u64)> {
        let limit_kib = self.config.rss_hard_limit_kib?;
        match self.child_rss_kib() {
            Some(current_kib) if current_kib >= limit_kib => {
                self.stats.last_rss_kib = Some(current_kib);
                Some((current_kib, limit_kib))
            }
            Some(current_kib) => {
                self.stats.last_rss_kib = Some(current_kib);
                None
            }
            None => {
                self.stats.rss_samples_unavailable = self.stats.rss_samples_unavailable.saturating_add(1);
                None
            }
        }
    }

    pub(super) fn child_rss_kib(&mut self) -> Option<u64> {
        let child = self.child.as_mut()?;
        child_rss_kib(child.id())
    }

    fn child_cpu_millis(&mut self) -> Option<u64> {
        let child = self.child.as_mut()?;
        child_cpu_millis(child.id())
    }
}

#[cfg(target_os = "linux")]
fn child_rss_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

/// Cumulative child CPU time (user + system) in milliseconds.
///
/// Sampling this before and after a unit of work gives a contention-immune
/// cost measure: unlike wallclock it does not inflate when the child waits for
/// a busy CPU, and unlike `getrusage(RUSAGE_CHILDREN)` it works on a live,
/// long-lived child. A `None` result means the platform did not provide a
/// usable sample; it is not a worker failure.
#[cfg(target_os = "linux")]
pub(super) fn child_cpu_millis(pid: u32) -> Option<u64> {
    // /proc/<pid>/stat: fields 14 (utime) and 15 (stime), in clock ticks. The
    // comm field (2) may contain spaces and parentheses, so split relative to
    // the final ')' — the remaining tokens start at field 3. The kernel reports
    // these fields in USER_HZ units, a stable ABI constant of 100 on every
    // supported architecture, independent of CONFIG_HZ.
    const USER_HZ: u64 = 100;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let stime_ticks = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime_ticks.saturating_add(stime_ticks).saturating_mul(1_000) / USER_HZ)
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "proc_pidinfo is the only live-process CPU-time API on macOS that populates user/system time \
              (proc_pid_rusage returns zeros); the buffer is a POD proc_taskinfo the kernel interface writes once"
)]
pub(super) fn child_cpu_millis(pid: u32) -> Option<u64> {
    // SAFETY: `proc_taskinfo` is plain-old-data; `proc_pidinfo` with
    // PROC_PIDTASKINFO writes exactly one such struct into the buffer and
    // returns its size on success.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let expected = std::mem::size_of::<libc::proc_taskinfo>();
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            (&raw mut info).cast::<libc::c_void>(),
            expected as libc::c_int,
        )
    };
    if usize::try_from(written).ok()? != expected {
        return None;
    }
    // pti_total_user/pti_total_system are mach absolute-time ticks, NOT
    // nanoseconds despite the header's wording; scale by the timebase
    // (125/3 on Apple Silicon) to get nanoseconds.
    let mut timebase = mach2::mach_time::mach_timebase_info_data_t::default();
    // SAFETY: writes one POD timebase struct; kern_return_t 0 is success.
    let rc = unsafe { mach2::mach_time::mach_timebase_info(&raw mut timebase) };
    if rc != 0 || timebase.denom == 0 {
        return None;
    }
    let ticks = info.pti_total_user.saturating_add(info.pti_total_system);
    let nanos = ticks
        .saturating_mul(u64::from(timebase.numer))
        .checked_div(u64::from(timebase.denom))?;
    Some(nanos / 1_000_000)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn child_cpu_millis(pid: u32) -> Option<u64> {
    // Same `ps` fallback shape as `child_rss_kib`. `cputime` prints cumulative
    // CPU as [[DD-]HH:]MM:SS — one-second granularity, which suffices for the
    // sampling deltas callers take (their thresholds are seconds-scale).
    let output = Command::new("ps")
        .args(["-o", "cputime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ps_cputime(output.stdout.as_slice())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn parse_ps_cputime(text: impl AsRef<[u8]>) -> Option<u64> {
    let text = String::from_utf8_lossy(text.as_ref());
    // macOS prints [[DD-]HH:]MM:SS with a ".hh" hundredths fraction on the
    // seconds; Linux prints the same without the fraction. Truncate it.
    let text = text.trim().split('.').next()?.trim();
    let (days, text) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let mut parts = text.splitn(3, ':');
    let first = parts.next()?.parse::<u64>().ok()?;
    let second = parts.next()?.parse::<u64>().ok()?;
    let (hours, minutes, seconds) = match parts.next() {
        Some(third) => (first, second, third.parse::<u64>().ok()?),
        None => (0, first, second),
    };
    let total_seconds = days
        .saturating_mul(86_400)
        .saturating_add(hours.saturating_mul(3_600))
        .saturating_add(minutes.saturating_mul(60))
        .saturating_add(seconds);
    Some(total_seconds.saturating_mul(1_000))
}

#[cfg(not(target_os = "linux"))]
fn child_rss_kib(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<u64>().ok().filter(|value| *value > 0)
}
