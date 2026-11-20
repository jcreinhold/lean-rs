//! Stable writer domains and the configured helper's owned transaction client.
//!
//! Caller and helper share close-only lock descriptions through local writes and
//! actual command cleanup. On macOS the helper confirms its private process-group
//! scope; descendants must retain that group. A lost result is separate from stop
//! evidence, and an unknown scope never authorizes another command.

use std::fs::File;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::writer_lock::WriterLock;

/// Typed worker configuration is transported through this private environment key.
pub const SHARED_WRITER_EXECUTABLE_ENV: &str = "LEAN_HOST_MCP_WRITER_EXECUTABLE";
const MAX_FRAME: u64 = 1024 * 1024;
const LOCK_LOANS_ENV: &str = "LEAN_HOST_MCP_WRITER_LOCK_LOANS";
const WORKSPACES_ENV: &str = "LEAN_HOST_MCP_WRITER_WORKSPACES";
/// Maximum close-only project and source-entry lock descriptions in one helper.
pub const MAX_WRITER_LOCK_LOANS: usize = 64;

/// Resolve a bounded canonical artifact-domain union from Lake's flattened manifest.
///
/// This same graph is suitable for cooperating readers and writers. Missing or
/// unsupported dependency metadata fails closed; arbitrary configuration I/O and
/// uncoordinated external Lake invocations remain outside this contract.
///
/// # Errors
/// Returns malformed, missing, oversized, or unsupported dependency metadata errors.
pub fn project_artifact_roots(root: &Path) -> io::Result<Vec<PathBuf>> {
    crate::write_domains::artifact_roots(root)
}

/// Resolve one bounded artifact union across independently runnable workspaces.
///
/// Uses the same parser as [`project_artifact_roots`], also checking unsafe Git
/// replacement ancestry across the combined graph. Unrelated workspaces still
/// require the documented stable-parent ownership contract.
///
/// # Errors
/// Returns unsupported metadata, graph-size, or unsafe replacement-layout errors.
pub fn project_artifact_root_union(workspaces: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    crate::write_domains::workspace_roots(workspaces)
}

/// Expand workspace seeds and retain separately frozen physical artifact domains.
///
/// Only `workspaces` are interpreted as independently runnable Lake projects.
/// `frozen_domains` must be canonical absolute directories and are merged without
/// reading their manifests, even when a workspace no longer discovers them.
/// The complete union retains the same size, deduplication and Git-cleanup
/// ancestry checks. Unrelated domains require stable-parent ownership.
///
/// # Errors
/// Returns unsupported workspace metadata, noncanonical or missing frozen
/// directories, graph-size violations, or unsafe replacement-layout errors.
pub fn project_artifact_roots_with_domains(
    workspaces: &[PathBuf],
    frozen_domains: &[PathBuf],
) -> io::Result<Vec<PathBuf>> {
    crate::write_domains::workspace_roots_with_domains(workspaces, frozen_domains)
}

/// Decode helper startup workspace roots; their actual domains still require locks.
///
/// # Errors
/// Rejects noncanonical, duplicate, oversized, or missing-primary workspace lists.
pub fn inherited_writer_workspaces(primary: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(encoded) = std::env::var_os(WORKSPACES_ENV) else {
        return Ok(vec![canonical_project_root(primary)?]);
    };
    let roots: Vec<PathBuf> = serde_json::from_str(
        encoded
            .to_str()
            .ok_or_else(|| invalid("writer workspaces must be UTF-8"))?,
    )
    .map_err(io::Error::other)?;
    if roots.is_empty()
        || roots.len() > crate::write_domains::MAX_ROOTS
        || roots.first().is_none_or(|root| root != primary)
    {
        return Err(invalid("writer workspace list has no bounded primary"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for root in &roots {
        if canonical_project_root(root)? != *root || !seen.insert(root.clone()) {
            return Err(invalid("writer workspaces must be distinct canonical roots"));
        }
    }
    Ok(roots)
}

/// Actual command-execution evidence attached to a writer failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriterDisposition {
    /// No command was released by this operation.
    NotStarted,
    /// The owned command scope is confirmed stopped; its result is unavailable.
    Stopped,
    /// Complete owned-scope closure has not been confirmed.
    Unknown,
}

/// A writer failure preserving process evidence independently of result bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriterFailure {
    /// Strongest confirmed execution disposition for this operation.
    pub disposition: WriterDisposition,
    /// Human-readable failure detail, not a source of execution evidence.
    pub detail: String,
    /// Validated helper stop receipt, when closure was directly confirmed.
    pub stop_receipt: Option<Value>,
}
impl std::fmt::Display for WriterFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.disposition, self.detail)
    }
}
impl std::error::Error for WriterFailure {}
impl WriterFailure {
    /// Recover execution evidence preserved inside a standard I/O error.
    #[must_use]
    pub fn from_io(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }
}

