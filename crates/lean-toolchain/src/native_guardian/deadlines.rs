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

#[allow(dead_code, reason = "authenticated activity sources are not wired yet")]
pub(super) enum ActivityObservation {
    Advance(AuthenticatedActivity),
    KnownInactive,
    Unavailable,
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
    command: Option<u64>,
}

#[derive(Deserialize)]
enum ArmOp {
    #[serde(rename = "prepare_hard_deadline")]
    Prepare,
    #[serde(rename = "arm_hard_deadline")]
    Hard,
    #[serde(rename = "arm_reader_hard_deadline")]
    Reader,
    #[serde(rename = "arm_fenced_hard_deadline")]
    Fenced,
    #[serde(rename = "arm_warm_hard_deadline")]
    Warm,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    op: FinishOp,
    protocol: u32,
    nonce: String,
    attempt: [u8; 16],
    stage: u64,
    command: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FinishOp {
    FinishHardDeadline,
}

#[derive(Default)]
pub(super) struct OwnerDeadline {
    input: Vec<u8>,
    arm: Option<Arm>,
    finished: bool,
    idle: Option<(Duration, Duration)>,
    soft: Option<Duration>,
    last_activity: Option<(u64, Duration)>,
}

impl OwnerDeadline {
    pub(super) fn with_idle_limit(limit: Option<Duration>) -> io::Result<Self> {
        let idle = limit
            .map(|limit| boot_ticks().map(|now| (limit, now.saturating_add(limit))))
            .transpose()?;
        Ok(Self {
            idle,
            ..Self::default()
        })
    }

    pub(super) fn armed(&self) -> bool {
        self.arm.is_some()
    }

    pub(super) fn preparing(&self) -> bool {
        self.arm.as_ref().is_some_and(|arm| matches!(arm.op, ArmOp::Prepare))
    }

    pub(super) fn active(&self) -> bool {
        self.armed() && !self.finished
    }

    fn expires_at(&self) -> Option<Duration> {
        self.arm
            .as_ref()
            .filter(|_| !self.finished)
            .map(|arm| self.soft.unwrap_or_else(|| Duration::from_nanos(arm.expires_ns)))
            .or_else(|| self.idle.map(|(_, until)| until))
    }

    pub(super) fn expired(&self) -> io::Result<bool> {
        self.expires_at()
            .map_or(Ok(false), |expires| Ok(boot_ticks()? >= expires))
    }

