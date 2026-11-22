//! A link-free guardian for one fixed native worker process group.
//!
//! The worker remains its parent's actual `Child`. This independent helper is
//! the worker's child, in its own process group but the worker's private session.
//! Its live session membership pins the payload group number on macOS. A surviving
//! original parent also retains its actual child unreaped for group-stop fallback.
//! Serialized identities never authorize signals. Payloads must neither regroup nor detach.
//!
//! Read descriptors and their published domains are retained for the generation,
//! including after logical request release. A dead guardian is never closure:
//! managed writers must check the same durable owner records after obtaining EX.

use std::fs::{self, File};
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::{FdFlags, fcntl_setfd};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer, SendAncillaryMessage,
    SendFlags, recvmsg, sendmsg,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Private guardian protocol; it is independent of the worker body protocol.
pub const PROTOCOL: u32 = 1;
/// Explicit, link-free helper selected by the spawning host.
pub const EXECUTABLE_ENV: &str = "LEAN_RS_WORKER_GUARDIAN_EXECUTABLE";
/// One authoritative owner namespace shared by native workers and managed writers.
pub const OWNERS_DIRECTORY_ENV: &str = "LEAN_RS_WORKER_OWNERS_DIRECTORY";
/// Opt into the one-stage stopped-parent drain slot in a matched installation.
pub const DRAIN_SLOT_ENV: &str = "LEAN_RS_WORKER_GUARDIAN_DRAIN_SLOT";
const CONTROL_ENV: &str = "LEAN_RS_NATIVE_GUARDIAN_CONTROL_FD";
const WORKER_ENV: &str = "LEAN_RS_NATIVE_GUARDIAN_WORKER_PID";
const SOCKET_ENV: &str = "LEAN_RS_WORKER_PARENT_SOCKET";
const NONCE_ENV: &str = "LEAN_RS_WORKER_PARENT_NONCE";
/// A generation never retains more than this many distinct artifact domains.
pub const MAX_READ_DOMAINS: usize = 32;
/// Owner admission and writer checks refuse a larger namespace.
pub const MAX_OWNER_RECORDS: usize = 128;
/// Every owner record has a fixed maximum, including the bounded read union.
pub const MAX_OWNER_BYTES: usize = 256 * 1024;
const MAX_FRAME: usize = 16 * 1024;
const POLL: Duration = Duration::from_millis(5);

mod deadlines;

/// Read the same suspend-inclusive boot clock used by the native guardian.
///
/// # Errors
/// Returns an error if the platform clock cannot be read.
#[doc(hidden)]
pub fn guardian_deadline_ticks() -> io::Result<Duration> {
    deadlines::boot_ticks()
}
mod drain_slot;
/// Validate immutable Running ownership before an admitted request uses its
/// existing private guardian lifeline.
///
/// # Errors
/// Refuses stale registry, boot, process identities or unavailable custody.
#[doc(hidden)]
pub fn validate_managed_guardian_scope(record: &Path, expected: &NativeGuardianDrainBinding) -> io::Result<()> {
    let directory = record.parent().ok_or_else(|| invalid("owner directory missing"))?;
    let registry = directory.parent().ok_or_else(|| invalid("owner registry missing"))?;
    let metadata = fs::symlink_metadata(registry)?;
    if !metadata.is_dir()
        || registry.canonicalize()? != registry
        || (metadata.dev(), metadata.ino()) != (expected.registry_device, expected.registry_inode)
        || directory != registry.join(format!("worker-{}", expected.generation))
        || record != directory.join("owner.json")
    {
        return Err(invalid("managed guardian registry binding changed"));
    }
    let value = read_owner(record)?;
    ownership::matches_original_guardian_binding(&value, expected)?;
    if field(&value, "phase") != "running"
        || !expected.parent.is_live()?
        || !expected.worker.is_live()?
        || !expected.guardian.is_live()?
    {
        return Err(invalid("managed guardian scope is not live and running"));
    }
    Ok(())
}

/// Observe the exact current guardian binding for this private Running record.
/// This is identity evidence. The caller retains the separate admission grant.
///
/// # Errors
/// Refuses an invalid or unavailable Running generation.
#[doc(hidden)]
pub fn observe_managed_guardian_binding(record: &Path) -> io::Result<NativeGuardianDrainBinding> {
    let registry = record
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("owner registry missing"))?;
    ownership::validate_registry(registry)?;
    let metadata = fs::symlink_metadata(registry)?;
    let value = read_owner(record)?;
    ownership::verify_original_worker_boot(&value)?;
    let text = |name| {
        field(&value, name)
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("managed owner field {name} missing")))
    };
    let identity = |name| serde_json::from_value(field(&value, name).clone()).map_err(io::Error::other);
    if field(&value, "phase") != "running" {
        return Err(invalid("managed guardian scope is not running"));
    }
    Ok(NativeGuardianDrainBinding {
        registry_device: metadata.dev(),
        registry_inode: metadata.ino(),
        generation: text("generation_id")?,
        boot: text("original_worker_boot")?,
        parent: identity("parent")?,
        worker: identity("process")?,
        guardian: identity("guardian")?,
    })
}

