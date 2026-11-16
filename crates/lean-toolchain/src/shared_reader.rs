//! Receive actual shared-artifact read custody from the configured helper.
//!
//! This client makes no admission or recovery decisions and never unlocks an
//! open file description. On Darwin, use it only in the worker's pre-body
//! scope: `SCM_RIGHTS` has no atomic CLOEXEC receive there. Multithreaded parents
//! must use the helper's EOF proxy instead.

use std::fs::{self, File, OpenOptions};
use std::io::{self, IoSliceMut, Read};
use std::mem::MaybeUninit;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg};
use serde::Deserialize;

const MAX_FRAME: usize = 16 * 1024;
const MAX_EPOCH: usize = 4096;
const POLL: Duration = Duration::from_millis(2);

/// An actual shared lock and the writer epoch observed while acquiring it.
/// Dropping this value closes its alias; it never invokes flock unlock.
#[derive(Debug)]
pub struct SharedReader {
    file: File,
    epoch: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    protocol: u32,
    state: String,
    project_root: PathBuf,
    nonce: String,
    epoch: String,
    lock_identity: LockIdentity,
    #[serde(default)]
    custody: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LockIdentity {
    device: u64,
    inode: u64,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "shared reader acquisition deadline elapsed")
}

// Lock authority is the canonical sibling file, so a cold root needs no .lake
// directory. Existing build trees must still be directories in this same domain.
fn validate_lake_if_present(root: &Path) -> io::Result<()> {
    let lake = root.join(".lake");
    match lake.canonicalize() {
        Ok(canonical) if canonical == lake && lake.is_dir() => Ok(()),
        Ok(_) => Err(invalid("existing .lake directory is aliased or not a directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::symlink_metadata(&lake) {
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(invalid("existing .lake directory is aliased or not a directory")),
        },
        Err(error) => Err(error),
    }
}

struct SocketDirectory(PathBuf);
impl SocketDirectory {
    fn new() -> io::Result<(Self, String)> {
        let mut bytes = [0_u8; 24];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut nonce = String::with_capacity(48);
        for byte in bytes {
            nonce.push(char::from(
                *HEX.get(usize::from(byte >> 4))
                    .ok_or_else(|| invalid("nonce encoding failed"))?,
            ));
            nonce.push(char::from(
                *HEX.get(usize::from(byte & 15))
                    .ok_or_else(|| invalid("nonce encoding failed"))?,
            ));
        }
        // Keep Unix socket paths below Darwin's short sockaddr_un limit.
        let directory = Path::new("/tmp")
            .canonicalize()?
            .join(format!("lean-read-{}", &nonce[..24]));
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        Ok((Self(directory), nonce))
    }
}
impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _socket = fs::remove_file(self.0.join("fd.sock"));
        let _fifo = fs::remove_file(self.0.join("control.fifo"));
        let _directory = fs::remove_dir(&self.0);
    }
}

// Reserve reaping before spawn, so bounded Drop never abandons a Child.
fn reserve_reaper() -> io::Result<mpsc::Sender<Child>> {
    let (sender, receiver) = mpsc::channel::<Child>();
    thread::Builder::new()
        .name("lean-reader-reaper".to_owned())
        .spawn(move || {
            if let Ok(mut child) = receiver.recv() {
                // Waiting never signals a PID and can safely report ECHILD if an
                // embedding host installed its own SIGCHLD reaper.
                while let Err(error) = child.wait() {
                    if error.kind() != io::ErrorKind::Interrupted {
                        break;
                    }
                }
            }
        })?;
    Ok(sender)
}