pub(crate) fn failure(disposition: WriterDisposition, error: io::Error) -> io::Error {
    if WriterFailure::from_io(&error).is_some() {
        return error;
    }
    io::Error::new(
        error.kind(),
        WriterFailure {
            disposition,
            detail: error.to_string(),
            stop_receipt: None,
        },
    )
}

fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or(&Value::Null)
}

fn valid_process(identity: &Value, owner: &Value) -> bool {
    field(identity, "pid")
        .as_u64()
        .is_some_and(|pid| pid > 1 && i32::try_from(pid).is_ok())
        && field(identity, "process_group") == field(identity, "pid")
        && field(identity, "session") == field(owner, "session")
        && identity
            .get("start")
            .is_none_or(|start| start.as_str().is_some_and(|start| !start.is_empty()))
        && identity.get("start_new_session").is_none_or(|value| value == false)
}

fn stopped_failure(error: &io::Error, receipt: &Value) -> io::Error {
    io::Error::new(
        error.kind(),
        WriterFailure {
            disposition: WriterDisposition::Stopped,
            detail: error.to_string(),
            stop_receipt: Some(receipt.clone()),
        },
    )
}

/// A bundled Lean package strictly below its owning Rust Cargo package.
///
/// This explicit mode bootstraps disjoint package-local outputs before a writer
/// executable exists. It never follows an absent helper into an arbitrary root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateLakeBootstrap {
    project_root: PathBuf,
    owner_root: PathBuf,
    bundled_host_interop: bool,
}

impl PrivateLakeBootstrap {
    /// Bind a canonical bundled Lean subdirectory to its actual Cargo owner.
    ///
    /// # Errors
    /// Rejects roots outside the owner, the owner root itself, and non-packages.
    pub fn new(owner_cargo_package: &Path, project_root: &Path) -> io::Result<Self> {
        let owner = owner_cargo_package.canonicalize()?;
        let project_root = project_root.canonicalize()?;
        let manifest = std::fs::read_to_string(owner.join("Cargo.toml"))?;
        let manifest: toml::Value = toml::from_str(&manifest).map_err(io::Error::other)?;
        if manifest.get("package").is_none() || project_root == owner || !project_root.starts_with(&owner) {
            return Err(invalid("private Lean bootstrap must be below its owning Cargo package"));
        }
        if project_artifact_roots(&project_root)?
            .iter()
            .any(|root| root == &owner || !root.starts_with(&owner))
        {
            return Err(invalid(
                "private Lean bootstrap dependencies must stay below their Cargo owner",
            ));
        }
        let bundled_host_interop = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            == Some("lean-rs-host")
            && project_root == owner.join("shims/lean-rs-interop-shims");
        Ok(Self {
            project_root,
            owner_root: owner,
            bundled_host_interop,
        })
    }

    pub(crate) fn is_bundled_host_interop(&self, project_root: &Path) -> bool {
        self.bundled_host_interop && self.project_root == project_root
    }

    pub(crate) fn covers_roots(&self, roots: &[PathBuf]) -> bool {
        roots
            .iter()
            .all(|root| root != &self.owner_root && root.starts_with(&self.owner_root))
    }

    /// The primary Lake workspace authorized by this private bootstrap.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

/// Resolve one stable sibling lock for a project, including a not-yet-created root.
///
/// Existing symlink aliases resolve to the same domain. Replacement of the final
/// project directory does not replace this file; its parent must not be removed
/// or renamed while any cooperating reader or writer can remain active.
///
/// # Errors
/// Returns an I/O error for an unavailable parent or a root without one normal basename.
#[allow(
    clippy::indexing_slicing,
    reason = "both hexadecimal digit indices are masked to the sixteen-byte alphabet"
)]
pub fn project_lock_path(root: &Path) -> io::Result<PathBuf> {
    let root = canonical_project_root(root)?;
    let parent = root.parent().ok_or_else(|| invalid("project root has no parent"))?;
    #[cfg(unix)]
    let bytes = root.as_os_str().as_bytes();
    #[cfg(not(unix))]
    let bytes = root.as_os_str().as_encoded_bytes();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::from(".lean-rs-writer-");
    for byte in Sha256::digest(bytes) {
        name.push(char::from(HEX[(byte >> 4) as usize]));
        name.push(char::from(HEX[(byte & 15) as usize]));
    }
    name.push_str(".lock");
    Ok(parent.join(name))
}

