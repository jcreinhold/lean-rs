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
}

impl OwnerDeadline {
    pub(super) fn armed(&self) -> bool {
        self.arm.is_some()
    }

    pub(super) fn expired(&self) -> io::Result<bool> {
        self.arm.as_ref().map_or(Ok(false), |arm| {
            Ok(boot_ticks()? >= Duration::from_nanos(arm.expires_ns))
        })
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
        if self.expired()? {
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
