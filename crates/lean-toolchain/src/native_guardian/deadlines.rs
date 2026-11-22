//! One immutable active-stage deadline on the existing guardian lifeline.

use super::{PROTOCOL, frame, invalid};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

const MAX_ARM_FRAME: usize = 512;
const SOFT_WINDOW: Duration = Duration::from_secs(600);

/// An observation authenticated by an in-process guardian source. The
/// lifeline wire protocol cannot construct one.
pub(super) struct AuthenticatedActivity {
    pub(super) attempt: [u8; 16],
    pub(super) stage: u64,
    pub(super) sequence: u64,
    pub(super) observed_at: Duration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arm {
    op: ArmOp,
    protocol: u32,
    nonce: String,
    attempt: [u8; 16],
    stage: u64,
    expires_ns: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArmOp {
    ArmHardDeadline,
}

#[derive(Default)]
pub(super) struct OwnerDeadline {
    input: Vec<u8>,
    arm: Option<Arm>,
    soft: Option<Duration>,
    last_activity: Option<(u64, Duration)>,
}

impl OwnerDeadline {
    pub(super) fn armed(&self) -> bool {
        self.arm.is_some()
    }

    fn expires_at(&self) -> Option<Duration> {
        self.arm
            .as_ref()
            .map(|arm| self.soft.unwrap_or_else(|| Duration::from_nanos(arm.expires_ns)))
    }

    pub(super) fn expired(&self) -> io::Result<bool> {
        self.expires_at()
            .map_or(Ok(false), |expires| Ok(boot_ticks()? >= expires))
    }

    /// Missing or unattributable observations use the original hard bound.
    /// Keep the last accepted sequence across gaps so replay cannot renew it.
    pub(super) fn observe_activity(&mut self, now: Duration, activity: Option<AuthenticatedActivity>) {
        let Some(arm) = &self.arm else { return };
        let hard = Duration::from_nanos(arm.expires_ns);
        if now >= self.soft.unwrap_or(hard) {
            return;
        }
        let Some(activity) = activity else {
            self.soft = None;
            return;
        };
        if activity.attempt != arm.attempt
            || activity.stage != arm.stage
            || activity.observed_at > now
            || self.last_activity.is_some_and(|(sequence, observed_at)| {
                activity.sequence <= sequence || activity.observed_at <= observed_at
            })
        {
            return;
        }
        let renewed = activity.observed_at.saturating_add(SOFT_WINDOW).min(hard);
        if renewed <= now || self.soft.is_some_and(|soft| renewed <= soft) {
            return;
        }
        self.last_activity = Some((activity.sequence, activity.observed_at));
        self.soft = Some(renewed);
    }

    /// Observe EOF or accept one arm. The authenticated parent socket is
    /// already nonblocking. Each poll does one bounded read and at most
    /// one nonblocking ACK write; expiry never waits for a partial frame.
    pub(super) fn poll_parent(&mut self, parent: &mut UnixStream, nonce: &str) -> io::Result<bool> {
        let mut scratch = [0; MAX_ARM_FRAME];
        let count = match parent.read(&mut scratch) {
            Ok(0) => return Ok(true),
            Ok(count) => count,
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        if self.armed() {
            return Err(invalid("native guardian deadline is already armed"));
        }
        if count > MAX_ARM_FRAME.saturating_sub(self.input.len()) {
            return Err(invalid("native guardian arm exceeds frame bound"));
        }
        self.input.extend_from_slice(
            scratch
                .get(..count)
                .ok_or_else(|| invalid("native guardian arm read count"))?,
        );
        let Some(newline) = self.input.iter().position(|byte| *byte == b'\n') else {
            return if self.input.len() == MAX_ARM_FRAME {
                Err(invalid("native guardian arm lacks terminator"))
            } else {
                Ok(false)
            };
        };
        if newline.checked_add(1) != Some(self.input.len()) {
            return Err(invalid("native guardian arm cannot be pipelined"));
        }
        let arm: Arm = serde_json::from_slice(&self.input).map_err(io::Error::other)?;
        let ArmOp::ArmHardDeadline = arm.op;
        if arm.protocol != PROTOCOL || arm.nonce != nonce {
            return Err(invalid("native guardian arm binding differs"));
        }
        let ack = frame(&json!({
            "state": "hard_deadline_armed", "protocol": PROTOCOL, "nonce": nonce,
            "attempt": arm.attempt, "stage": arm.stage, "expires_ns": arm.expires_ns
        }))?;
        self.arm = Some(arm);
        // Neither supported process-group implementation attributes complete
        // fork and exit usage to this attempt, so start with the hard bound.
        let now = boot_ticks()?;
        self.observe_activity(now, None);
        if self.expires_at().is_some_and(|expires| now >= expires) {
            return Ok(true);
        }
        if parent.write(&ack)? != ack.len() {
            return Err(invalid("partial native guardian arm ACK"));
        }
        Ok(false)
    }
}

/// Same-boot, suspend-inclusive clock for the original absolute deadline.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(unsafe_code, reason = "read the suspend-inclusive native clock")]
pub(super) fn boot_ticks() -> io::Result<Duration> {
    #[cfg(target_os = "linux")]
    let clock = libc::CLOCK_BOOTTIME;
    #[cfg(target_os = "macos")]
    let clock = libc::CLOCK_MONOTONIC;
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: time is aligned and writable until clock_gettime returns.
    if unsafe { libc::clock_gettime(clock, &raw mut time) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds = u64::try_from(time.tv_sec).map_err(io::Error::other)?;
    let nanos = u32::try_from(time.tv_nsec).map_err(io::Error::other)?;
    if nanos >= 1_000_000_000 {
        return Err(io::Error::other("invalid native clock reading"));
    }
    Ok(Duration::new(seconds, nanos))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn boot_ticks() -> io::Result<Duration> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native deadline clock unavailable",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_activity_is_attempt_bound_and_falls_back_without_telemetry() {
        let seconds = Duration::from_secs;
        let mut owner = OwnerDeadline {
            arm: Some(Arm {
                op: ArmOp::ArmHardDeadline,
                protocol: PROTOCOL,
                nonce: String::new(),
                attempt: [7; 16],
                stage: 4,
                expires_ns: 1_000_000_000_000,
            }),
            ..OwnerDeadline::default()
        };
        let observation = |attempt, stage, sequence, at| AuthenticatedActivity {
            attempt,
            stage,
            sequence,
            observed_at: seconds(at),
        };
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(6), Some(observation([8; 16], 4, 1, 5)));
        owner.observe_activity(seconds(6), Some(observation([7; 16], 3, 1, 5)));
        owner.observe_activity(seconds(6), Some(observation([7; 16], 4, 1, 7)));
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(6), Some(observation([7; 16], 4, 1, 5)));
        assert_eq!(owner.expires_at(), Some(seconds(605)));
        owner.observe_activity(seconds(10), None);
        owner.observe_activity(seconds(11), Some(observation([7; 16], 4, 1, 10)));
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(41), Some(observation([7; 16], 4, 2, 40)));
        assert_eq!(owner.expires_at(), Some(seconds(640)));
        owner.observe_activity(seconds(42), Some(observation([7; 16], 4, 3, 39)));
        assert_eq!(owner.expires_at(), Some(seconds(640)));
        owner.observe_activity(seconds(501), Some(observation([7; 16], 4, 3, 500)));
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(1001), Some(observation([7; 16], 4, 4, 1001)));
        assert_eq!(owner.last_activity, Some((3, seconds(500))));

        owner.soft = Some(seconds(700));
        owner.observe_activity(seconds(701), None);
        owner.observe_activity(seconds(701), Some(observation([7; 16], 4, 4, 701)));
        assert_eq!(owner.expires_at(), Some(seconds(700)));
    }
}