/// Retained shared custody of the complete project artifact union.
#[cfg(unix)]
#[derive(Debug)]
#[doc(hidden)]
#[must_use = "retain custody across the readiness probe and its handoff"]
pub struct LocalArtifactRead {
    files: Vec<(PathBuf, File)>,
}

#[cfg(unix)]
impl LocalArtifactRead {
    /// Try one read admission without waiting, writing or spawning helpers.
    ///
    /// # Errors
    /// Returns `WouldBlock` for contention and other errors for invalid
    /// lock files or an unavailable record.
    pub fn try_acquire(roots: &[PathBuf]) -> io::Result<Self> {
        use crate::writer_demand::{ReadAdmission, open_read_only_lock};
        use crate::writer_lock::validate_artifact_lock;
        let mut paths = roots
            .iter()
            .map(|root| project_lock_path(root))
            .collect::<io::Result<Vec<_>>>()?;
        paths.sort();
        paths.dedup();
        let busy = || io::Error::new(io::ErrorKind::WouldBlock, "artifact reader admission is busy");
        let admission = ReadAdmission::try_acquire_existing(&paths)?.ok_or_else(busy)?;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let file = open_read_only_lock(&path)?;
            validate_artifact_lock(&file, &path, None)?;
            match fs4::FileExt::try_lock_shared(&file) {
                Ok(()) => {}
                Err(fs4::TryLockError::WouldBlock) => return Err(busy()),
                Err(fs4::TryLockError::Error(error)) => return Err(error),
            }
            validate_artifact_lock(&file, &path, None)?;
            files.push((path, file));
        }
        drop(admission);
        let guard = Self { files };
        guard.validate()?;
        Ok(guard)
    }

    /// Check every retained descriptor against its named lock and prior writer.
    ///
    /// # Errors
    /// Returns invalid lock, uncertain writer closure or filesystem errors.
    pub fn validate(&self) -> io::Result<()> {
        for (path, file) in &self.files {
            crate::writer_lock::validate_artifact_lock(file, path, None)?;
            if !previous_writer_stopped(file)? {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "previous artifact writer has not stopped",
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn canonical_project_root(root: &Path) -> io::Result<PathBuf> {
    match root.canonicalize() {
        Ok(root) => Ok(root),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let name = root
                .file_name()
                .ok_or_else(|| invalid("project root requires one normal basename"))?;
            if !matches!(
                Path::new(name).components().next(),
                Some(std::path::Component::Normal(_))
            ) {
                return Err(invalid("project root requires one normal basename"));
            }
            let parent = root
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Ok(parent.canonicalize()?.join(name))
        }
        Err(error) => Err(error),
    }
}

/// Test a previous writer's recorded group without acquiring signaling authority.
///
/// This is the common passive admission check for both source-entry and project
/// domains. Malformed committed records and observation failures never prove stop.
/// A torn record precedes GO or follows confirmed closure in the helper protocol.
///
/// # Errors
/// Returns record or operating-system observation errors.
#[cfg(unix)]
pub fn previous_writer_stopped(file: &File) -> io::Result<bool> {
    let mut input = file;
    input.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    input.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(invalid("writer ownership record is oversized"));
    }
    if bytes.is_empty() || bytes.last() != Some(&b'\n') {
        return Ok(true);
    }
    let record: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let owner = field(&record, "owner");
    let owner_pid = field(owner, "pid")
        .as_u64()
        .filter(|pid| *pid > 0 && i32::try_from(*pid).is_ok());
    if field(&record, "version") != 1
        || field(owner, "version") != 1
        || owner_pid.is_none()
        || field(owner, "session") != field(owner, "pid")
        || field(owner, "start").as_str().is_none_or(str::is_empty)
    {
        return Err(invalid("writer ownership record has no exact owner"));
    }
    let process = record
        .get("process")
        .ok_or_else(|| invalid("writer ownership record omits process disposition"))?;
    if process.is_null() {
        return Ok(true);
    }
    let group = field(process, "pid")
        .as_u64()
        .filter(|pid| *pid > 0 && i32::try_from(*pid).is_ok())
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| invalid("writer ownership record has no process group"))?;
    if field(process, "process_group") != field(process, "pid")
        || field(process, "session") != field(owner, "session")
        || field(process, "start_new_session") != false
        || field(process, "start").as_str().is_none_or(str::is_empty)
    {
        return Err(invalid("writer ownership record has no exact group"));
    }
    Ok(!crate::process_group::group_present(
        u32::try_from(group.as_raw_pid()).map_err(io::Error::other)?,
    )?)
}

#[cfg(unix)]
#[derive(Deserialize, Serialize)]
struct LockLoan {
    fd: i32,
    path: PathBuf,
    device: u64,
    inode: u64,
}

