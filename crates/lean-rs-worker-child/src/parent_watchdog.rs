//! Optional parent lifeline, established before the Lean runtime starts.
//!
//! The configured independent guardian owns group cleanup and read custody.
//! Legacy opt-in lifelines without that configuration remain self-only.
#![allow(
    unsafe_code,
    reason = "setsid establishes this process session; _exit terminates only this current process"
)]

pub(crate) use lean_toolchain::native_guardian::GuardianHandle;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub(crate) fn install_parent_watchdog() -> io::Result<Option<GuardianHandle>> {
    let Some(socket) = std::env::var_os("LEAN_RS_WORKER_PARENT_SOCKET") else {
        return if std::env::var_os(lean_toolchain::native_guardian::EXECUTABLE_ENV).is_some() {
            Err(io::Error::other("native guardian requires parent lifeline"))
        } else {
            Ok(None)
        };
    };
    let nonce = std::env::var("LEAN_RS_WORKER_PARENT_NONCE").map_err(io::Error::other)?;
    if nonce.len() != 48 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("invalid parent lifeline nonce"));
    }
    // SAFETY: fresh child bootstrap before Lean; scalar setsid failure prevents
    // all runtime/body work, and no foreign process identity is signalled.
    let session = unsafe { libc::setsid() };
    if session < 0 || session as u32 != std::process::id() {
        return Err(io::Error::last_os_error());
    }
    if let Some(executable) = std::env::var_os(lean_toolchain::native_guardian::EXECUTABLE_ENV) {
        return GuardianHandle::spawn(
            Path::new(&executable),
            Path::new(&socket),
            &nonce,
            Duration::from_secs(30),
        )
        .map(Some);
    }
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(format!("{nonce} {}\n", std::process::id()).as_bytes())?;
    let mut acknowledgment = String::new();
    BufReader::new(&stream).take(64).read_line(&mut acknowledgment)?;
    if acknowledgment != "durable-owner-ready\n" {
        return Err(io::Error::other("durable startup ACK missing"));
    }
    stream.set_read_timeout(None)?;
    std::thread::Builder::new()
        .name("lean-parent-lifeline".to_owned())
        .spawn(move || {
            let _observed = std::panic::catch_unwind(move || {
                let mut byte = [0_u8; 1];
                let _delivery = stream.read(&mut byte);
            });
            // SAFETY: EOF, unexpected data, read error or panic exits only this
            // worker; parent capacity still waits for verified session absence.
            unsafe { libc::_exit(125) }
        })?;
    Ok(None)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "isolated bootstrap tests own their sockets and child processes"
)]
#[path = "parent_watchdog/tests.rs"]
mod tests;