    /// Missing or unattributable observations use the original hard bound.
    /// Keep the last accepted sequence across gaps so replay cannot renew it.
    pub(super) fn observe_activity(&mut self, now: Duration, activity: ActivityObservation) {
        let Some(arm) = &self.arm else { return };
        let hard = Duration::from_nanos(arm.expires_ns);
        if now >= self.soft.unwrap_or(hard) {
            return;
        }
        let activity = match activity {
            ActivityObservation::Advance(activity) => activity,
            ActivityObservation::KnownInactive => return,
            ActivityObservation::Unavailable => {
                self.soft = None;
                return;
            }
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
    pub(super) fn poll_parent(
        &mut self,
        parent: &mut UnixStream,
        nonce: &str,
        reads_retained: bool,
    ) -> io::Result<bool> {
        let mut scratch = [0; MAX_ARM_FRAME];
        let count = match parent.read(&mut scratch) {
            Ok(0) => return Ok(true),
            Ok(count) => count,
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        if self
            .arm
            .as_ref()
            .is_some_and(|arm| arm.command.is_none() && !matches!(arm.op, ArmOp::Prepare))
        {
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
        let input = std::mem::take(&mut self.input);
        // A finish may arrive late after another warm command is armed.
        // Recognize its operation independently from the active arm state.
        let message: serde_json::Value = serde_json::from_slice(&input).map_err(io::Error::other)?;
        if message.get("op").and_then(serde_json::Value::as_str) == Some("finish_hard_deadline") {
            let Some(arm) = self.arm.as_ref().filter(|_| self.active()) else {
                return Ok(false);
            };
            let Finish {
                op: FinishOp::FinishHardDeadline,
                protocol,
                nonce: received_nonce,
                attempt,
                stage,
                command,
            } = serde_json::from_slice(&input).map_err(io::Error::other)?;
            if protocol != PROTOCOL
                || received_nonce != nonce
                || attempt != arm.attempt
                || stage != arm.stage
                || Some(command) != arm.command
            {
                return Ok(false);
            }
            let ack = frame(&json!({
                "state":"hard_deadline_finished", "protocol":PROTOCOL, "nonce":nonce,
                "attempt":attempt, "stage":stage, "command":command
            }))?;
            // Detect EOF even if the last frame was already queued when
            // the parent closed its write half. Pipelining is forbidden.
            match rustix::net::recv(&*parent, &mut [0_u8; 1][..], rustix::net::RecvFlags::PEEK) {
                Ok((_, 0)) => return Ok(true),
                Ok(_) => return Err(invalid("native guardian finish cannot be pipelined")),
                Err(error) if error == rustix::io::Errno::AGAIN => {}
                Err(error) => return Err(error.into()),
            }
            let now = boot_ticks()?;
            if self.expires_at().is_some_and(|expiry| now >= expiry) {
                return Ok(true);
            }
            self.finished = true;
            if let Some((limit, until)) = &mut self.idle {
                *until = now.saturating_add(*limit);
            }
            if parent.write(&ack)? != ack.len() {
                return Err(invalid("partial native guardian finish ACK"));
            }
            return Ok(false);
        }
        let arm: Arm = serde_json::from_slice(&input).map_err(io::Error::other)?;
        let prepared = self.arm.as_ref().filter(|_| self.preparing());
        let can_dispatch = if let Some(prior) = prepared {
            reads_retained
                && arm.attempt == prior.attempt
                && arm.stage == prior.stage
                && arm.expires_ns == prior.expires_ns
                && match arm.op {
                    ArmOp::Hard => arm.command.is_none(),
                    ArmOp::Fenced | ArmOp::Warm => arm.command == Some(1),
                    ArmOp::Prepare | ArmOp::Reader => false,
                }
        } else {
            match arm.op {
                ArmOp::Warm => self.arm.as_ref().map_or_else(
                    || arm.command == Some(1),
                    |previous| {
                        self.finished
                            && matches!(previous.op, ArmOp::Warm)
                            && arm.command > previous.command
                            && (arm.attempt, arm.stage) != (previous.attempt, previous.stage)
                    },
                ),
                ArmOp::Fenced => !self.armed() && arm.command == Some(1),
                ArmOp::Reader | ArmOp::Hard | ArmOp::Prepare => !self.armed() && arm.command.is_none(),
            }
        };
        if !can_dispatch {
            return Err(invalid("native guardian arm command differs"));
        }
        if matches!(arm.op, ArmOp::Reader) && !reads_retained {
            return Err(invalid("managed reader arm requires retained read descriptors"));
        }
        if arm.protocol != PROTOCOL || arm.nonce != nonce {
            return Err(invalid("native guardian arm binding differs"));
        }
        let mut ack = json!({
            "state": if matches!(arm.op, ArmOp::Prepare) { "hard_deadline_prepared" } else { "hard_deadline_armed" },
            "protocol": PROTOCOL, "nonce": nonce,
            "attempt": arm.attempt, "stage": arm.stage, "expires_ns": arm.expires_ns
        });
        if let Some(command) = arm.command {
            ack.as_object_mut()
                .ok_or_else(|| invalid("invalid arm ACK"))?
                .insert("command".to_owned(), json!(command));
        }
        let ack = frame(&ack)?;
        let now = boot_ticks()?;
        if self.expires_at().is_some_and(|until| now >= until) {
            return Ok(true);
        }
        self.arm = Some(arm);
        self.finished = false;
        self.last_activity = None;
        // Neither supported process-group implementation attributes complete
        // fork and exit usage to this attempt, so start with the hard bound.
        self.observe_activity(now, ActivityObservation::Unavailable);
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
                op: ArmOp::Hard,
                protocol: PROTOCOL,
                nonce: String::new(),
                attempt: [7; 16],
                stage: 4,
                expires_ns: 1_000_000_000_000,
                command: None,
            }),
            ..OwnerDeadline::default()
        };
        let observation = |attempt, stage, sequence, at| {
            ActivityObservation::Advance(AuthenticatedActivity {
                attempt,
                stage,
                sequence,
                observed_at: seconds(at),
            })
        };
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(6), observation([8; 16], 4, 1, 5));
        owner.observe_activity(seconds(6), observation([7; 16], 3, 1, 5));
        owner.observe_activity(seconds(6), observation([7; 16], 4, 1, 7));
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(6), observation([7; 16], 4, 1, 5));
        assert_eq!(owner.expires_at(), Some(seconds(605)));
        owner.observe_activity(seconds(9), ActivityObservation::KnownInactive);
        assert_eq!(owner.expires_at(), Some(seconds(605)));
        owner.observe_activity(seconds(10), ActivityObservation::Unavailable);
        owner.observe_activity(seconds(11), observation([7; 16], 4, 1, 10));
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(41), observation([7; 16], 4, 2, 40));
        assert_eq!(owner.expires_at(), Some(seconds(640)));
        owner.observe_activity(seconds(42), observation([7; 16], 4, 3, 39));
        assert_eq!(owner.expires_at(), Some(seconds(640)));
        owner.observe_activity(seconds(501), observation([7; 16], 4, 3, 500));
        assert_eq!(owner.expires_at(), Some(seconds(1000)));
        owner.observe_activity(seconds(1001), observation([7; 16], 4, 4, 1001));
        assert_eq!(owner.last_activity, Some((3, seconds(500))));

        owner.soft = Some(seconds(700));
        owner.observe_activity(seconds(701), ActivityObservation::Unavailable);
        owner.observe_activity(seconds(701), observation([7; 16], 4, 4, 701));
        assert_eq!(owner.expires_at(), Some(seconds(700)));
    }
}