/// Duplicate and validate startup lock loans without closing arbitrary inherited descriptors.
///
/// Intended for the configured helper before its locked acknowledgement. Each
/// returned File shares the caller's open file description. Close-only custody
/// must remain until its protected local writes and owned command groups stop.
///
/// # Errors
/// Returns invalid-descriptor, identity, record, or passive ownership errors.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "fcntl duplicates an integer descriptor into a fresh owned File; original descriptors are never closed"
)]
pub fn inherited_writer_locks() -> io::Result<Option<Vec<(PathBuf, File)>>> {
    let Some(encoded) = std::env::var_os(LOCK_LOANS_ENV) else {
        return Ok(None);
    };
    let loans: Vec<LockLoan> =
        serde_json::from_str(encoded.to_str().ok_or_else(|| invalid("lock loans must be UTF-8"))?)
            .map_err(io::Error::other)?;
    if loans.is_empty() || loans.len() > MAX_WRITER_LOCK_LOANS {
        return Err(invalid("one to sixty-four writer loans required"));
    }
    let mut result = Vec::new();
    let mut paths = std::collections::BTreeSet::new();
    for loan in loans {
        if !paths.insert(loan.path.clone()) {
            return Err(invalid("writer loan domain is duplicated"));
        }
        if loan.fd <= 2 {
            return Err(invalid("writer loan cannot name standard I/O"));
        }
        // SAFETY: fcntl accepts an integer, returns a fresh descriptor or -1,
        // and never closes the supplied possibly untrusted descriptor.
        let duplicate = unsafe { libc::fcntl(loan.fd, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this successful fcntl returned a fresh uniquely owned descriptor.
        let file = unsafe { File::from_raw_fd(duplicate) };
        fs4::FileExt::try_lock(&file).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "writer loan is a different locked description",
            ),
            fs4::TryLockError::Error(error) => error,
        })?;
        let metadata = file.metadata()?;
        let named = std::fs::symlink_metadata(&loan.path)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || !named.is_file()
            || metadata.dev() != loan.device
            || metadata.ino() != loan.inode
            || (named.dev(), named.ino()) != (loan.device, loan.inode)
            || loan
                .path
                .parent()
                .ok_or_else(|| invalid("writer loan has no parent"))?
                .canonicalize()?
                != loan.path.parent().ok_or_else(|| invalid("writer loan has no parent"))?
            || !previous_writer_stopped(&file)?
            || !crate::native_guardian::protected_native_reads_stopped(
                &crate::native_guardian::registry_directory()?,
                &loan.path,
                (metadata.dev(), metadata.ino()),
            )?
        {
            return Err(invalid("writer loan identity or previous ownership is unresolved"));
        }
        result.push((loan.path, file));
    }
    Ok(Some(result))
}