mod inputs;
#[doc(hidden)]
pub use inputs::{cleanup_generation_inputs, create_generation_inputs, generation_inputs};
mod ownership;
#[doc(hidden)]
pub mod writer_storage;
pub use ownership::{
    NativeCapacitySlot, NativeGuardianDrainBinding, NativeGuardianDrainStatus, NativeOwnerObservation,
    NativeProcessIdentity, claim_native_recovery, collect_closed_owner_records, dispatch_owner_record,
    observe_original_guardian_closure, observe_original_guardian_drain, publish_startup_owner, registry_directory,
    request_bound_original_guardian_drain, request_original_guardian_drain, reserve_owner_record,
    reserve_owner_record_with_slot, retire_preparing_owner_record, retire_unspawned_owner_record,
    submit_original_guardian_drain,
};

/// One actual shared lock and epoch protected by a native generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeReadDomain {
    pub project_root: PathBuf,
    pub lock_path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub epoch: String,
}

/// A close-only loan of a descriptor already acquired by `SharedReader`.
pub struct GuardianReadLoan<'a> {
    pub domain: NativeReadDomain,
    pub descriptor: BorrowedFd<'a>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn expired() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "native guardian deadline elapsed")
}
fn deadline(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .filter(|_| !timeout.is_zero())
        .ok_or_else(expired)
}
fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or(&Value::Null)
}
fn valid_nonce(nonce: &str) -> bool {
    nonce.len() == 48 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn pid(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?)
        .ok()
        .filter(|pid| *pid > 1 && i32::try_from(*pid).is_ok())
}

