//! Receive actual shared-artifact read custody from the configured helper.
//!
//! Single-domain acquisition fails closed; whole-domain admission can wait only
//! on validated pre-transfer contention, dropping every tentative read first.
//! No path unlocks an open file description. On Darwin, use it only in the worker's pre-body
//! scope: `SCM_RIGHTS` has no atomic CLOEXEC receive there. Multithreaded parents
//! must use the helper's EOF proxy instead.

use std::fs::{self, File, OpenOptions};
use std::io::{self, IoSliceMut, Read};
use std::mem::MaybeUninit;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
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
const MAX_STDERR: usize = 16 * 1024;
const CONTENTION_EXIT: i32 = 75;

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

#[derive(Debug)]
struct ReaderContention {
    stage: &'static str,
    status: Option<ExitStatus>,
    diagnostics: String,
}
impl std::fmt::Display for ReaderContention {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "shared reader admission contention at {}: helper_status={}; {}",
            self.stage,
            status_label(self.status),
            self.diagnostics
        )
    }
}
impl std::error::Error for ReaderContention {}
#[derive(Debug)]
struct HelperFailure {
    stage: &'static str,
    status: Option<ExitStatus>,
    diagnostics: String,
    cause: io::Error,
}
impl std::fmt::Display for HelperFailure {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "shared reader {}: {}; helper_status={}; {}",
            self.stage,
            self.cause,
            status_label(self.status),
            self.diagnostics
        )
    }
}
impl std::error::Error for HelperFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}
fn status_label(status: Option<ExitStatus>) -> String {
    status.map_or_else(|| "not_observed".to_owned(), |status| status.to_string())
}