struct Helper {
    child: Option<Child>,
    reaper: mpsc::Sender<Child>,
}
impl Helper {
    fn finish(&mut self, deadline: Instant) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .ok_or_else(|| invalid("reader helper missing"))?
                .try_wait()?
            {
                self.child = None;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(timed_out());
            }
            thread::sleep(POLL);
        }
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        if matches!(child.try_wait(), Ok(None)) {
            // Only our unreaped pure reader helper, never proof/writer work.
            let _kill = child.kill();
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(100))
            .unwrap_or_else(Instant::now);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(None) => thread::sleep(POLL),
                Ok(Some(_)) => return,
                Err(_) => break,
            }
        }
        // The receiver performs no fallible work before recv and remains
        // alive until this sender disconnects, so this first send cannot fail.
        let _sent = self.reaper.send(child);
    }
}

impl SharedReader {
    /// Acquire one descriptor from the trusted helper within one admission deadline.
    ///
    /// The canonical root must exist; a missing `.lake` stays untouched. An
    /// existing `.lake` must be an unaliased directory in that root. On
    /// Darwin this belongs only in a worker pre-body scope with no concurrent
    /// process spawning; the parent cache path must use the EOF proxy.
    ///
    /// # Errors
    /// Rejects timeout, failed helper, malformed/extra descriptors, changed
    /// lock identity, aliases, or a mismatched root/nonce/receipt. Failed pure
    /// helper cleanup may take at most another 100 milliseconds.
    pub fn acquire(helper: &Path, root: &Path, timeout: Duration) -> io::Result<Self> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(timed_out)?;
        if !helper.is_absolute() || !helper.is_file() || !root.is_absolute() || root.canonicalize()? != root {
            return Err(invalid("explicit helper and canonical reader root required"));
        }
        validate_lake_if_present(root)?;
        let lock = crate::shared_writer::project_lock_path(root)?;
        let (directory, nonce) = SocketDirectory::new()?;
        let socket = directory.0.join("fd.sock");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let reaper = reserve_reaper()?;
        let mut helper = Helper {
            child: Some(
                Command::new(helper)
                    .arg("reader")
                    .arg(root)
                    .arg(&lock)
                    .arg(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .as_secs_f64()
                            .to_string(),
                    )
                    .arg(&socket)
                    .arg(&nonce)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
            ),
            reaper,
        };
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    if helper
                        .child
                        .as_mut()
                        .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
                    {
                        return Err(invalid("reader helper exited before descriptor transfer"));
                    }
                    thread::sleep(POLL);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Err(timed_out()),
                Err(error) => return Err(error),
            }
        };
        let reader = receive(&stream, root, &lock, &nonce, deadline)?;
        if !helper.finish(deadline)?.success() {
            return Err(invalid("reader helper failed after descriptor transfer"));
        }
        Ok(reader)
    }

    /// Writer epoch captured under the received shared lock.
    #[must_use]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Borrow a validated close-only read loan for the independent guardian.
    /// The actual worker retains this alias until the guardian durable ACK.
    ///
    /// # Errors
    /// Returns changed domain or descriptor identity observations.
    pub fn guardian_loan(&self, root: &Path) -> io::Result<crate::native_guardian::GuardianReadLoan<'_>> {
        use std::os::fd::AsFd as _;
        let lock_path = crate::shared_writer::project_lock_path(root)?;
        let actual = self.file.metadata()?;
        let named = fs::symlink_metadata(&lock_path)?;
        if root.canonicalize()? != root
            || !named.is_file()
            || named.nlink() != 1
            || (actual.dev(), actual.ino()) != (named.dev(), named.ino())
        {
            return Err(invalid("native guardian read identity changed"));
        }
        Ok(crate::native_guardian::GuardianReadLoan {
            domain: crate::native_guardian::NativeReadDomain {
                project_root: root.to_path_buf(),
                lock_path,
                device: actual.dev(),
                inode: actual.ino(),
                epoch: self.epoch.clone(),
            },
            descriptor: self.file.as_fd(),
        })
    }

    /// Let actual body children inherit custody. Call only inside the worker,
    /// before imports/body spawning; never change the daemon parent's FD flags.
    ///
    /// # Errors
    /// Returns the OS error when descriptor inheritance cannot be established.
    pub fn inherit_by_body_children(&self) -> io::Result<()> {
        fcntl_setfd(&self.file, FdFlags::empty()).map_err(Into::into)
    }
}