fn frame(value: &Value) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() >= MAX_FRAME {
        return Err(invalid("native guardian frame exceeds limit"));
    }
    bytes.push(b'\n');
    Ok(bytes)
}
fn write_bytes(stream: &mut UnixStream, bytes: &[u8], until: Instant) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= until {
            return Err(expired());
        }
        match stream.write(bytes.get(offset..).ok_or_else(|| invalid("guardian write offset"))?) {
            Ok(0) => return Err(invalid("guardian channel accepted no bytes")),
            Ok(written) => {
                offset = offset
                    .checked_add(written)
                    .ok_or_else(|| invalid("guardian write overflow"))?;
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                thread::sleep(POLL);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn write_frame(stream: &mut UnixStream, value: &Value, until: Instant) -> io::Result<()> {
    write_bytes(stream, &frame(value)?, until)
}
fn read_frame(stream: &mut UnixStream, until: Instant) -> io::Result<Value> {
    let mut bytes = Vec::new();
    loop {
        if Instant::now() >= until {
            return Err(expired());
        }
        // One byte prevents consuming a following control frame into a reader
        // that is about to hand the same stream to the lifecycle loop.
        let mut byte = [0_u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => return Err(invalid("guardian channel closed before ACK")),
            Ok(_) => {
                bytes.extend_from_slice(&byte);
                if bytes.len() > MAX_FRAME {
                    return Err(invalid("guardian frame exceeds limit"));
                }
                if byte == *b"\n" {
                    return serde_json::from_slice(&bytes).map_err(io::Error::other);
                }
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                thread::sleep(POLL);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Worker-side connection. It carries one outstanding bounded descriptor loan.
pub struct GuardianHandle {
    control: UnixStream,
    child: Child,
    nonce: String,
    next_sequence: u64,
}
impl GuardianHandle {
    /// Start the explicit guardian after this worker established its session.
    /// The guardian cannot release runtime startup before the parent durable ACK.
    ///
    /// # Errors
    /// Fails closed on spawn, protocol, descriptor or startup deadline failures.
    pub fn spawn(executable: &Path, socket: &Path, nonce: &str, timeout: Duration) -> io::Result<Self> {
        if !executable.is_absolute() || !executable.is_file() || !valid_nonce(nonce) {
            return Err(invalid("explicit native guardian and valid nonce required"));
        }
        let until = deadline(timeout)?;
        let (mut control, guardian_control) = UnixStream::pair()?;
        control.set_nonblocking(true)?;
        let mut command = Command::new(executable);
        command
            .arg("native-guardian")
            .env(CONTROL_ENV, guardian_control.as_raw_fd().to_string())
            .env(WORKER_ENV, std::process::id().to_string())
            .env(SOCKET_ENV, socket)
            .env(NONCE_ENV, nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = crate::process_group::spawn_inheriting(command, &[guardian_control.as_fd()])?;
        drop(guardian_control);
        let ready = read_frame(&mut control, until)?;
        if field(&ready, "state") != "ready"
            || field(&ready, "protocol") != PROTOCOL
            || field(&ready, "nonce") != nonce
            || pid(field(&ready, "guardian_pid")) != Some(child.id())
        {
            return Err(invalid("native guardian READY differs"));
        }
        Ok(Self {
            control,
            child,
            nonce: nonce.to_owned(),
            next_sequence: 0,
        })
    }

    /// Transfer close-only custody and wait for its durable generation-bound ACK.
    /// A failure never authorizes a body; callers retire the generation.
    ///
    /// # Errors
    /// Rejects excessive domains, dead guardian, ambiguous transfers and deadlines.
    pub fn retain_reads(&mut self, loans: &[GuardianReadLoan<'_>], timeout: Duration) -> io::Result<()> {
        let result = self.retain_reads_inner(loans, timeout);
        if result.is_err() {
            let _stop = self.control.shutdown(std::net::Shutdown::Both);
        }
        result
    }
    fn retain_reads_inner(&mut self, loans: &[GuardianReadLoan<'_>], timeout: Duration) -> io::Result<()> {
        if loans.len() > MAX_READ_DOMAINS {
            return Err(invalid("native read batch exceeds limit"));
        }
        let until = deadline(timeout)?;
        for loan in loans {
            if self.child.try_wait()?.is_some() {
                return Err(invalid("native guardian exited"));
            }
            let sequence = self.next_sequence;
            self.next_sequence = sequence
                .checked_add(1)
                .ok_or_else(|| invalid("native read sequence exhausted"))?;
            let value = json!({"op":"retain","nonce":self.nonce,"sequence":sequence,"domain":loan.domain});
            let bytes = frame(&value)?;
            let descriptors = [loan.descriptor];
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut ancillary = SendAncillaryBuffer::new(&mut space);
            if !ancillary.push(SendAncillaryMessage::ScmRights(&descriptors)) {
                return Err(invalid("native read descriptor exceeds frame"));
            }
            let count = loop {
                if Instant::now() >= until {
                    return Err(expired());
                }
                match sendmsg(
                    &self.control,
                    &[IoSlice::new(&bytes)],
                    &mut ancillary,
                    SendFlags::DONTWAIT,
                ) {
                    Ok(count) => break count,
                    Err(error) if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::INTR => {
                        thread::sleep(POLL);
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            if count == 0 {
                return Err(invalid("native read transfer accepted no bytes"));
            }
            write_bytes(
                &mut self.control,
                bytes.get(count..).ok_or_else(|| invalid("native transfer count"))?,
                until,
            )?;
            let ack = read_frame(&mut self.control, until)?;
            if field(&ack, "state") != "retained"
                || field(&ack, "nonce") != self.nonce.as_str()
                || field(&ack, "sequence").as_u64() != Some(sequence)
                || field(&ack, "domain") != &serde_json::to_value(&loan.domain).map_err(io::Error::other)?
            {
                return Err(invalid("native guardian custody ACK differs"));
            }
        }
        Ok(())
    }
}
impl Drop for GuardianHandle {
    fn drop(&mut self) {
        // EOF requests cleanup. The guardian owns its own cleanup lifetime; a
        // worker cannot wait for it while the worker itself keeps the group live.
        let _shutdown = self.control.shutdown(std::net::Shutdown::Both);
        let _status = self.child.try_wait();
    }
}

#[derive(Default)]
struct Incoming {
    bytes: Vec<u8>,
    descriptors: Vec<OwnedFd>,
}
impl Incoming {
    fn poll(&mut self, stream: &UnixStream) -> io::Result<Option<(Value, Vec<OwnedFd>)>> {
        let mut bytes = vec![0_u8; MAX_FRAME + 1];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        #[cfg(target_os = "linux")]
        let flags = RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = RecvFlags::DONTWAIT;
        let received = match recvmsg(stream, &mut [IoSliceMut::new(&mut bytes)], &mut ancillary, flags) {
            Ok(received) => received,
            Err(error) if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::INTR => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        for message in ancillary.drain() {
            let RecvAncillaryMessage::ScmRights(rights) = message else {
                return Err(invalid("unexpected guardian ancillary data"));
            };
            for descriptor in rights {
                fcntl_setfd(&descriptor, FdFlags::CLOEXEC)?;
                self.descriptors.push(descriptor);
            }
        }
        if received.bytes == 0 {
            return Err(invalid("native worker control closed"));
        }
        if received.flags.intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC) || self.descriptors.len() > 1 {
            return Err(invalid("truncated or extra native read descriptors"));
        }
        self.bytes.extend_from_slice(
            bytes
                .get(..received.bytes)
                .ok_or_else(|| invalid("native read count"))?,
        );
        if self.bytes.len() > MAX_FRAME {
            return Err(invalid("native read frame exceeds limit"));
        }
        let Some(newline) = self.bytes.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        if newline.checked_add(1) != Some(self.bytes.len()) {
            return Err(invalid("multiple native read frames"));
        }
        let value = serde_json::from_slice(&self.bytes).map_err(io::Error::other)?;
        self.bytes.clear();
        Ok(Some((value, std::mem::take(&mut self.descriptors))))
    }
}

fn read_owner(record: &Path) -> io::Result<Value> {
    // Atomic owner publication replaces the inode. A path stat followed by a
    // separate open can observe the old vnode with nlink=0 during rename. Only
    // demonstrated replacement retries; a stable malformed owner stays an error.
    for _attempt in 0..8 {
        let descriptor = rustix::fs::open(
            record,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )?;
        let mut file = File::from(descriptor);
        let opened = file.metadata()?;
        let named = fs::symlink_metadata(record)?;
        if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) || opened.nlink() == 0 || named.nlink() == 0 {
            thread::yield_now();
            continue;
        }
        if !opened.is_file()
            || !named.is_file()
            || opened.nlink() != 1
            || named.nlink() != 1
            || opened.len() > u64::try_from(MAX_OWNER_BYTES).map_err(io::Error::other)?
        {
            return Err(invalid("native owner record identity or size invalid"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(
                u64::try_from(MAX_OWNER_BYTES)
                    .map_err(io::Error::other)?
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)?;
        let after = fs::symlink_metadata(record)?;
        if (opened.dev(), opened.ino()) != (after.dev(), after.ino()) || after.nlink() == 0 {
            thread::yield_now();
            continue;
        }
        if !after.is_file() || after.nlink() != 1 || bytes.len() > MAX_OWNER_BYTES {
            return Err(invalid("native owner record changed shape or exceeded its bound"));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        return validate_owner_record_path(record, value);
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "native owner publication changed during bounded read",
    ))
}
fn validate_owner_record_path(record: &Path, value: Value) -> io::Result<Value> {
    let nonce = field(&value, "generation_id")
        .as_str()
        .filter(|nonce| valid_nonce(nonce))
        .ok_or_else(|| invalid("native owner generation missing"))?;
    let expected = format!("worker-{nonce}");
    if record.file_name().and_then(|name| name.to_str()) != Some("owner.json")
        || record.parent().and_then(Path::file_name).and_then(|name| name.to_str()) != Some(expected.as_str())
    {
        return Err(invalid("native owner record generation path differs"));
    }
    Ok(value)
}
fn persist_owner(record: &Path, value: &Value) -> io::Result<()> {
    persist_owner_at(record, value, "guardian-write")
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RecoveryWriteCut {
    BeforeRename,
    AfterRename,
    DirectorySync,
}
#[cfg(test)]
thread_local! {
    static RECOVERY_WRITE_CUT: std::cell::Cell<Option<RecoveryWriteCut>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
fn interrupt_recovery_write(suffix: &str, cut: RecoveryWriteCut) -> io::Result<()> {
    if suffix == "recovery-write" && RECOVERY_WRITE_CUT.with(|armed| armed.get() == Some(cut) && armed.take().is_some())
    {
        return Err(io::Error::other("injected recovery publication interruption"));
    }
    Ok(())
}

fn persist_owner_at(record: &Path, value: &Value, suffix: &str) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_OWNER_BYTES {
        return Err(invalid("native owner union exceeds limit"));
    }
    let temporary = record.with_extension(suffix);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    #[cfg(test)]
    interrupt_recovery_write(suffix, RecoveryWriteCut::BeforeRename)?;
    fs::rename(&temporary, record)?;
    #[cfg(test)]
    interrupt_recovery_write(suffix, RecoveryWriteCut::AfterRename)?;
    sync_owner_directory_for(record, suffix)
}
fn sync_owner_directory_for(record: &Path, suffix: &str) -> io::Result<()> {
    #[cfg(test)]
    interrupt_recovery_write(suffix, RecoveryWriteCut::DirectorySync)?;
    #[cfg(not(test))]
    let _ = suffix;
    File::open(record.parent().ok_or_else(|| invalid("owner directory missing"))?)?.sync_all()
}
// Called under the admission lock by this exact live guardian only. Staging is
// never closure evidence; only the fsynced owner.json publication is authoritative.
fn persist_closure_owner(record: &Path, value: &Value) -> io::Result<()> {
    let temporary = record.with_extension("closure-write");
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_OWNER_BYTES as u64 {
                return Err(invalid("closure staging identity differs"));
            }
            let bytes = fs::read(&temporary)?;
            if bytes.len() > MAX_OWNER_BYTES {
                return Err(invalid("closure staging exceeds bound"));
            }
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(staged) if staged == *value => {
                    File::open(&temporary)?.sync_all()?;
                    fs::rename(&temporary, record)?;
                    return File::open(record.parent().ok_or_else(|| invalid("owner directory missing"))?)?.sync_all();
                }
                Ok(_) => return Err(invalid("closure staging belongs to another receipt")),
                Err(_) => fs::remove_file(&temporary)?, // This exact guardian's partial staging only.
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    persist_owner_at(record, value, "closure-write")
}

fn validate_domain(domain: &NativeReadDomain, file: &File) -> io::Result<()> {
    if domain.epoch.is_empty()
        || domain.epoch.len() > 4096
        || !domain.project_root.is_absolute()
        || domain.project_root.canonicalize()? != domain.project_root
        || crate::shared_writer::project_lock_path(&domain.project_root)? != domain.lock_path
    {
        return Err(invalid("native read domain binding differs"));
    }
    let actual = file.metadata()?;
    let named = fs::symlink_metadata(&domain.lock_path)?;
    if !actual.is_file()
        || actual.nlink() != 1
        || !named.is_file()
        || named.nlink() != 1
        || (actual.dev(), actual.ino()) != (domain.device, domain.inode)
        || (named.dev(), named.ino()) != (domain.device, domain.inode)
    {
        return Err(invalid("native read lock identity differs"));
    }
    Ok(())
}

struct Guardian {
    worker: u32,
    nonce: String,
    control: UnixStream,
    parent: Option<UnixStream>,
    parent_identity: Option<NativeProcessIdentity>,
    record: Option<PathBuf>,
    next_sequence: u64,
    loans: Vec<(NativeReadDomain, File)>,
    incoming: Incoming,
    bootstrap_identity: Option<(NativeProcessIdentity, NativeProcessIdentity)>,
}
impl Guardian {
    fn prepare_bootstrap_record(&mut self) -> io::Result<()> {
        let root = std::env::var_os(OWNERS_DIRECTORY_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| invalid("native owners registry missing"))?;
        ownership::validate_registry(&root)?;
        let record = root.join(format!("worker-{}", self.nonce)).join("owner.json");
        let value = read_owner(&record)?;
        ownership::validate_owner_header(&value)?;
        if field(&value, "generation_id") != self.nonce.as_str() {
            return Err(invalid("bootstrap generation differs"));
        }
        let worker = NativeProcessIdentity::capture(self.worker)?.ok_or_else(|| invalid("bootstrap worker absent"))?;
        let guardian = NativeProcessIdentity::capture(std::process::id())?.ok_or_else(|| invalid("guardian absent"))?;
        let worker_owns_session = worker.pid == worker.session && worker.pid == worker.process_group;
        let guardian_shares_worker_session = guardian.session == worker.pid;
        let guardian_owns_group = guardian.process_group == guardian.pid;
        if !worker_owns_session || !guardian_shares_worker_session || !guardian_owns_group {
            return Err(invalid("bootstrap identity scope differs"));
        }
        self.record = Some(record);
        self.bootstrap_identity = Some((worker, guardian));
        Ok(())
    }
    fn publish_bootstrap_closed(&self) -> io::Result<()> {
        let record = self
            .record
            .as_ref()
            .ok_or_else(|| invalid("bootstrap record unknown"))?;
        let (worker, guardian) = self
            .bootstrap_identity
            .as_ref()
            .ok_or_else(|| invalid("bootstrap identity unknown"))?;
        let _lock = ownership::lock_publication(record, Duration::from_secs(2))?;
        let mut value = read_owner(record)?;
        ownership::validate_owner_header(&value)?;
        if field(&value, "generation_id") != self.nonce.as_str() {
            return Err(invalid("bootstrap closure generation differs"));
        }
        if field(&value, "phase") == "running" {
            return Ok(());
        }
        if field(&value, "phase") == "bootstrap_closed" {
            if field(&value, "process") != &serde_json::to_value(worker).map_err(io::Error::other)?
                || field(&value, "guardian") != &serde_json::to_value(guardian).map_err(io::Error::other)?
            {
                return Err(invalid("durable bootstrap closure identity differs"));
            }
            return File::open(record.parent().ok_or_else(|| invalid("owner directory missing"))?)?.sync_all();
        }
        if !matches!(field(&value, "phase").as_str(), Some("dispatching" | "bootstrap")) {
            return Err(invalid("bootstrap closure phase differs"));
        }
        if crate::process_group::group_present(self.worker)? || !guardian.is_live()? {
            return Err(invalid("bootstrap closure not observed"));
        }
        let object = value.as_object_mut().ok_or_else(|| invalid("owner object missing"))?;
        object.insert(
            "process".to_owned(),
            serde_json::to_value(worker).map_err(io::Error::other)?,
        );
        object.insert(
            "guardian".to_owned(),
            serde_json::to_value(guardian).map_err(io::Error::other)?,
        );
        object.insert("phase".to_owned(), json!("bootstrap_closed"));
        persist_closure_owner(record, &value)
    }
    fn run(&mut self, socket: &Path, timeout: Duration) -> io::Result<()> {
        self.prepare_bootstrap_record()?;
        let until = deadline(timeout)?;
        let mut parent = UnixStream::connect(socket)?;
        parent.set_nonblocking(true)?;
        write_frame(
            &mut parent,
            &json!({"protocol":PROTOCOL,"nonce":self.nonce,
            "worker_pid":self.worker,"guardian_pid":std::process::id()}),
            until,
        )?;
        let ack = read_frame(&mut parent, until)?;
        if field(&ack, "state") != "durable-owner-ready" || field(&ack, "nonce") != self.nonce.as_str() {
            return Err(invalid("durable native guardian ACK missing"));
        }
        let record = PathBuf::from(
            field(&ack, "record")
                .as_str()
                .ok_or_else(|| invalid("native owner path missing"))?,
        );
        if self.record.as_ref() != Some(&record) {
            return Err(invalid("ACK changed bootstrap owner path"));
        }
        let value = read_owner(&record)?;
        ownership::validate_owner_header(&value)?;
        if field(&value, "version") == 3 {
            ownership::verify_original_worker_boot(&value)?;
        }
        if !matches!(field(&value, "version").as_u64(), Some(2 | 3))
            || field(&value, "generation_id") != self.nonce.as_str()
            || pid(field(field(&value, "process"), "pid")) != Some(self.worker)
            || pid(field(field(&value, "guardian"), "pid")) != Some(std::process::id())
            || field(&value, "phase") != "running"
            || field(&value, "protected_domains")
                .as_array()
                .is_none_or(|domains| !domains.is_empty())
        {
            return Err(invalid("durable native guardian record differs"));
        }
        let (worker, guardian) = self
            .bootstrap_identity
            .as_ref()
            .ok_or_else(|| invalid("guardian bootstrap identity missing"))?;
        let use_drain_slot = std::env::var_os(DRAIN_SLOT_ENV).is_some_and(|mode| mode == "1");
        #[cfg(all(test, target_os = "macos"))]
        tests::pause_before_slot_advertisement(use_drain_slot)?;
        let drain_guardian = ownership::publish_guardian_drain_support(&record, worker, guardian, use_drain_slot)?
            .then(|| guardian.clone());
        let mut sampled_stopped = false;
        self.parent_identity = Some(serde_json::from_value(field(&value, "parent").clone()).map_err(io::Error::other)?);
        self.record = Some(record.clone());
        self.parent = Some(parent);
        write_frame(
            &mut self.control,
            &json!({"state":"ready","protocol":PROTOCOL,
            "nonce":self.nonce,"guardian_pid":std::process::id()}),
            until,
        )?;
        let mut hard_deadline = deadlines::OwnerDeadline::default();
        let mut last_recovery_poll = Instant::now();
        loop {
            if hard_deadline.expired()? {
                return Ok(());
            }
            // The server endpoint exists only in the server, never in payloads.
            // getppid also detects root death when fork-only children retained
            // copies of the worker-control endpoint despite CLOEXEC.
            if native::parent_pid() != self.worker
                || !self
                    .parent_identity
                    .as_ref()
                    .ok_or_else(|| invalid("native server identity missing"))?
                    .is_live()?
                || hard_deadline.poll_parent(
                    self.parent
                        .as_mut()
                        .ok_or_else(|| invalid("native guardian parent missing"))?,
                    &self.nonce,
                )?
            {
                return Ok(());
            }
            // An armed stage must not enter a synchronous registry read.
            // Its original deadline and lifeline remain the stop authority.
            if !hard_deadline.armed()
                && let Some(guardian) = &drain_guardian
                && last_recovery_poll.elapsed() >= Duration::from_millis(50)
            {
                last_recovery_poll = Instant::now();
                // An unavailable registry never supplies a request. The
                // independently owned deadline and lifeline remain active.
                let decision = if use_drain_slot {
                    ownership::consume_guardian_drain_slot(&record, guardian, &mut sampled_stopped)
                } else {
                    ownership::original_guardian_drain_requested(&record, guardian)
                };
                if decision.is_ok_and(|requested| requested) {
                    return Ok(());
                }
            }
            if let Some((request, descriptors)) = self.incoming.poll(&self.control)? {
                // Retain publishes files synchronously. No active stage may
                // enter that path or wait for loan ACK backpressure.
                if hard_deadline.armed() {
                    return Err(invalid("read loan after native deadline arm"));
                }
                self.retain(&request, descriptors)?;
            }
            thread::sleep(POLL);
        }
    }
    fn retain(&mut self, request: &Value, mut descriptors: Vec<OwnedFd>) -> io::Result<()> {
        if field(request, "op") != "retain"
            || field(request, "nonce") != self.nonce.as_str()
            || field(request, "sequence").as_u64() != Some(self.next_sequence)
            || descriptors.len() != 1
        {
            return Err(invalid("native read request binding differs"));
        }
        let domain: NativeReadDomain =
            serde_json::from_value(field(request, "domain").clone()).map_err(io::Error::other)?;
        let file = File::from(
            descriptors
                .pop()
                .ok_or_else(|| invalid("native read descriptor missing"))?,
        );
        validate_domain(&domain, &file)?;
        if let Some((previous, _)) = self
            .loans
            .iter()
            .find(|(previous, _)| previous.lock_path == domain.lock_path)
        {
            if previous != &domain {
                return Err(invalid("native read domain changed within generation"));
            }
        } else {
            if self.loans.len() >= MAX_READ_DOMAINS {
                return Err(invalid("native generation read union full"));
            }
            // Store first: any publication/ACK failure retains the actual loan
            // until the common shutdown drain confirms payload-group closure.
            self.loans.push((domain.clone(), file));
            let record = self
                .record
                .as_ref()
                .ok_or_else(|| invalid("native owner record missing"))?;
            let _lock = ownership::lock_publication(record, Duration::from_secs(2))?;
            let mut value = read_owner(record)?;
            let domains: Vec<_> = self.loans.iter().map(|(domain, _)| domain).collect();
            value
                .as_object_mut()
                .ok_or_else(|| invalid("native owner record must be object"))?
                .insert(
                    "protected_domains".to_owned(),
                    serde_json::to_value(domains).map_err(io::Error::other)?,
                );
            persist_owner(record, &value)?;
        }
        let sequence = self.next_sequence;
        self.next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| invalid("native sequence exhausted"))?;
        write_frame(
            &mut self.control,
            &json!({"state":"retained","nonce":self.nonce,
            "sequence":sequence,"domain":domain}),
            deadline(Duration::from_secs(2))?,
        )
    }
    #[cfg(target_os = "macos")]
    fn publish_requested_zombie_closure(&self) -> io::Result<bool> {
        let record = self
            .record
            .as_ref()
            .ok_or_else(|| invalid("guardian owner record missing"))?;
        let (worker, guardian) = self
            .bootstrap_identity
            .as_ref()
            .ok_or_else(|| invalid("guardian bootstrap identity missing"))?;
        let value = read_owner(record)?;
        if field(&value, "guardian_drain") != "requested" {
            return Ok(false);
        }
        ownership::publish_guardian_zombie_closure(record, worker, guardian)
    }

    fn drain(&self) {
        loop {
            match crate::process_group::group_present(self.worker) {
                Ok(false) => {
                    #[cfg(all(test, target_os = "macos"))]
                    tests::pause_after_payload_closed(self);
                    if self.publish_bootstrap_closed().is_ok() {
                        return;
                    }
                }
                Ok(true) => {
                    #[cfg(target_os = "macos")]
                    if self.publish_requested_zombie_closure().is_ok_and(|closed| closed) {
                        #[cfg(test)]
                        tests::pause_after_zombie_receipt(self);
                        return;
                    }
                    #[cfg(all(test, target_os = "macos"))]
                    if tests::hold_requested_group_signal(self) || tests::hold_service_loss_group_signal() {
                        thread::sleep(POLL);
                        continue;
                    }
                    let _requested = native::kill_payload(self.worker);
                }
                Err(_) => {}
            }
            // No failed kill/observation releases loans or erases ownership.
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod closure_publication_tests {
    use super::*;

    #[test]
    fn requested_drain_retries_directory_sync_after_uncertain_rename() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("lean-drain-retry-{}", std::process::id()));
        fs::create_dir(&root)?;
        let record = root.join("owner.json");
        let initial = json!({"guardian_drain":"available"});
        fs::write(&record, serde_json::to_vec(&initial)?)?;
        let read = || -> io::Result<Value> { Ok(serde_json::from_slice(&fs::read(&record)?)?) };
        let mut value = initial.clone();
        RECOVERY_WRITE_CUT.with(|cut| cut.set(Some(RecoveryWriteCut::BeforeRename)));
        assert!(ownership::publish_guardian_drain_request(&record, &mut value).is_err());
        assert_eq!(read()?, initial);
        assert!(record.with_extension("recovery-write").exists());
        let mut value = read()?;
        RECOVERY_WRITE_CUT.with(|cut| cut.set(Some(RecoveryWriteCut::AfterRename)));
        assert!(ownership::publish_guardian_drain_request(&record, &mut value).is_err());
        let mut requested = read()?;
        assert_eq!(field(&requested, "guardian_drain"), "requested");
        assert!(!record.with_extension("recovery-write").exists());
        RECOVERY_WRITE_CUT.with(|cut| cut.set(Some(RecoveryWriteCut::DirectorySync)));
        assert!(ownership::publish_guardian_drain_request(&record, &mut requested).is_err());
        assert_eq!(read()?, requested);
        ownership::publish_guardian_drain_request(&record, &mut requested)?;
        fs::remove_dir_all(root)
    }
    #[test]
    fn interrupted_closure_staging_is_not_evidence_and_can_retry() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("lean-closure-retry-{}", std::process::id()));
        fs::create_dir(&root)?;
        let record = root.join("owner.json");
        let value = json!({"phase":"bootstrap_closed","generation_id":"test"});
        fs::write(&record, b"dispatching")?;
        fs::write(record.with_extension("closure-write"), b"{partial")?;
        assert_eq!(fs::read(&record)?, b"dispatching");
        persist_closure_owner(&record, &value)?;
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&record)?).map_err(io::Error::other)?,
            value
        );
        fs::write(
            record.with_extension("closure-write"),
            serde_json::to_vec(&value).map_err(io::Error::other)?,
        )?;
        persist_closure_owner(&record, &value)?;
        assert!(!record.with_extension("closure-write").exists());
        fs::remove_dir_all(root)
    }
}

/// Run the hidden link-free helper mode. Inputs arrive only through the private
/// bootstrap environment/descriptor; ordinary stdout is never used.
///
/// # Errors
/// Reports malformed bootstrap or failed setup only after owned work has drained.
pub fn run_guardian_cli() -> io::Result<()> {
    let worker = std::env::var(WORKER_ENV)
        .map_err(io::Error::other)?
        .parse::<u32>()
        .map_err(io::Error::other)?;
    native::validate_pin(worker)?;
    let descriptor = std::env::var(CONTROL_ENV)
        .map_err(io::Error::other)?
        .parse::<i32>()
        .map_err(io::Error::other)?;
    let control = native::adopt_control(descriptor)?;
    control.set_nonblocking(true)?;
    let nonce = std::env::var(NONCE_ENV).map_err(io::Error::other)?;
    let socket = std::env::var_os(SOCKET_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("native parent socket missing"))?;
    let mut guardian = Guardian {
        worker,
        nonce,
        control,
        parent: None,
        parent_identity: None,
        record: None,
        next_sequence: 0,
        loans: Vec::new(),
        incoming: Incoming::default(),
        bootstrap_identity: None,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !valid_nonce(&guardian.nonce) {
            return Err(invalid("native guardian nonce invalid"));
        }
        guardian.run(&socket, Duration::from_secs(30))
    }));
    guardian.drain();
    match result {
        Ok(result) => result,
        Err(_) => Err(io::Error::other("native guardian panicked; owned group drained")),
    }
}

/// Check recorded native read protection after acquiring the exclusive lock.
///
/// Match the canonical stable lock path before checking inode identity. Unknown,
/// malformed, oversized, inaccessible or excessive records never grant admission.
/// This passively observes groups and never signals a recorded numeric identity.
///
/// # Errors
/// Returns an unavailable record/observation or a boundedness/identity violation.
pub fn protected_native_reads_stopped(registry: &Path, lock_path: &Path, identity: (u64, u64)) -> io::Result<bool> {
    let entries = match fs::read_dir(registry) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    ownership::validate_registry(registry)?;
    let mut count = 0_usize;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".admission.lock" || writer_storage::is_charge_directory(&entry)? {
            continue;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("native owner count overflow"))?;
        if count > MAX_OWNER_RECORDS {
            return Err(invalid("native owner namespace exceeds limit"));
        }
        if !entry.file_type()?.is_dir() {
            return Err(invalid("unexpected native owner namespace entry"));
        }
        let record = entry.path().join("owner.json");
        let value = match read_owner(&record) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match ownership::read_or_collect_unpublished(registry, &entry.path()) {
                    Ok(Some(value)) => value,
                    Ok(None) => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        let version = field(&value, "version").as_u64();
        // A raw EX candidate cannot replace the registry's custody check.
        // A fully closed scope needs no access to its former artifact paths.
        if ownership::record_can_be_removed(&value)? || field(&value, "phase") == "preparing" {
            continue;
        }
        // Dispatching and legacy records have no usable domain binding.
        if field(&value, "phase") == "dispatching" || version == Some(1) {
            return Ok(false);
        }
        let domains: Vec<NativeReadDomain> =
            serde_json::from_value(field(&value, "protected_domains").clone()).map_err(io::Error::other)?;
        if domains.len() > MAX_READ_DOMAINS {
            return Err(invalid("native owner domain union exceeds limit"));
        }
        // Only the matching recorded path can conflict. Do not reopen an
        // unrelated guardian's project paths, which may already be gone.
        if let Some(domain) = domains.into_iter().find(|domain| domain.lock_path == lock_path) {
            if domain.project_root.canonicalize()? != domain.project_root
                || crate::shared_writer::project_lock_path(&domain.project_root)? != domain.lock_path
            {
                return Err(invalid("invalid native protected domain"));
            }
            if (domain.device, domain.inode) != identity {
                return Err(invalid("protected native lock inode changed"));
            }
            return Ok(false);
        }
    }
    Ok(true)
}

mod native {
    #![allow(
        unsafe_code,
        reason = "narrow scalar process/session operations and unique inherited descriptor adoption"
    )]
    use super::{FdFlags, FromRawFd, UnixStream, fcntl_setfd, invalid, io};
    pub(super) fn parent_pid() -> u32 {
        // SAFETY: getppid is a scalar query with no memory or ownership effects.
        unsafe { libc::getppid() as u32 }
    }
    pub(super) fn validate_pin(worker: u32) -> io::Result<()> {
        let worker = i32::try_from(worker).map_err(io::Error::other)?;
        // SAFETY: scalar queries inspect this guardian's current session/group.
        let session = unsafe { libc::getsid(0) };
        // SAFETY: getpgrp observes only the current process's group.
        let group = unsafe { libc::getpgrp() };
        if worker <= 1 || session != worker || group != std::process::id() as i32 || group == worker {
            return Err(invalid("native guardian does not pin the worker private session"));
        }
        Ok(())
    }
    pub(super) fn kill_payload(worker: u32) -> io::Result<()> {
        validate_pin(worker)?;
        let target = i32::try_from(worker)
            .map_err(io::Error::other)?
            .checked_neg()
            .ok_or_else(|| invalid("native group invalid"))?;
        // SAFETY: this live guardian's same-SID membership pins the numeric
        // payload group. Managed payloads never regroup; this guardian never
        // joins that group. Its exact scope was checked before every signal.
        if unsafe { libc::kill(target, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        Err(io::Error::last_os_error())
    }
    pub(super) fn adopt_control(descriptor: i32) -> io::Result<UnixStream> {
        if descriptor <= 2 {
            return Err(invalid("native guardian control descriptor invalid"));
        }
        // SAFETY: numeric fcntl safely rejects invalid integers. It returns a
        // fresh descriptor owned only here; the supplied descriptor is never
        // closed or assigned fabricated Rust ownership.
        let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this successful duplication created one unique owned handle.
        let stream = unsafe { UnixStream::from_raw_fd(duplicate) };
        stream.peer_addr()?;
        fcntl_setfd(&stream, FdFlags::CLOEXEC)?;
        Ok(stream)
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "native_guardian/tests.rs"]
mod tests;