/// Describe loaned lock identities for the helper's custody acknowledgement.
///
/// # Errors
/// Returns a metadata error for any loan.
#[cfg(unix)]
pub fn writer_lock_receipt(locks: &[(PathBuf, File)]) -> io::Result<Value> {
    let mut result = Vec::new();
    for (path, file) in locks {
        let metadata = file.metadata()?;
        result.push(json!({"path":path,"device":metadata.dev(),"inode":metadata.ino()}));
    }
    Ok(Value::Array(result))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn text(value: &std::ffi::OsStr) -> io::Result<&str> {
    value
        .to_str()
        .ok_or_else(|| invalid("shared writer command must be UTF-8"))
}

#[cfg(unix)]
fn send_until(
    input: &mut ChildStdin,
    value: &Value,
    deadline: Instant,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> io::Result<()> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_FRAME {
        return Err(invalid("shared writer control frame is oversized"));
    }
    fcntl_setfl(&*input, fcntl_getfl(&*input)? | OFlags::NONBLOCK)?;
    let mut offset = 0;
    while offset < bytes.len() {
        if cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "writer launch cancelled"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "writer launch deadline expired",
            ));
        }
        match input.write(
            bytes
                .get(offset..)
                .ok_or_else(|| invalid("writer launch offset invalid"))?,
        ) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "writer launch channel closed")),
            Ok(count) => {
                offset = offset
                    .checked_add(count)
                    .ok_or_else(|| invalid("writer launch offset overflow"))?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(remaining.min(Duration::from_millis(20)));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
struct Helper {
    input: Option<ChildStdin>,
    id: u32,
}

#[cfg(unix)]
impl Helper {
    #[allow(
        clippy::expect_used,
        clippy::unwrap_in_result,
        reason = "the pre-created receiver cannot exit before this first send; see the proof obligation at send"
    )]
    fn spawn(command: Command, fds: &[BorrowedFd<'_>]) -> io::Result<(Self, mpsc::Receiver<io::Result<Value>>)> {
        let (children, incoming) = mpsc::sync_channel::<Child>(1);
        let (sender, receiver) = mpsc::sync_channel(4);
        // Establish reaping ownership before a helper can exist. Failure to create
        // this thread therefore cannot abandon a process or dispatch a command.
        thread::Builder::new()
            .name("lean-writer-reaper".to_owned())
            .spawn(move || {
                let Ok(mut child) = incoming.recv() else { return };
                let _observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Some(output) = child.stdout.take() {
                        let mut reader = io::BufReader::new(output);
                        let mut deliver = true;
                        loop {
                            let mut bytes = Vec::new();
                            let result = reader.by_ref().take(MAX_FRAME + 1).read_until(b'\n', &mut bytes);
                            let read_failed = result.is_err();
                            let frame = match result {
                                Ok(0) => break,
                                Ok(_) if bytes.len() as u64 > MAX_FRAME || bytes.last() != Some(&b'\n') => {
                                    Err(invalid("writer reply is incomplete or oversized"))
                                }
                                Ok(_) => serde_json::from_slice::<Value>(&bytes).map_err(io::Error::other),
                                Err(error) => Err(error),
                            };
                            if deliver && sender.try_send(frame).is_err() {
                                deliver = false;
                            }
                            if read_failed {
                                break;
                            }
                        }
                    }
                }));
                // A lost reply seals the caller, but H remains the cleanup owner.
                // Only this thread owns its Child; EOF is not an exit observation.
                while child.wait().is_err() {
                    thread::sleep(Duration::from_millis(20));
                }
            })?;
        let mut child = crate::process_group::spawn_inheriting(command, fds)?;
        let input = child.stdin.take();
        let id = child.id();
        // Proof obligation: the receiver thread executes no fallible or
        // panicking operation before recv, and only exits after this sender
        // disconnects. Thus this first send cannot lose its receiver.
        children.send(child).expect("writer reaper exists before helper launch");
        let input = input.ok_or_else(|| invalid("writer stdin missing"))?;
        Ok((Self { input: Some(input), id }, receiver))
    }
}

#[cfg(unix)]
impl Drop for Helper {
    fn drop(&mut self) {
        // EOF forbids later launches and asks H to retire its actual work. The
        // already-running observer retains and reaps H after this caller exits.
        drop(self.input.take());
    }
}

/// One helper lifetime, retained across commands and caller-owned metadata writes.
#[cfg(unix)]
pub struct SharedWriter {
    helper: Helper,
    receiver: mpsc::Receiver<io::Result<Value>>,
    owner: Value,
    root: PathBuf,
    write_roots: Vec<PathBuf>,
    workspaces: Vec<PathBuf>,
    active: bool,
    asset_dir: PathBuf,
    _controller_locks: Vec<File>,
}

#[cfg(unix)]
impl SharedWriter {
    /// Acquire the configured process-bound writer before any command launch.
    ///
    /// # Errors
    /// Returns helper launch, admission, or handshake errors.
    pub fn open(executable: &Path, root: &Path, timeout: Duration) -> io::Result<Self> {
        Self::open_cancellable(executable, root, timeout, &|| false)
    }

    /// Acquire writer custody with one bounded, cancellable admission wait.
    ///
    /// # Errors
    /// Returns cancellation, lock, launch, or custody-handshake errors before GO.
    pub fn open_cancellable(
        executable: &Path,
        root: &Path,
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        let root = root
            .canonicalize()
            .map_err(|error| failure(WriterDisposition::NotStarted, error))?;
        Self::open_with_write_roots(executable, &root, &[], timeout, cancelled)
    }

    /// Acquire one helper for all declared workspaces and their artifact domains.
    ///
    /// Roots may be prospective only when their parent exists. A caller that
    /// materializes such a root must prove its payload has zero dependencies or
    /// supply the complete already-prepared workspace union before writing it.
    /// Commands must keep all outputs within these domains or helper-owned assets.
    ///
    /// # Errors
    /// Returns graph, lock, cancellation, launch, or custody errors before GO.
    pub fn open_with_write_roots(
        executable: &Path,
        root: &Path,
        prepared_roots: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        Self::open_with_admission_timeout(executable, root, prepared_roots, timeout, timeout, cancelled)
    }

    pub(crate) fn open_immediately(
        executable: &Path,
        root: &Path,
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        Self::open_with_admission_timeout(executable, root, &[], timeout, Duration::ZERO, cancelled)
    }