fn receive(stream: &UnixStream, root: &Path, lock: &Path, nonce: &str, deadline: Instant) -> io::Result<SharedReader> {
    stream.set_nonblocking(true)?;
    let mut payload = Vec::new();
    let mut descriptors = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        let mut bytes = vec![0_u8; MAX_FRAME + 1];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        #[cfg(target_os = "linux")]
        let flags = RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = RecvFlags::DONTWAIT;
        let message = match recvmsg(stream, &mut [IoSliceMut::new(&mut bytes)], &mut control, flags) {
            Ok(message) => message,
            Err(error) if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::INTR => {
                thread::sleep(POLL);
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        for ancillary in control.drain() {
            let RecvAncillaryMessage::ScmRights(rights) = ancillary else {
                return Err(invalid("unexpected reader ancillary data"));
            };
            for fd in rights {
                fcntl_setfd(&fd, FdFlags::CLOEXEC)?;
                descriptors.push(fd);
            }
        }
        if message.flags.intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC) || descriptors.len() > 1 {
            return Err(invalid("truncated or extra reader descriptors"));
        }
        if message.bytes == 0 {
            return Err(invalid("reader receipt ended before newline"));
        }
        payload.extend_from_slice(
            bytes
                .get(..message.bytes)
                .ok_or_else(|| invalid("reader byte count exceeds buffer"))?,
        );
        if payload.len() > MAX_FRAME {
            return Err(invalid("reader receipt is oversized"));
        }
        if payload.contains(&b'\n') {
            break;
        }
    }
    if !one_frame(&payload) || descriptors.len() != 1 {
        return Err(invalid("exactly one reader frame and descriptor required"));
    }
    let receipt: Receipt = serde_json::from_slice(&payload).map_err(io::Error::other)?;
    if receipt.protocol != 1
        || receipt.state != "read_ready"
        || receipt.project_root != root
        || receipt.nonce != nonce
        || receipt.epoch.is_empty()
        || receipt.epoch.len() > MAX_EPOCH
        || receipt.custody.is_some()
    {
        return Err(invalid("reader receipt binding differs"));
    }
    let file = File::from(descriptors.pop().ok_or_else(|| invalid("reader descriptor missing"))?);
    let actual = file.metadata()?;
    let named = fs::symlink_metadata(lock)?;
    if !actual.is_file()
        || !named.is_file()
        || actual.nlink() != 1
        || named.nlink() != 1
        || (actual.dev(), actual.ino()) != (named.dev(), named.ino())
        || (actual.dev(), actual.ino()) != (receipt.lock_identity.device, receipt.lock_identity.inode)
    {
        return Err(invalid("reader lock identity changed"));
    }
    Ok(SharedReader {
        file,
        epoch: receipt.epoch,
    })
}

/// Parent-side read scope whose only custody endpoint is an atomically
/// CLOEXEC-opened FIFO. The helper owns the shared lock until FIFO EOF.
pub struct SharedReadProxy {
    control: Option<File>,
    helper: Option<Child>,
    _directory: SocketDirectory,
    epoch: String,
    reaper: mpsc::Sender<Child>,
}