/// Whether this is validated pre-transfer admission contention, safe for a
/// caller with no other read custody to wait on before launching any body.
///
/// Ordinary `WouldBlock` errors or diagnostic text never establish this fact.
#[must_use]
pub fn is_admission_contention(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|source| source.is::<ReaderContention>())
}
fn check_admission(deadline: Instant, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<()> {
    if cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "shared reader admission cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(timed_out());
    }
    Ok(())
}
fn admission_deadline(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .filter(|_| !timeout.is_zero())
        .ok_or_else(timed_out)
}
fn retry_admission(deadline: Instant, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<()> {
    check_admission(deadline, cancelled)?;
    thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    check_admission(deadline, cancelled)
}
struct Diagnostics {
    pipe: Option<ChildStderr>,
    bytes: Vec<u8>,
    truncated: bool,
}
impl Diagnostics {
    fn empty() -> Self {
        Self {
            pipe: None,
            bytes: Vec::new(),
            truncated: false,
        }
    }
    fn capture(child: &mut Child) -> io::Result<Self> {
        let pipe = child.stderr.take();
        if let Some(pipe) = &pipe {
            fcntl_setfl(pipe, fcntl_getfl(pipe)? | OFlags::NONBLOCK)?;
        }
        Ok(Self {
            pipe,
            bytes: Vec::new(),
            truncated: false,
        })
    }
    fn drain(&mut self) -> io::Result<()> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(());
        };
        let mut ended = false;
        // Bounded work per poll, including after the retained prefix is full.
        for _ in 0..16 {
            let mut buffer = [0_u8; 2048];
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(count) => {
                    let retained = count.min(MAX_STDERR.saturating_sub(self.bytes.len()));
                    self.bytes
                        .extend_from_slice(buffer.get(..retained).ok_or_else(|| invalid("stderr count invalid"))?);
                    self.truncated |= retained < count;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        if ended {
            self.pipe = None;
        }
        Ok(())
    }
    fn text(&self) -> String {
        format!(
            "stderr{}={}",
            if self.truncated { "[truncated]" } else { "" },
            String::from_utf8_lossy(&self.bytes)
        )
    }
}
struct Helper {
    child: Option<Child>,
    reaper: mpsc::Sender<Child>,
    status: Option<ExitStatus>,
    diagnostics: Diagnostics,
}
impl Helper {
    fn spawn(mut command: Command) -> io::Result<Self> {
        let reaper = reserve_reaper()?;
        let child = command.stderr(Stdio::piped()).spawn()?;
        let mut helper = Self {
            child: Some(child),
            reaper,
            status: None,
            diagnostics: Diagnostics::empty(),
        };
        helper.diagnostics =
            Diagnostics::capture(helper.child.as_mut().ok_or_else(|| invalid("reader helper missing"))?)?;
        Ok(helper)
    }
    fn observe_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        self.diagnostics.drain()?;
        if self.status.is_none() {
            self.status = self
                .child
                .as_mut()
                .ok_or_else(|| invalid("reader helper missing"))?
                .try_wait()?;
            if self.status.is_some() {
                self.child = None;
                self.diagnostics.drain()?;
            }
        }
        Ok(self.status)
    }
    fn error(&mut self, stage: &'static str, error: io::Error) -> io::Error {
        let _exit_observation = self.observe_exit();
        io::Error::new(
            error.kind(),
            HelperFailure {
                stage,
                status: self.status,
                diagnostics: self.diagnostics.text(),
                cause: error,
            },
        )
    }
    fn contention(&mut self, stage: &'static str, status: ExitStatus) -> io::Error {
        if let Err(error) = self.diagnostics.drain() {
            return self.error("stderr after pre-transfer exit", error);
        }
        io::Error::new(
            io::ErrorKind::WouldBlock,
            ReaderContention {
                stage,
                status: Some(status),
                diagnostics: self.diagnostics.text(),
            },
        )
    }
    fn finish(&mut self, deadline: Instant, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<ExitStatus> {
        loop {
            check_admission(deadline, cancelled)?;
            if let Some(status) = self.observe_exit()? {
                return Ok(status);
            }
            thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}
// Canonical roots are immutable for this admission. The tuple retains caller
// order while actual acquisitions follow the shared global lock-path order.
fn union_domains(helper: &Path, roots: &[PathBuf]) -> io::Result<Vec<(usize, PathBuf, PathBuf)>> {
    if roots.is_empty() || roots.len() > crate::native_guardian::MAX_READ_DOMAINS {
        return Err(invalid(
            "reader union requires one to thirty-two distinct canonical roots",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut domains = Vec::with_capacity(roots.len());
    for (index, root) in roots.iter().enumerate() {
        validate_reader_root(helper, root)?;
        if !seen.insert(root.clone()) {
            return Err(invalid("reader union roots must be distinct"));
        }
        domains.push((index, root.clone(), crate::shared_writer::project_lock_path(root)?));
    }
    domains.sort_by(|left, right| left.2.cmp(&right.2));
    Ok(domains)
}
fn preflight(domains: &[(usize, PathBuf, PathBuf)]) -> io::Result<()> {
    let locks = domains.iter().map(|(_, _, lock)| lock.clone()).collect::<Vec<_>>();
    match crate::writer_demand::ReadAdmission::try_acquire(&locks) {
        Ok(Some(gates)) => {
            drop(gates);
            Ok(())
        }
        Ok(None) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            ReaderContention {
                stage: "whole-domain preflight",
                status: None,
                diagnostics: "validated writer demand".to_owned(),
            },
        )),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            ReaderContention {
                stage: "whole-domain preflight",
                status: None,
                diagnostics: "validated transient reader gate".to_owned(),
            },
        )),
        Err(error) => Err(error),
    }
}
fn validate_reader_root(helper: &Path, root: &Path) -> io::Result<()> {
    if !helper.is_absolute()
        || !helper.is_file()
        || !root.is_absolute()
        || root.canonicalize()? != root
        || !root.is_dir()
    {
        return Err(invalid("explicit helper and canonical reader root required"));
    }
    validate_lake_if_present(root)
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
        Self::acquire_until(helper, root, admission_deadline(timeout)?, &|| false, false)
    }

    /// Acquire a complete fixed union, releasing every tentative descriptor
    /// before waiting on validated pre-transfer contention. One absolute budget
    /// and cancellation predicate cover all rounds and helper polls.
    ///
    /// The caller must hold no other artifact SH, including guardian loans.
    /// Inherit descriptors or ACK guardian loans only after this returns. Roots
    /// must be distinct canonical directories, at most thirty-two; locks are
    /// acquired in lock-path order, and results correspond to input root order.
    /// Darwin pre-body/no-concurrent-spawn restrictions also apply here.
    /// # Errors
    /// Hard helper, transport, descriptor and identity failures abort without
    /// retry. Deadline or cancellation ends admission before any body is run.
    pub fn acquire_union(
        helper: &Path,
        roots: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Vec<Self>> {
        Self::union_until(helper, roots, admission_deadline(timeout)?, cancelled, true)
    }

    /// Make one complete admission pass, releasing partial reads on failure.
    ///
    /// A warm worker retaining earlier guardian loans must use this rather than
    /// wait behind a writer it may itself exclude. Validated contention is
    /// recognizable through [`is_admission_contention`]; never retry a body.
    /// Result order and pre-body restrictions match [`Self::acquire_union`].
    /// # Errors
    /// Returns validated contention or any hard admission/transfer failure.
    pub fn try_acquire_union(
        helper: &Path,
        roots: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Vec<Self>> {
        Self::union_until(helper, roots, admission_deadline(timeout)?, cancelled, false)
    }
    fn union_until(
        helper: &Path,
        roots: &[PathBuf],
        deadline: Instant,
        cancelled: &(impl Fn() -> bool + ?Sized),
        wait: bool,
    ) -> io::Result<Vec<Self>> {
        check_admission(deadline, cancelled)?;
        let domains = union_domains(helper, roots)?;
        loop {
            check_admission(deadline, cancelled)?;
            let mut readers = Vec::with_capacity(domains.len());
            let pass = (|| {
                preflight(&domains)?; // All gates close before any helper spawn.
                for (index, root, _) in &domains {
                    check_admission(deadline, cancelled)?;
                    readers.push((*index, Self::acquire_until(helper, root, deadline, cancelled, true)?));
                }
                Ok(())
            })();
            match pass {
                Ok(()) => {
                    readers.sort_by_key(|(index, _)| *index);
                    return Ok(readers.into_iter().map(|(_, reader)| reader).collect());
                }
                Err(error) => {
                    drop(readers); // No inherited/guardian aliases exist yet.
                    if !wait || !is_admission_contention(&error) {
                        return Err(error);
                    }
                    retry_admission(deadline, cancelled)?;
                }
            }
        }
    }
    fn acquire_until(
        helper_path: &Path,
        root: &Path,
        deadline: Instant,
        cancelled: &(impl Fn() -> bool + ?Sized),
        report_contention: bool,
    ) -> io::Result<Self> {
        check_admission(deadline, cancelled)?;
        validate_reader_root(helper_path, root)?;
        let lock = crate::shared_writer::project_lock_path(root)?;
        let (directory, nonce) = SocketDirectory::new()?;
        let listener = UnixListener::bind(directory.0.join("fd.sock"))?;
        listener.set_nonblocking(true)?;
        let mut command = Command::new(helper_path);
        command.arg("reader");
        if report_contention {
            command.arg("--report-contention");
        }
        command
            .arg(root)
            .arg(&lock)
            .arg(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64()
                    .to_string(),
            )
            .arg(directory.0.join("fd.sock"))
            .arg(&nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        let mut helper = Helper::spawn(command)?;
        let stream = loop {
            if let Err(error) = check_admission(deadline, cancelled) {
                return Err(helper.error("before descriptor transfer", error));
            }
            helper
                .diagnostics
                .drain()
                .map_err(|error| helper.error("stderr observation before transfer", error))?;
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(status) = helper
                        .observe_exit()
                        .map_err(|error| helper.error("observe helper before transfer", error))?
                    {
                        // Recheck AFTER reap: a helper could have connected and
                        // exited between our first accept and the wait result.
                        match listener.accept() {
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Ok((_pending, _)) => {
                                return Err(helper.error(
                                    "pending descriptor connection after helper exit",
                                    invalid("helper exited with a pending custody transfer"),
                                ));
                            }
                            Err(error) => return Err(helper.error("recheck descriptor listener after exit", error)),
                        }
                        if report_contention && status.code() == Some(CONTENTION_EXIT) {
                            return Err(helper.contention("before descriptor transfer", status));
                        }
                        return Err(helper.error(
                            "before descriptor transfer",
                            invalid("reader helper exited before descriptor transfer"),
                        ));
                    }
                    thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
                }
                Err(error) => return Err(helper.error("accept descriptor transfer", error)),
            }
        };
        let received = {
            let mut poll = || helper.diagnostics.drain();
            receive_while(&stream, root, &lock, &nonce, deadline, cancelled, &mut poll)
        };
        let reader = received.map_err(|error| helper.error("descriptor transfer", error))?;
        let status = helper
            .finish(deadline, cancelled)
            .map_err(|error| helper.error("after descriptor transfer", error))?;
        if !status.success() {
            return Err(helper.error(
                "after descriptor transfer",
                invalid("reader helper failed after descriptor transfer"),
            ));
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

#[cfg(test)]
fn receive(stream: &UnixStream, root: &Path, lock: &Path, nonce: &str, deadline: Instant) -> io::Result<SharedReader> {
    receive_while(stream, root, lock, nonce, deadline, &|| false, &mut || Ok(()))
}
fn receive_while(
    stream: &UnixStream,
    root: &Path,
    lock: &Path,
    nonce: &str,
    deadline: Instant,
    cancelled: &(impl Fn() -> bool + ?Sized),
    poll: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<SharedReader> {
    stream.set_nonblocking(true)?;
    let mut payload = Vec::new();
    let mut descriptors = Vec::new();
    loop {
        check_admission(deadline, cancelled)?;
        poll()?;
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
    diagnostics: Diagnostics,
}

impl SharedReadProxy {
    /// Acquire parent cache custody without receiving any lock descriptor.
    ///
    /// # Errors
    /// Returns admission/receipt/identity errors within the supplied deadline.
    pub fn acquire(helper: &Path, root: &Path, timeout: Duration) -> io::Result<Self> {
        Self::acquire_until(helper, root, admission_deadline(timeout)?, &|| false, false)
    }

    /// Acquire a complete fixed parent-cache union under one admission budget.
    ///
    /// Call only while holding no other artifact SH/EX custody. Every partial
    /// FIFO closes before any contention wait, and each actual helper must exit
    /// normally before retry. Cleanup uncertainty or a post-ready failure aborts.
    /// Canonical distinct roots and result order match [`SharedReader::acquire_union`].
    /// # Errors
    /// Returns timeout/cancellation, hard protocol/identity/helper errors, or
    /// failure to confirm that all tentative proxy helpers have released custody.
    pub fn acquire_union(
        helper: &Path,
        roots: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Vec<Self>> {
        let deadline = admission_deadline(timeout)?;
        check_admission(deadline, cancelled)?;
        let domains = union_domains(helper, roots)?;
        loop {
            check_admission(deadline, cancelled)?;
            let mut proxies = Vec::with_capacity(domains.len());
            let pass = (|| {
                preflight(&domains)?;
                for (index, root, _) in &domains {
                    check_admission(deadline, cancelled)?;
                    proxies.push((*index, Self::acquire_until(helper, root, deadline, cancelled, true)?));
                }
                for (_, proxy) in &mut proxies {
                    proxy.validate()?;
                }
                Ok(())
            })();
            match pass {
                Ok(()) => {
                    proxies.sort_by_key(|(index, _)| *index);
                    return Ok(proxies.into_iter().map(|(_, proxy)| proxy).collect());
                }
                Err(error) => {
                    // Close ALL partial custody endpoints before observing any
                    // helper, so one wait cannot retain a different domain.
                    for (_, proxy) in &mut proxies {
                        drop(proxy.control.take());
                    }
                    let mut cleanup_failure = None;
                    for (_, proxy) in &mut proxies {
                        if cleanup_failure.is_none()
                            && proxy.helper.is_some()
                            && let Err(error) = proxy.confirm_closed(deadline, cancelled)
                        {
                            cleanup_failure = Some(error);
                        }
                        if cleanup_failure.is_some() {
                            proxy.handoff_closed();
                        }
                    }
                    if let Some(failure) = cleanup_failure {
                        return Err(io::Error::new(
                            failure.kind(),
                            format!("{error}; partial custody release failed: {failure}"),
                        ));
                    }
                    if !is_admission_contention(&error) {
                        return Err(error);
                    }
                    retry_admission(deadline, cancelled)?;
                }
            }
        }
    }
    fn acquire_until(
        helper_path: &Path,
        root: &Path,
        deadline: Instant,
        cancelled: &(impl Fn() -> bool + ?Sized),
        report_contention: bool,
    ) -> io::Result<Self> {
        check_admission(deadline, cancelled)?;
        validate_reader_root(helper_path, root)?;
        let lock = crate::shared_writer::project_lock_path(root)?;
        let (directory, nonce) = SocketDirectory::new()?;
        let fifo = directory.0.join("control.fifo");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).map_err(io::Error::from)?;
        let control = OpenOptions::new().read(true).write(true).open(&fifo)?;
        let mut command = Command::new(helper_path);
        command.arg("read-proxy");
        if report_contention {
            command.arg("--report-contention");
        }
        command
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
            .stdout(Stdio::piped());
        let mut helper = Helper::spawn(command)?;
        let mut output = helper
            .child
            .as_mut()
            .and_then(|child| child.stdout.take())
            .ok_or_else(|| invalid("reader proxy output missing"))?;
        fcntl_setfl(&output, fcntl_getfl(&output)? | OFlags::NONBLOCK)?;
        let mut payload = Vec::new();
        let raw = loop {
            if let Err(error) = check_admission(deadline, cancelled) {
                return Err(helper.error("before proxy readiness", error));
            }
            helper
                .diagnostics
                .drain()
                .map_err(|error| helper.error("proxy stderr observation", error))?;
            let mut bytes = [0_u8; 2048];
            match output.read(&mut bytes) {
                Ok(0) => {
                    let status = helper
                        .finish(deadline, cancelled)
                        .map_err(|error| helper.error("proxy EOF before readiness", error))?;
                    // Actual reap plus stdout EOF proves no readiness bytes are
                    // pending. Any prefix, even whitespace, is a hard failure.
                    if report_contention && payload.is_empty() && status.code() == Some(CONTENTION_EXIT) {
                        return Err(helper.contention("before proxy readiness", status));
                    }
                    return Err(helper.error(
                        "proxy EOF before readiness",
                        invalid("reader proxy reply ended before newline"),
                    ));
                }
                Ok(count) => {
                    payload.extend_from_slice(bytes.get(..count).ok_or_else(|| invalid("proxy count invalid"))?);
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                    // A reaped helper with an inherited pipe alias has not
                    // proven empty EOF; wait only within this same deadline.
                    let _status = helper
                        .observe_exit()
                        .map_err(|error| helper.error("observe proxy before readiness", error))?;
                    thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
                    continue;
                }
                Err(error) => return Err(helper.error("read proxy readiness", error)),
            }
            if payload.len() > MAX_FRAME {
                return Err(helper.error("proxy readiness", invalid("reader proxy reply is oversized")));
            }
            if payload.contains(&b'\n') {
                if !one_frame(&payload) {
                    return Err(helper.error("proxy readiness", invalid("reader proxy sent extra frames")));
                }
                break payload;
            }
        };
        let receipt: Receipt = serde_json::from_slice(&raw)
            .map_err(|error| helper.error("decode proxy readiness", io::Error::other(error)))?;
        if receipt.protocol != 1
            || receipt.state != "read_ready"
            || receipt.project_root != root
            || receipt.nonce != nonce
            || receipt.epoch.is_empty()
            || receipt.epoch.len() > MAX_EPOCH
            || receipt.custody.as_deref() != Some("proxy")
        {
            return Err(helper.error(
                "proxy readiness binding",
                invalid("reader proxy receipt binding differs"),
            ));
        }
        let named = fs::symlink_metadata(lock)?;
        if !named.is_file()
            || named.nlink() != 1
            || (named.dev(), named.ino()) != (receipt.lock_identity.device, receipt.lock_identity.inode)
        {
            return Err(helper.error(
                "proxy readiness identity",
                invalid("reader proxy lock identity changed"),
            ));
        }
        if helper.observe_exit()?.is_some() {
            return Err(helper.error("after proxy readiness", invalid("reader proxy exited before owned EOF")));
        }
        Ok(Self {
            control: Some(control),
            helper: helper.child.take(),
            _directory: directory,
            epoch: receipt.epoch,
            reaper: helper.reaper.clone(),
            diagnostics: std::mem::replace(&mut helper.diagnostics, Diagnostics::empty()),
        })
    }
    fn confirm_closed(&mut self, deadline: Instant, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<()> {
        loop {
            if let Err(error) = check_admission(deadline, cancelled) {
                return Err(self.cleanup_error(error));
            }
            self.diagnostics.drain().map_err(|error| self.cleanup_error(error))?;
            let status = self
                .helper
                .as_mut()
                .ok_or_else(|| invalid("proxy cleanup helper missing"))?
                .try_wait()
                .map_err(|error| self.cleanup_error(error))?;
            match status {
                Some(status) => {
                    self.helper = None;
                    self.diagnostics.drain().map_err(|error| self.cleanup_error(error))?;
                    if status.success() {
                        return Ok(());
                    }
                    return Err(self.cleanup_error(invalid(&format!("proxy cleanup helper exited {status}"))));
                }
                None => thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))),
            }
        }
    }
    fn cleanup_error(&self, error: io::Error) -> io::Error {
        io::Error::new(
            error.kind(),
            HelperFailure {
                stage: "partial proxy cleanup unconfirmed",
                status: None,
                diagnostics: self.diagnostics.text(),
                cause: error,
            },
        )
    }
    fn handoff_closed(&mut self) {
        drop(self.control.take());
        if let Some(child) = self.helper.take() {
            let _sent = self.reaper.send(child);
        }
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
        self.diagnostics.drain()?;
        match self
            .helper
            .as_mut()
            .ok_or_else(|| invalid("reader proxy helper missing"))?
            .try_wait()?
        {
            None => Ok(()),
            Some(status) => {
                self.helper = None;
                self.diagnostics.drain()?;
                Err(invalid(&format!(
                    "reader proxy custody helper exited {status}; {}",
                    self.diagnostics.text()
                )))
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

#[cfg(test)]
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

#[cfg(test)]
#[path = "shared_reader/union_tests.rs"]
mod union_tests;