    fn open_with_admission_timeout(
        executable: &Path,
        root: &Path,
        prepared_roots: &[PathBuf],
        timeout: Duration,
        lock_timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        (|| {
            let started = Instant::now();
            let root = canonical_project_root(root)?;
            let mut workspaces = vec![root.clone()];
            for prepared in prepared_roots {
                let prepared = canonical_project_root(prepared)?;
                if !workspaces.contains(&prepared) {
                    workspaces.push(prepared);
                }
            }
            if workspaces.len() > crate::write_domains::MAX_ROOTS {
                return Err(invalid("writer workspace list exceeds thirty-two roots"));
            }
            let (guards, roots) = WriterLock::acquire_projects(&root, &workspaces, lock_timeout, cancelled)?;
            let references: Vec<_> = guards.iter().collect();
            Self::open_with_locks_and_workspaces(
                executable,
                &root,
                &references,
                &workspaces,
                &roots,
                timeout.saturating_sub(started.elapsed()),
                cancelled,
            )
        })()
        .map_err(|error| failure(WriterDisposition::NotStarted, error))
    }

    pub(crate) fn open_with_locks(
        executable: &Path,
        root: &Path,
        locks: &[&WriterLock],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        let workspaces = vec![root.to_path_buf()];
        let roots = crate::write_domains::workspace_union(root, &workspaces)?;
        Self::open_with_locks_and_workspaces(executable, root, locks, &workspaces, &roots, timeout, cancelled)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one private handoff binds locks and their workspace graph before ACK"
    )]
    fn open_with_locks_and_workspaces(
        executable: &Path,
        root: &Path,
        locks: &[&WriterLock],
        workspaces: &[PathBuf],
        roots: &[PathBuf],
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Self> {
        (|| {
            let deadline = Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| invalid("writer deadline is too large"))?;
            if cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "writer admission cancelled"));
            }
            if timeout.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "writer admission deadline expired",
                ));
            }
            if !executable.is_absolute()
                || !executable.is_file()
                || canonical_project_root(root)? != root
                || locks.is_empty()
                || locks.len() > MAX_WRITER_LOCK_LOANS
            {
                return Err(invalid(
                    "explicit helper, canonical writer root and lock custody required",
                ));
            }
            for domain in roots {
                let path = project_lock_path(domain)?;
                if !locks.iter().any(|lock| lock.path() == path) {
                    return Err(invalid("writer handoff omits a declared artifact domain"));
                }
            }
            let mut duplicates = Vec::new();
            let mut loans = Vec::new();
            let mut expected = Vec::new();
            for lock in locks {
                let file = lock.file().try_clone()?;
                let metadata = file.metadata()?;
                loans.push(LockLoan {
                    fd: file.as_raw_fd(),
                    path: lock.path().to_path_buf(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                });
                expected.push(json!({"path":lock.path(),"device":metadata.dev(),"inode":metadata.ino()}));
                duplicates.push(file);
            }
            let primary = locks.first().ok_or_else(|| invalid("writer primary lock missing"))?;
            let mut command = Command::new(executable);
            command
                .arg("writer")
                .arg(root)
                .arg(primary.path())
                .arg(timeout.as_secs_f64().to_string())
                .env(LOCK_LOANS_ENV, serde_json::to_string(&loans).map_err(io::Error::other)?)
                .env(
                    WORKSPACES_ENV,
                    serde_json::to_string(workspaces).map_err(io::Error::other)?,
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let fds: Vec<_> = duplicates.iter().map(AsFd::as_fd).collect();
            let (helper, receiver) = Helper::spawn(command, &fds)?;
            let mut client = Self {
                helper,
                receiver,
                owner: Value::Null,
                root: root.to_path_buf(),
                write_roots: roots.to_vec(),
                workspaces: workspaces.to_vec(),
                active: false,
                asset_dir: PathBuf::new(),
                _controller_locks: duplicates,
            };
            let locked = client.receive_until(deadline, cancelled)?;
            let owner = field(&locked, "owner");
            if field(&locked, "state") != "locked"
                || field(owner, "version") != 1
                || field(owner, "pid") != client.helper.id
                || field(owner, "session") != field(owner, "pid")
                || field(owner, "start").as_str().is_none_or(str::is_empty)
                || field(&locked, "lock_loans") != &Value::Array(expected)
            {
                return Err(invalid("writer ownership/lock-custody handshake failed"));
            }
            let asset_dir = field(&locked, "asset_dir")
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| invalid("writer asset custody missing"))?;
            if !asset_dir.is_absolute() || asset_dir.canonicalize()? != asset_dir || !asset_dir.is_dir() {
                return Err(invalid("writer asset custody has no canonical private directory"));
            }
            client.asset_dir = asset_dir;
            client.owner = owner.clone();
            Ok(client)
        })()
        .map_err(|error| failure(WriterDisposition::NotStarted, error))
    }

    /// Canonical project domain covered by this writer, which may initially be prospective.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.root
    }

    /// Canonical artifact roots whose project domains remain held by this helper.
    #[must_use]
    pub fn write_roots(&self) -> &[PathBuf] {
        &self.write_roots
    }

    /// Check a registered workspace's complete current graph against held domains.
    ///
    /// Additional independently built roots must be supplied when opening the
    /// writer, even when they also occur in another workspace's dependency map.
    ///
    /// # Errors
    /// Returns unsupported or unavailable dependency metadata errors.
    pub fn covers_project(&self, root: &Path) -> io::Result<bool> {
        let root = canonical_project_root(root)?;
        if !self.workspaces.contains(&root) {
            return Ok(false);
        }
        Ok(project_artifact_roots(&root)?
            .iter()
            .all(|root| self.write_roots.contains(root)))
    }

    /// Private helper-owned workspace, retained until actual work has stopped.
    ///
    /// Create inputs beneath this directory without a caller-owned cleanup guard.
    /// The helper removes it after confirmed closure; helper failure may leave an orphan.
    #[must_use]
    pub fn asset_dir(&self) -> &Path {
        &self.asset_dir
    }

    fn receive_until(&self, deadline: Instant, cancelled: &(impl Fn() -> bool + ?Sized)) -> io::Result<Value> {
        loop {
            if cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "writer operation cancelled"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "writer reply deadline expired; do not replay",
                ));
            }
            match self.receiver.recv_timeout(remaining.min(Duration::from_millis(20))) {
                Ok(frame) => return frame,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => return Err(invalid(format!("writer reply missing; do not replay: {error}"))),
            }
        }
    }

    /// Run once and retain the helper lock for later stages and metadata writes.
    ///
    /// # Errors
    /// A lost reply seals this lifetime. The helper retains its owned cleanup scope after cancellation.
    pub fn run(&mut self, command: &Command, timeout: Duration) -> io::Result<Output> {
        self.run_cancellable(command, timeout, &|| false)
    }

    /// Run once, observing cancellation and retaining unknown work without replay.
    ///
    /// One absolute deadline covers the command and bounded cleanup slack. The
    /// helper remains the cleanup owner after an error; this scope stays sealed.
    ///
    /// # Errors
    /// Returns cancellation, protocol, command, or missing terminal evidence errors.
    pub fn run_cancellable(
        &mut self,
        command: &Command,
        timeout: Duration,
        cancelled: &(impl Fn() -> bool + ?Sized),
    ) -> io::Result<Output> {
        use std::os::unix::process::ExitStatusExt;
        if self.active {
            return Err(failure(
                WriterDisposition::Unknown,
                invalid("writer scope is sealed; no replay"),
            ));
        }
        let prepared = (|| -> io::Result<_> {
            if cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "writer cancelled before GO"));
            }
            let command_deadline = Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| invalid("writer deadline is too large"))?;
            let reply_deadline = command_deadline
                .checked_add(Duration::from_secs(5))
                .ok_or_else(|| invalid("writer deadline is too large"))?;
            if crate::write_domains::workspace_union(&self.root, &self.workspaces)?
                .iter()
                .any(|root| !self.write_roots.contains(root))
            {
                return Err(invalid("writer dependency graph expanded after admission"));
            }
            let mut args = vec![text(command.get_program())?.to_owned()];
            for arg in command.get_args() {
                args.push(text(arg)?.to_owned());
            }
            let mut env = serde_json::Map::new();
            for (name, value) in command.get_envs() {
                env.insert(
                    text(name)?.to_owned(),
                    match value {
                        Some(value) => json!(text(value)?),
                        None => Value::Null,
                    },
                );
            }
            let cwd = command.get_current_dir().unwrap_or(&self.root);
            Ok((
                command_deadline,
                reply_deadline,
                json!({"action":"run","command":args,"cwd":cwd,"env":env,"timeout":timeout.as_secs_f64()}),
            ))
        })()
        .map_err(|error| failure(WriterDisposition::NotStarted, error))?;
        let (command_deadline, reply_deadline, request) = prepared;
        self.active = true;
        let delivery = self
            .helper
            .input
            .as_mut()
            .ok_or_else(|| invalid("writer stdin missing"))
            .and_then(|input| send_until(input, &request, command_deadline, cancelled));
        let mut reason = delivery.err().map(|error| error.to_string());
        if reason.is_some() {
            self.cancel();
        }
        let mut process: Option<Value> = None;
        loop {
            if cancelled() && reason.is_none() {
                reason = Some("writer operation cancelled".to_owned());
                self.cancel();
            }
            let remaining = reply_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failure(
                    WriterDisposition::Unknown,
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        reason.unwrap_or_else(|| "writer closure receipt deadline expired".to_owned()),
                    ),
                ));
            }
            let frame = match self.receiver.recv_timeout(remaining.min(Duration::from_millis(20))) {
                Ok(Ok(frame)) => frame,
                Ok(Err(error)) => {
                    self.cancel();
                    return Err(failure(WriterDisposition::Unknown, error));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(error) => {
                    self.cancel();
                    return Err(failure(
                        WriterDisposition::Unknown,
                        invalid(format!("writer reply missing: {error}")),
                    ));
                }
            };
            if field(&frame, "owner") != &self.owner {
                self.cancel();
                return Err(failure(
                    WriterDisposition::Unknown,
                    invalid("writer owner receipt changed"),
                ));
            }
            match field(&frame, "state").as_str() {
                Some("started") if process.is_none() => {
                    let identity = field(&frame, "process");
                    if !valid_process(identity, &self.owner) {
                        self.cancel();
                        return Err(failure(
                            WriterDisposition::Unknown,
                            invalid("writer child receipt invalid"),
                        ));
                    }
                    process = Some(identity.clone());
                }
                Some("still_running" | "unknown") => {
                    if reason.is_none() {
                        reason = Some(
                            field(&frame, "reason")
                                .as_str()
                                .unwrap_or("writer result unavailable")
                                .to_owned(),
                        );
                    }
                    self.cancel();
                }
                Some("stopped") => {
                    // H can close a setup failure before sending Started. One
                    // run is outstanding, and this already-bound owner is its
                    // sole writer; accept a fully valid first terminal identity.
                    if process.is_none() && valid_process(field(&frame, "process"), &self.owner) {
                        process = Some(field(&frame, "process").clone());
                    }
                    if process.as_ref() != Some(field(&frame, "process")) || field(&frame, "owned_work_stopped") != true
                    {
                        self.cancel();
                        return Err(failure(
                            WriterDisposition::Unknown,
                            invalid("writer scope closure receipt invalid"),
                        ));
                    }
                    // Stop evidence remains valid independently of result
                    // compatibility. Old or partial receipts cannot attest that
                    // the command completed without cancellation/intervention.
                    if reason.is_some()
                        || field(&frame, "cancelled") != false
                        || field(&frame, "intervened") != false
                        || field(&frame, "result") != "result"
                    {
                        return Err(stopped_failure(
                            &io::Error::new(
                                io::ErrorKind::Interrupted,
                                reason.unwrap_or_else(|| "writer stopped without a usable result".to_owned()),
                            ),
                            &frame,
                        ));
                    }
                    let output = (|| -> io::Result<Output> {
                        let code = field(&frame, "exit_code")
                            .as_i64()
                            .and_then(|value| i32::try_from(value).ok())
                            .filter(|value| (-127..=255).contains(value))
                            .ok_or_else(|| invalid("writer exit code missing"))?;
                        let raw = if code < 0 {
                            code.checked_neg()
                        } else {
                            code.checked_shl(8)
                        }
                        .ok_or_else(|| invalid("writer exit code overflow"))?;
                        Ok(Output {
                            status: std::process::ExitStatus::from_raw(raw),
                            stdout: field(&frame, "stdout")
                                .as_str()
                                .ok_or_else(|| invalid("writer stdout missing"))?
                                .as_bytes()
                                .to_vec(),
                            stderr: field(&frame, "stderr")
                                .as_str()
                                .ok_or_else(|| invalid("writer stderr missing"))?
                                .as_bytes()
                                .to_vec(),
                        })
                    })()
                    .map_err(|error| stopped_failure(&error, &frame))?;
                    self.active = false;
                    return Ok(output);
                }
                _ => {
                    self.cancel();
                    return Err(failure(
                        WriterDisposition::Unknown,
                        invalid("writer lifecycle receipt invalid"),
                    ));
                }
            }
        }
    }

    fn cancel(&mut self) {
        // A partial frame cannot safely be followed by a cancel frame. EOF is
        // nonblocking and seals delivery while H retains actual cleanup custody.
        drop(self.helper.input.take());
    }
}

/// Run one command when no caller-owned shared metadata follows it.
///
/// # Errors
/// Returns admission, protocol, or missing terminal-evidence errors.
#[cfg(unix)]
pub fn run_shared_writer(executable: &Path, root: &Path, command: &Command, timeout: Duration) -> io::Result<Output> {
    SharedWriter::open(executable, root, timeout)?.run(command, timeout)
}