impl SharedReadProxy {
    /// Acquire parent cache custody without receiving any lock descriptor.
    ///
    /// # Errors
    /// Returns admission/receipt/identity errors within the supplied deadline.
    pub fn acquire(helper: &Path, root: &Path, timeout: Duration) -> io::Result<Self> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(timed_out)?;
        if !helper.is_absolute() || !helper.is_file() || !root.is_absolute() || root.canonicalize()? != root {
            return Err(invalid("explicit helper and canonical proxy root required"));
        }
        validate_lake_if_present(root)?;
        let lock = crate::shared_writer::project_lock_path(root)?;
        let (directory, nonce) = SocketDirectory::new()?;
        let fifo = directory.0.join("control.fifo");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).map_err(io::Error::from)?;
        // std File::open adds O_CLOEXEC in the open syscall on Darwin. Unlike
        // Stdio::piped/SCM receive, no inheritable custody window exists here.
        let control = OpenOptions::new().read(true).write(true).open(&fifo)?;
        let reaper = reserve_reaper()?;
        let mut child = Command::new(helper)
            .arg("read-proxy")
            .arg(root)
            .arg(&lock)
            .arg(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64()
                    .to_string(),
            )
            .arg(&nonce)
            .arg(&fifo)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| invalid("reader proxy output missing"))?;
        let mut proxy = Self {
            control: Some(control),
            helper: Some(child),
            _directory: directory,
            epoch: String::new(),
            reaper,
        };
        fcntl_setfl(&output, fcntl_getfl(&output)? | OFlags::NONBLOCK)?;
        let receipt: Receipt = serde_json::from_slice(&read_line(output, deadline)?).map_err(io::Error::other)?;
        if receipt.protocol != 1
            || receipt.state != "read_ready"
            || receipt.project_root != root
            || receipt.nonce != nonce
            || receipt.epoch.is_empty()
            || receipt.epoch.len() > MAX_EPOCH
            || receipt.custody.as_deref() != Some("proxy")
        {
            return Err(invalid("reader proxy receipt binding differs"));
        }
        let named = fs::symlink_metadata(lock)?;
        if !named.is_file()
            || named.nlink() != 1
            || (named.dev(), named.ino()) != (receipt.lock_identity.device, receipt.lock_identity.inode)
        {
            return Err(invalid("reader proxy lock identity changed"));
        }
        proxy.epoch = receipt.epoch;
        Ok(proxy)
    }

    /// Writer epoch observed while the helper acquired the shared lock.
    #[must_use]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Confirm the pure custody helper has not failed before accepting a cache hit.
    ///
    /// # Errors
    /// Returns an error if helper exit or OS observation invalidates custody.
    pub fn validate(&mut self) -> io::Result<()> {
        match self
            .helper
            .as_mut()
            .ok_or_else(|| invalid("reader proxy helper missing"))?
            .try_wait()?
        {
            None => Ok(()),
            Some(_) => {
                self.helper = None;
                Err(invalid("reader proxy custody helper exited"))
            }
        }
    }
}

impl Drop for SharedReadProxy {
    fn drop(&mut self) {
        drop(self.control.take());
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(100))
            .unwrap_or_else(Instant::now);
        if let Some(mut child) = self.helper.take() {
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(None) => thread::sleep(POLL),
                    Ok(Some(_)) => return,
                    Err(_) => break,
                }
            }
            // Reaping was reserved before spawn; no later thread creation can
            // lose this Child. No signal and no explicit flock unlock.
            let _sent = self.reaper.send(child);
        }
    }
}

fn read_line(mut input: impl Read, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        let mut bytes = [0_u8; 2048];
        match input.read(&mut bytes) {
            Ok(0) => return Err(invalid("reader proxy reply ended before newline")),
            Ok(count) => payload.extend_from_slice(
                bytes
                    .get(..count)
                    .ok_or_else(|| invalid("proxy byte count exceeds buffer"))?,
            ),
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                thread::sleep(POLL);
                continue;
            }
            Err(error) => return Err(error),
        }
        if payload.len() > MAX_FRAME {
            return Err(invalid("reader proxy reply is oversized"));
        }
        if payload.contains(&b'\n') {
            if !one_frame(&payload) {
                return Err(invalid("reader proxy sent extra frames"));
            }
            return Ok(payload);
        }
    }
}

fn one_frame(payload: &[u8]) -> bool {
    payload.strip_suffix(b"\n").is_some_and(|body| !body.contains(&b'\n'))
}

#[cfg(test)]
#[path = "shared_reader/tests.rs"]
mod tests;
