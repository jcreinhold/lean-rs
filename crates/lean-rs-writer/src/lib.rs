//! Process-bound coordination for actual shared writes, independent of proof results.
//!
//! Each actual command starts as a blocked exec wrapper. Its process group is
//! synced under the stable flock before the command can run. Trusted work must
//! retain that private group; regrouping or starting a new session is outside
//! this boundary. Inherited descriptors add a passive resource fence.
//!
//! On macOS the actual parent pins the unreaped leader while stopping and
//! atomically observing its group. Deadline, cancellation, client loss and
//! post-launch errors request cleanup and retain all resources until closure.
//! Intervention always reports no result, even when the leader exited zero.
//! The Linux backend is weaker: after leader reap it only observes group absence
//! and cannot safely signal a surviving or late-forked descendant.

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, IoSlice, Read, Seek, Write};
use std::mem::MaybeUninit;
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use lean_toolchain::native_guardian::{OWNERS_DIRECTORY_ENV, registry_directory};
use lean_toolchain::process_group::OwnedCommandGroup;
use lean_toolchain::writer_demand::{ReadAdmission, WriterDemand};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
use rustix::pipe::pipe;
use rustix::process::{Pid, getpgrp, getsid, setsid};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod log;

/// Version of the shared-writer CLI and ownership record.
pub const PROTOCOL: u32 = 1;
const FRAME_BYTES: u64 = 1024 * 1024;
const POLL: Duration = Duration::from_millis(25);

/// The actual helper executable and the semantic protocol it implements.
#[derive(Clone, Debug, Serialize)]
pub struct WriterContract {
    pub protocol: u32,
    pub executable: PathBuf,
    /// Shared native scope namespace that every managed writer must inspect.
    pub ownership_registry: PathBuf,
}

/// The helper executable set by [`use_helper_executable`].
static HELPER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Name the executable that serves the [`HelperCommand`] subcommands, for a
/// process whose own executable does not, such as a test harness. Call it once
/// before native work; [`contract`] then reports `path`.
///
/// # Errors
/// Returns an error when `path` cannot be resolved, or when an earlier call
/// named another file.
pub fn use_helper_executable(path: &Path) -> io::Result<()> {
    let path = path.canonicalize()?;
    if *HELPER.get_or_init(|| path.clone()) == path {
        Ok(())
    } else {
        Err(invalid("the helper executable is already set to another file"))
    }
}

/// Report the helper executable, without a path-specific compatibility pin.
///
/// That is the path given to [`use_helper_executable`], else this process's
/// own executable.
///
/// # Errors
/// Returns the operating-system error when the executable cannot be identified.
pub fn contract() -> io::Result<WriterContract> {
    let executable = match HELPER.get() {
        Some(path) => path.clone(),
        None => std::env::current_exe()?.canonicalize()?,
    };
    Ok(WriterContract {
        protocol: PROTOCOL,
        executable,
        ownership_registry: lean_toolchain::native_guardian::registry_directory()?,
    })
}

/// The helper subcommands.
///
/// A host binary that serves them itself flattens this enum into its own
/// subcommands and calls [`HelperCommand::run`]. If the host keeps its owner
/// directory somewhere other than the environment or the SDK default, it calls
/// `lean_toolchain::native_guardian::bind_registry_directory` first.
///
/// ```no_run
/// use clap::Parser as _;
///
/// #[derive(clap::Parser)]
/// enum Command {
///     /// The host's own work.
///     Serve,
///     #[command(flatten)]
///     Helper(lean_rs_writer::HelperCommand),
/// }
///
/// fn main() -> std::process::ExitCode {
///     match Command::parse() {
///         Command::Serve => std::process::ExitCode::SUCCESS,
///         Command::Helper(helper) => helper.run("my-host"),
///     }
/// }
/// ```
#[derive(Debug, clap::Subcommand)]
pub enum HelperCommand {
    /// Hold the project write locks and run commands sent on stdin.
    Writer(WriterArgs),
    /// Print the project lock path without taking it.
    WriterLockPath(WriterLockPathArgs),
    /// Pass shared read custody to a worker over a socket.
    Reader(ReaderArgs),
    /// Hold shared read custody for a parent that keeps no lock descriptor.
    ReadProxy(ReadProxyArgs),
    /// Own one native worker generation.
    #[command(hide = true)]
    NativeGuardian,
    /// Wrap one writer command until its process group is recorded.
    #[command(hide = true)]
    WriterExec,
}

impl HelperCommand {
    /// Run the subcommand. On failure, print `{program} {subcommand}: {error}`
    /// to stderr and return the subcommand's failure status.
    pub fn run(&self, program: &str) -> std::process::ExitCode {
        let (name, result) = match self {
            Self::Writer(args) => ("writer", run_cli(args)),
            Self::WriterLockPath(args) => ("writer-lock-path", run_lock_path_cli(args)),
            Self::Reader(args) => ("reader", run_reader(args)),
            Self::ReadProxy(args) => ("read-proxy", run_read_proxy(args)),
            Self::NativeGuardian => ("native-guardian", lean_toolchain::native_guardian::run_guardian_cli()),
            Self::WriterExec => ("writer-exec", run_exec()),
        };
        let Err(error) = result else {
            return std::process::ExitCode::SUCCESS;
        };
        eprintln!("{program} {name}: {error}");
        match self {
            Self::Reader(_) | Self::ReadProxy(_) => std::process::ExitCode::from(reader_failure_exit_code(&error)),
            Self::Writer(_) | Self::WriterLockPath(_) | Self::NativeGuardian | Self::WriterExec => {
                std::process::ExitCode::FAILURE
            }
        }
    }
}

/// Read-only access to the shared Rust project-lock resolver.
#[derive(Debug, clap::Args)]
pub struct WriterLockPathArgs {
    root: PathBuf,
}

/// Emit one bounded project-lock descriptor without acquiring or creating a lock.
///
/// This command only resolves an existing directory. It does not open a worker,
/// evaluate Lake configuration, or authorize a later command.
///
/// # Errors
/// Returns filesystem, serialization, size-limit, or stdout errors.
pub fn run_lock_path_cli(args: &WriterLockPathArgs) -> io::Result<()> {
    let root = args.root.canonicalize()?;
    if !root.is_dir() {
        return Err(invalid("existing project directory required"));
    }
    let lock_path = lean_toolchain::shared_writer::project_lock_path(&root)?;
    let descriptor = json!({"protocol":PROTOCOL,"project_root":root,"lock_path":lock_path});
    let mut bytes = serde_json::to_vec(&descriptor).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > FRAME_BYTES {
        return Err(invalid("writer lock descriptor exceeds protocol limit"));
    }
    let mut output = io::stdout().lock();
    output.write_all(&bytes)?;
    output.flush()
}

/// Internal CLI entry: one supervisor lifetime can execute adjacent writer stages.
#[derive(Debug, clap::Args)]
pub struct WriterArgs {
    root: PathBuf,
    lock: PathBuf,
    admission: f64,
}

/// Internal one-shot transfer of shared read custody to an actual worker.
#[derive(Debug, clap::Args)]
pub struct ReaderArgs {
    /// Return normal exit 75 for typed pre-transfer contention, without custody.
    #[arg(long)]
    report_contention: bool,
    root: PathBuf,
    lock: PathBuf,
    admission: f64,
    socket: PathBuf,
    nonce: String,
}

/// Internal read custody for parent-only cache work, without a parent lock FD.
#[derive(Debug, clap::Args)]
pub struct ReadProxyArgs {
    /// Wait on one metadata domain; caller must hold no other artifact custody.
    #[arg(long, conflicts_with = "report_contention")]
    single_domain_wait: bool,
    /// Report typed pre-Ready contention to a whole-union acquisition owner.
    #[arg(long, conflicts_with = "single_domain_wait")]
    report_contention: bool,
    root: PathBuf,
    lock: PathBuf,
    admission: f64,
    nonce: String,
    fifo: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    version: u32,
    pid: u32,
    start: String,
    session: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessIdentity {
    pid: u32,
    process_group: u32,
    session: u32,
    start: String,
    start_new_session: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    owner: Owner,
    // The writer always publishes a disposition, including explicit null
    // for Idle. A missing field cannot certify command closure.
    #[serde(deserialize_with = "Option::deserialize")]
    process: Option<ProcessIdentity>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn duration(seconds: f64) -> io::Result<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(invalid("positive finite writer timeout required"));
    }
    Duration::try_from_secs_f64(seconds).map_err(|error| invalid(error.to_string()))
}

fn sid(value: Option<Pid>) -> io::Result<u32> {
    u32::try_from(getsid(value)?.as_raw_pid()).map_err(|error| invalid(error.to_string()))
}

#[cfg(target_os = "macos")]
fn birth(value: u32) -> io::Result<String> {
    let raw = i32::try_from(value).map_err(|error| invalid(error.to_string()))?;
    let info = libproc::proc_pid::pidinfo::<libproc::bsd_info::BSDInfo>(raw, 0)
        .map_err(|error| io::Error::new(io::ErrorKind::Interrupted, error))?;
    if info.pbi_pid != value {
        return Err(invalid("writer process identity changed"));
    }
    Ok(format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(target_os = "linux")]
fn birth(value: u32) -> io::Result<String> {
    let stat = fs::read_to_string(format!("/proc/{value}/stat"))?;
    let fields = stat
        .rsplit_once(')')
        .ok_or_else(|| invalid("malformed process stat"))?
        .1;
    let start = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| invalid("process birth missing"))?;
    Ok(format!(
        "{}:{}",
        fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim(),
        start
    ))
}

#[cfg(test)]
fn group_live_with(
    process: &ProcessIdentity,
    observe_birth: impl FnOnce(u32) -> io::Result<String>,
    observe_group: impl FnOnce(u32) -> io::Result<bool>,
) -> io::Result<bool> {
    // A live group reserves its leader's numeric PID even after leader exit.
    // A different kernel birth at that PID therefore proves the old group ended.
    // Missing/denied leader birth proves nothing: the old group may still have
    // descendants, so retain the kernel group-existence check.
    if observe_birth(process.pid).is_ok_and(|actual| actual != process.start) {
        return Ok(false);
    }
    observe_group(process.process_group)
}

fn persist(file: &mut File, record: &Record) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    bytes.push(b'\n');
    file.rewind()?;
    file.set_len(0)?;
    file.write_all(&bytes)?;
    file.sync_all()
}

fn persist_all(file: &mut File, loans: &[File], record: &Record) -> io::Result<()> {
    persist(file, record)?;
    for loan in loans {
        // This is another handle to the same description, never a fresh lock.
        // Every domain is durable before GO and cleared only after closure.
        persist(&mut loan.try_clone()?, record)?;
    }
    Ok(())
}

fn owner(file: &mut File) -> io::Result<Option<Record>> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(invalid("writer owner record is oversized"));
    }
    if bytes.is_empty() || bytes.last() != Some(&b'\n') {
        // The complete record is synced before spawn. A partial record therefore
        // precedes any command; never launch after a failed metadata commit.
        return Ok(None);
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    let identity = &record.owner;
    if record.version != PROTOCOL
        || identity.version != PROTOCOL
        || identity.pid == 0
        || identity.session != identity.pid
        || identity.start.is_empty()
    {
        return Err(invalid("writer owner has no exact session identity"));
    }
    if let Some(process) = &record.process
        && (process.pid == 0
            || process.process_group != process.pid
            || process.session != identity.session
            || process.start.is_empty()
            || process.start_new_session)
    {
        return Err(invalid("writer stage has no exact process group identity"));
    }
    Ok(Some(record))
}

fn canonical(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.canonicalize()? != path {
        return Err(invalid("canonical absolute writer path required"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ReaderDemandPolicy {
    FailFast,
    WaitSingleDomain,
    ReportContention,
}

#[derive(Debug)]
struct ReaderContention(&'static str);
impl std::fmt::Display for ReaderContention {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "reader admission contended before custody: {}", self.0)
    }
}
impl std::error::Error for ReaderContention {}

fn reader_contention(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, ReaderContention(reason))
}

/// A flagged pure reader can report contention only before transferring custody.
/// Other I/O errors and every legacy unflagged reader keep ordinary failure status.
#[must_use]
pub fn reader_failure_exit_code(error: &io::Error) -> u8 {
    if error.get_ref().is_some_and(|cause| cause.is::<ReaderContention>()) {
        75
    } else {
        1
    }
}

fn acquire_while(
    path: &Path,
    timeout: Duration,
    shared: bool,
    caller_alive: impl FnMut() -> io::Result<bool>,
) -> io::Result<(File, Option<Record>)> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| invalid("writer timeout is too large"))?;
    acquire_while_until(path, deadline, shared, ReaderDemandPolicy::FailFast, caller_alive)
}

fn acquire_while_until(
    path: &Path,
    deadline: Instant,
    shared: bool,
    demand_policy: ReaderDemandPolicy,
    mut caller_alive: impl FnMut() -> io::Result<bool>,
) -> io::Result<(File, Option<Record>)> {
    canonical(path.parent().ok_or_else(|| invalid("writer lock has no parent"))?)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(invalid("writer lock aliases are unsupported"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    loop {
        if !caller_alive()? {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "read proxy caller closed before admission",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "artifact admission deadline elapsed",
            ));
        }
        // Generic readers can already hold earlier SH domains and must refuse
        // demand before their body. Only the explicit single-domain metadata
        // proxy may wait, with no other artifact custody and no target replay.
        let admission = if shared {
            match ReadAdmission::try_acquire(&[path.to_path_buf()]) {
                Ok(Some(admission)) => Some(admission),
                Ok(None) => match demand_policy {
                    ReaderDemandPolicy::FailFast => {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "artifact writer demand prevents new read admission",
                        ));
                    }
                    ReaderDemandPolicy::ReportContention => return Err(reader_contention("writer_demand")),
                    ReaderDemandPolicy::WaitSingleDomain => {
                        // try_acquire closed every transient gate. Retry only
                        // admission under the original deadline and FIFO owner.
                        thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
                        continue;
                    }
                },
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if matches!(demand_policy, ReaderDemandPolicy::ReportContention) {
                        return Err(reader_contention("reader_gate"));
                    }
                    // Another reader holds only a transient gate, never an
                    // artifact wait. All partial gates were closed by try_acquire.
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "reader admission gate is busy"));
                    }
                    thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
                    continue;
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let attempt = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match attempt {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) if matches!(demand_policy, ReaderDemandPolicy::ReportContention) => {
                return Err(reader_contention("artifact_lock"));
            }
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                drop(admission);
                thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
                continue;
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "shared resource lock is busy"));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
        let opened = file.metadata()?;
        let named = fs::symlink_metadata(path)?;
        if !opened.is_file()
            || opened.nlink() != 1
            || !named.is_file()
            || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
        {
            return Err(invalid("writer lock inode or canonical identity changed"));
        }
        let previous = owner(&mut file)?;
        if !lean_toolchain::shared_writer::previous_writer_stopped(&file)?
            || (!shared
                && !lean_toolchain::native_guardian::protected_native_reads_stopped(
                    &lean_toolchain::native_guardian::registry_directory()?,
                    path,
                    (opened.dev(), opened.ino()),
                )?)
        {
            if matches!(demand_policy, ReaderDemandPolicy::ReportContention) {
                return Err(reader_contention("previous_writer_scope"));
            }
            file.unlock()?;
            drop(admission);
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "previous writer process group remains live",
                ));
            }
            thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
            continue;
        }
        return Ok((file, previous));
    }
}

fn workspace_domains(workspaces: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    lean_toolchain::shared_writer::project_artifact_root_union(workspaces)?
        .into_iter()
        .map(|root| lean_toolchain::shared_writer::project_lock_path(&root))
        .collect()
}

fn validate_workspace_custody(workspaces: &[PathBuf], locks: &[(PathBuf, File)]) -> io::Result<()> {
    for path in workspace_domains(workspaces)? {
        if !locks.iter().any(|(held, _)| held == &path) {
            return Err(invalid("writer custody omits a declared dependency domain"));
        }
    }
    for (path, file) in locks {
        let opened = file.metadata()?;
        let named = fs::symlink_metadata(path)?;
        if !named.is_file() || opened.nlink() != 1 || (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
            return Err(invalid("writer domain inode changed after admission"));
        }
        if !lean_toolchain::native_guardian::protected_native_reads_stopped(
            &lean_toolchain::native_guardian::registry_directory()?,
            path,
            (opened.dev(), opened.ino()),
        )? {
            return Err(invalid("native artifact readers remain unresolved"));
        }
    }
    Ok(())
}

fn lock_workspaces(
    workspaces: &[PathBuf],
    primary: &Path,
    timeout: Duration,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> io::Result<(Vec<(PathBuf, File)>, WriterDemand)> {
    let started = Instant::now();
    loop {
        let mut paths = workspace_domains(workspaces)?;
        paths.sort();
        paths.dedup();
        // Announce the complete union before taking any EX. Compatible shared
        // demand guards let overlapping writers retain one sorted EX order.
        let demand = WriterDemand::announce(&paths, timeout.saturating_sub(started.elapsed()), cancelled)?;
        let mut locks = Vec::new();
        for path in &paths {
            let (file, _) = acquire_while(path, timeout.saturating_sub(started.elapsed()), false, || {
                Ok(!cancelled())
            })?;
            locks.push((path.clone(), file));
        }
        let mut rechecked = workspace_domains(workspaces)?;
        rechecked.sort();
        rechecked.dedup();
        if paths == rechecked {
            locks.sort_by_key(|(path, _)| path != primary);
            validate_workspace_custody(workspaces, &locks)?;
            return Ok((locks, demand));
        }
        drop(locks);
        drop(demand);
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "writer dependency graph changed during admission",
            ));
        }
    }
}

fn reader_lease(
    root: &Path,
    path: &Path,
    deadline: Instant,
    nonce: &str,
    report_contention: bool,
) -> io::Result<(File, Value)> {
    canonical(root)?;
    if path != lean_toolchain::shared_writer::project_lock_path(root)? {
        return Err(invalid("reader lock differs from canonical project domain"));
    }
    if nonce.is_empty() || nonce.len() > 256 {
        return Err(invalid("bounded reader nonce required"));
    }
    let policy = if report_contention {
        ReaderDemandPolicy::ReportContention
    } else {
        ReaderDemandPolicy::FailFast
    };
    let (file, previous) = acquire_while_until(path, deadline, true, policy, || Ok(true))?;
    let epoch = serde_json::to_string(&previous.as_ref().map(|record| &record.owner)).map_err(io::Error::other)?;
    let metadata = file.metadata()?;
    let receipt = json!({"protocol":PROTOCOL,"state":"read_ready","project_root":root,
        "nonce":nonce,"epoch":epoch,"lock_identity":{"device":metadata.dev(),"inode":metadata.ino()}});
    Ok((file, receipt))
}

fn fifo_closed(file: &mut File) -> io::Result<bool> {
    let mut bytes = [0_u8; 1024];
    match file.read(&mut bytes) {
        Ok(0) => Ok(true),
        Ok(_) => Ok(false),
        Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Hold a parent-cache read until its atomically CLOEXEC FIFO reaches EOF.
///
/// The parent receives metadata only. Its `O_CLOEXEC` FIFO control descriptor
/// cannot leak through a concurrent broker spawn, unlike a pipe-plus-fcntl
/// creation window. This does not replace actual native-worker custody.
///
/// # Errors
/// Returns admission, private FIFO, or metadata delivery errors.
pub fn run_read_proxy(args: &ReadProxyArgs) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(duration(args.admission)?)
        .ok_or_else(|| invalid("reader proxy timeout is too large"))?;
    canonical(&args.root)?;
    if args.lock != lean_toolchain::shared_writer::project_lock_path(&args.root)? {
        return Err(invalid("reader proxy lock differs from canonical project domain"));
    }
    let directory = args.fifo.parent().ok_or_else(|| invalid("reader FIFO has no parent"))?;
    canonical(directory)?;
    let parent = fs::metadata(directory)?;
    let named = fs::symlink_metadata(&args.fifo)?;
    if !parent.is_dir()
        || parent.uid() != rustix::process::getuid().as_raw()
        || parent.mode() & 0o077 != 0
        || !named.file_type().is_fifo()
        || named.uid() != parent.uid()
        || named.mode() & 0o077 != 0
    {
        return Err(invalid("read proxy requires its own private FIFO"));
    }
    let mut control = OpenOptions::new()
        .read(true)
        .custom_flags(i32::try_from(OFlags::NONBLOCK.bits()).map_err(io::Error::other)?)
        .open(&args.fifo)?;
    let opened = control.metadata()?;
    if !opened.file_type().is_fifo() || (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
        return Err(invalid("read proxy FIFO identity changed"));
    }
    if args.nonce.is_empty() || args.nonce.len() > 256 {
        return Err(invalid("bounded reader nonce required"));
    }
    let demand_policy = if args.single_domain_wait {
        ReaderDemandPolicy::WaitSingleDomain
    } else if args.report_contention {
        ReaderDemandPolicy::ReportContention
    } else {
        ReaderDemandPolicy::FailFast
    };
    let (file, previous) = acquire_while_until(&args.lock, deadline, true, demand_policy, || {
        fifo_closed(&mut control).map(|closed| !closed)
    })?;
    let epoch = serde_json::to_string(&previous.as_ref().map(|record| &record.owner)).map_err(io::Error::other)?;
    let identity = file.metadata()?;
    if fifo_closed(&mut control)? {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "read proxy caller closed before readiness",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "reader proxy admission deadline elapsed",
        ));
    }
    send(
        &json!({"protocol":PROTOCOL,"state":"read_ready","project_root":args.root,
        "nonce":args.nonce,"epoch":epoch,"custody":"proxy",
        "lock_identity":{"device":identity.dev(),"inode":identity.ino()}}),
    )?;
    loop {
        match fifo_closed(&mut control) {
            Ok(true) => return Ok(()),
            // A failed observation is never an unlock instruction.
            Ok(false) | Err(_) => thread::sleep(POLL),
        }
    }
}

/// Transfer the shared lock's open file description; only closing the final
/// transferred/inherited descriptor releases custody. Never explicitly unlock.
///
/// # Errors
/// Returns admission, stale owned-work, socket, or descriptor-transfer errors.
pub fn run_reader(args: &ReaderArgs) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(duration(args.admission)?)
        .ok_or_else(|| invalid("reader timeout is too large"))?;
    canonical(
        args.socket
            .parent()
            .ok_or_else(|| invalid("reader socket has no parent"))?,
    )?;
    let directory = fs::metadata(
        args.socket
            .parent()
            .ok_or_else(|| invalid("reader socket has no parent"))?,
    )?;
    if !directory.is_dir() || directory.uid() != rustix::process::getuid().as_raw() || directory.mode() & 0o077 != 0 {
        return Err(invalid("reader transfer requires its own private socket directory"));
    }
    let (file, receipt) = reader_lease(&args.root, &args.lock, deadline, &args.nonce, args.report_contention)?;
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "reader transfer deadline elapsed",
        ));
    }
    let mut payload = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
    payload.push(b'\n');
    let mut stream = UnixStream::connect(&args.socket)?;
    stream.set_write_timeout(Some(timeout))?;
    let descriptors = [file.as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(SendAncillaryMessage::ScmRights(&descriptors)) {
        return Err(invalid("reader descriptor message is too large"));
    }
    let count = sendmsg(&stream, &[IoSlice::new(&payload)], &mut ancillary, SendFlags::empty())?;
    if count == 0 {
        return Err(invalid("reader descriptor transfer was not delivered"));
    }
    stream.write_all(
        payload
            .get(count..)
            .ok_or_else(|| invalid("reader transfer count is invalid"))?,
    )?;
    // File Drop closes only this alias. The actual worker (or queued Unix
    // message) already owns the same locked open file description.
    Ok(())
}

const REPLY_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

struct ReplyFrame(Vec<u8>);

impl Write for ReplyFrame {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Reserve the final newline without allocating an oversized JSON frame.
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|total| total >= 1_048_576)
        {
            return Err(invalid("writer reply exceeds the control frame limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn send(value: &Value) -> io::Result<()> {
    let mut frame = ReplyFrame(Vec::new());
    serde_json::to_writer(&mut frame, value).map_err(io::Error::other)?;
    frame.0.push(b'\n');
    // This entry runs in the dedicated CLI helper, before any MCP runtime.
    // No other thread writes stdout. A peer that stops reading must not retain
    // confirmed-closed resources forever behind a blocked reply.
    let output = io::stdout();
    fcntl_setfl(&output, fcntl_getfl(&output)? | OFlags::NONBLOCK)?;
    let deadline = Instant::now()
        .checked_add(REPLY_WRITE_TIMEOUT)
        .ok_or_else(|| invalid("writer reply timeout invalid"))?;
    let mut remaining = frame.0.as_slice();
    while !remaining.is_empty() {
        let budget = deadline.saturating_duration_since(Instant::now());
        if budget.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "writer reply delivery timed out",
            ));
        }
        match rustix::io::write(&output, remaining) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "writer reply accepted no bytes",
                ));
            }
            Ok(count) => {
                remaining = remaining
                    .get(count..)
                    .ok_or_else(|| invalid("writer reply write count invalid"))?;
            }
            Err(error) if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::INTR => {
                thread::sleep(POLL.min(budget));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn control() -> Receiver<io::Result<Value>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let mut bytes = Vec::new();
            let read = input.by_ref().take(FRAME_BYTES + 1).read_until(b'\n', &mut bytes);
            let frame = match read {
                Ok(0) => return,
                Ok(_) if bytes.len() as u64 > FRAME_BYTES || bytes.last() != Some(&b'\n') => {
                    Err(invalid("writer control frame is incomplete or oversized"))
                }
                Ok(_) => serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string())),
                Err(error) => Err(error),
            };
            if sender.send(frame).is_err() {
                return;
            }
        }
    });
    receiver
}

fn code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| status.signal().unwrap_or(1).saturating_neg())
}

fn normalize_owner_directory(message: &mut Value, registry: &Path) -> io::Result<()> {
    if let Some(value) = message
        .get_mut("env")
        .and_then(Value::as_object_mut)
        .and_then(|env| env.remove(OWNERS_DIRECTORY_ENV))
        && value.as_str().map(OsStr::new) != Some(registry.as_os_str())
    {
        return Err(invalid(format!(
            "{OWNERS_DIRECTORY_ENV} must match the bound native owner directory"
        )));
    }
    Ok(())
}

/// Execute only after the supervisor has synced actual group ownership.
///
/// # Errors
/// Returns protocol or exec errors without running an uncommitted command.
pub fn run_exec() -> io::Result<()> {
    let mut bytes = Vec::new();
    io::stdin().lock().take(FRAME_BYTES + 1).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(invalid("writer launch handshake missing or oversized"));
    }
    let mut message: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let lock_path = Path::new(
        field(&message, "lock")
            .as_str()
            .ok_or_else(|| invalid("writer lock binding missing"))?,
    );
    let mut check = OpenOptions::new().read(true).write(true).open(lock_path)?;
    let committed = owner(&mut check)?.ok_or_else(|| invalid("writer launch ownership is not committed"))?;
    let process = committed
        .process
        .as_ref()
        .ok_or_else(|| invalid("writer launch has no actual group"))?;
    if process.pid != std::process::id()
        || process.process_group != u32::try_from(getpgrp().as_raw_pid()).map_err(io::Error::other)?
        || process.session != sid(None)?
        || process.start != birth(std::process::id())?
    {
        return Err(invalid("writer launch ownership differs from actual wrapper"));
    }
    match check.try_lock() {
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(error)) => return Err(error),
        Ok(()) => {
            check.unlock()?;
            return Err(invalid("writer launch has no inherited lock custody"));
        }
    }
    let registry = registry_directory()?;
    normalize_owner_directory(&mut message, &registry)?;
    let arguments: Vec<String> =
        serde_json::from_value(field(&message, "command").clone()).map_err(io::Error::other)?;
    let (program, arguments) = arguments
        .split_first()
        .ok_or_else(|| invalid("writer command missing"))?;
    let root = field(&message, "root")
        .as_str()
        .ok_or_else(|| invalid("writer root missing"))?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(root)
        .env(OWNERS_DIRECTORY_ENV, registry)
        .stdin(Stdio::null());
    if let Some(cwd) = message.get("cwd").and_then(Value::as_str) {
        command.current_dir(cwd);
    }
    if let Some(environment) = message.get("env").and_then(Value::as_object) {
        for (key, value) in environment {
            if value.is_null() {
                command.env_remove(key);
            } else {
                command.env(
                    key,
                    value
                        .as_str()
                        .ok_or_else(|| invalid("string writer environment required"))?,
                );
            }
        }
    }
    Err(command.exec())
}

fn barrier_closed(barrier: &rustix::fd::OwnedFd) -> io::Result<bool> {
    let mut byte = [0_u8; 1];
    match rustix::io::read(barrier, &mut byte) {
        Ok(0) => Ok(true),
        Ok(_) => Err(invalid("unexpected writer ownership barrier data")),
        Err(error) if error == rustix::io::Errno::AGAIN => Ok(false),
        Err(error) => Err(error.into()),
    }
}

// Two streams at this limit still fit the 1 MiB control frame even when
// every byte needs JSON's longest six-byte escape. A run with a log sends its
// output to the log instead.
const CAPTURE_BYTES: usize = 64 * 1024;

struct Capture {
    descriptor: rustix::fd::OwnedFd,
    bytes: Vec<u8>,
    overflowed: bool,
}

/// A pipe whose ends close on exec, with a nonblocking read end.
fn output_pipe() -> io::Result<(rustix::fd::OwnedFd, rustix::fd::OwnedFd)> {
    let (read, write) = pipe()?;
    fcntl_setfd(&read, FdFlags::CLOEXEC)?;
    fcntl_setfd(&write, FdFlags::CLOEXEC)?;
    fcntl_setfl(&read, fcntl_getfl(&read)? | OFlags::NONBLOCK)?;
    Ok((read, write))
}

impl Capture {
    fn pipe() -> io::Result<(Self, Stdio)> {
        let (read, write) = output_pipe()?;
        Ok((
            Self {
                descriptor: read,
                bytes: Vec::new(),
                overflowed: false,
            },
            Stdio::from(write),
        ))
    }

    fn read_available(&mut self) -> io::Result<bool> {
        let mut buffer = [0_u8; 4096];
        // Bound each poll even under continuous output or interrupt delivery.
        // After overflow, keep draining without retaining more bytes while the
        // existing owner requests stop and observes closure.
        for _ in 0..32 {
            match rustix::io::read(&self.descriptor, &mut buffer) {
                Ok(0) => return Ok(self.overflowed),
                Ok(count) => {
                    let keep = count.min(CAPTURE_BYTES.saturating_sub(self.bytes.len()));
                    self.bytes.extend_from_slice(
                        buffer
                            .get(..keep)
                            .ok_or_else(|| invalid("capture read count invalid"))?,
                    );
                    if keep < count && !self.overflowed {
                        self.overflowed = true;
                        // Report the first overflow before another read can fail.
                        // Later polls still drain and discard within the budget.
                        return Ok(true);
                    }
                }
                Err(error) if error == rustix::io::Errno::AGAIN => return Ok(self.overflowed),
                Err(error) if error == rustix::io::Errno::INTR => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(self.overflowed)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// Where a stage's stdout and stderr go: two captures that the stopped frame
/// carries, or one pipe into the caller's log.
enum StageOutput {
    Captured(Capture, Capture),
    Logged(log::LogSink),
}

impl StageOutput {
    /// The helper's side and the stage's stdout and stderr.
    fn open(log: Option<log::CappedLog>) -> io::Result<(Self, Stdio, Stdio)> {
        let Some(log) = log else {
            let (stdout, child_stdout) = Capture::pipe()?;
            let (stderr, child_stderr) = Capture::pipe()?;
            return Ok((Self::Captured(stdout, stderr), child_stdout, child_stderr));
        };
        let (read, write) = output_pipe()?;
        let stderr = write.try_clone()?;
        Ok((
            Self::Logged(log::LogSink::new(read, log)),
            Stdio::from(write),
            Stdio::from(stderr),
        ))
    }

    /// Read what the stage has written. A capture over its limit stops the
    /// stage; a log never does.
    fn poll(&mut self, read_error: &str, reason: &mut Option<String>) {
        match self {
            Self::Captured(stdout, stderr) => {
                for capture in [stdout, stderr] {
                    match capture.read_available() {
                        Ok(true) => {
                            reason.get_or_insert_with(|| "writer captured output limit exceeded".to_owned());
                        }
                        Err(error) => {
                            reason.get_or_insert_with(|| format!("{read_error}: {error}"));
                        }
                        Ok(false) => {}
                    }
                }
            }
            Self::Logged(sink) => {
                if let Err(error) = sink.read_available() {
                    reason.get_or_insert_with(|| format!("writer log failed: {error}"));
                }
            }
        }
    }

    /// The stdout and stderr a stopped frame carries. A logged stage's output
    /// is in its log.
    fn texts(&self) -> (String, String) {
        match self {
            Self::Captured(stdout, stderr) => (stdout.text(), stderr.text()),
            Self::Logged(_) => (String::new(), String::new()),
        }
    }
}

fn write_launch(
    channel: &mut std::process::ChildStdin,
    bytes: &[u8],
    input: &Receiver<io::Result<Value>>,
    deadline: Instant,
    cancelled: &mut bool,
) -> io::Result<()> {
    fcntl_setfl(&*channel, fcntl_getfl(&*channel)? | OFlags::NONBLOCK)?;
    let mut remaining = bytes;
    while !remaining.is_empty() {
        match input.try_recv() {
            Ok(Ok(message)) if field(&message, "action") == "cancel" => {
                *cancelled = true;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "writer cancelled before launch completed",
                ));
            }
            Ok(Ok(_) | Err(_)) => return Err(invalid("invalid writer control during launch")),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "writer client disconnected during launch",
                ));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let budget = deadline.saturating_duration_since(Instant::now());
        if budget.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "writer launch deadline expired",
            ));
        }
        match channel.write(remaining) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "writer launch pipe accepted no bytes",
                ));
            }
            Ok(count) => {
                remaining = remaining
                    .get(count..)
                    .ok_or_else(|| invalid("writer launch count invalid"))?;
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                thread::sleep(POLL.min(budget));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

const DESCENDANT_EXIT_GRACE: Duration = Duration::from_millis(100);

/// What a writer holds across its stages: the project, its lock files and the
/// owner record each stage writes its process into.
struct Custody<'a> {
    root: &'a Path,
    lock_path: &'a Path,
    file: &'a mut File,
    loans: &'a [File],
    record: &'a Record,
}

/// Run one stage. Tests pass a fixture `launcher` to drive real process and
/// lock custody.
fn execute_with_launcher(
    custody: Custody<'_>,
    message: &Value,
    log: Option<log::CappedLog>,
    input: &Receiver<io::Result<Value>>,
    launcher: &Path,
    mut emit: impl FnMut(&Value) -> io::Result<()>,
) -> io::Result<bool> {
    let Custody {
        root,
        lock_path,
        file,
        loans,
        record,
    } = custody;
    canonical(root)?;
    let timeout = duration(
        field(message, "timeout")
            .as_f64()
            .ok_or_else(|| invalid("writer timeout required"))?,
    )?;
    let started = Instant::now();
    let deadline = started
        .checked_add(timeout)
        .ok_or_else(|| invalid("writer timeout is too large"))?;
    let (mut output, child_stdout, child_stderr) = StageOutput::open(log)?;
    let (barrier, inherited) = pipe()?;
    fcntl_setfd(&barrier, FdFlags::CLOEXEC)?;
    fcntl_setfl(&barrier, fcntl_getfl(&barrier)? | OFlags::NONBLOCK)?;
    // No ownership FD is made inheritable in this helper's FD table. The
    // consumed command clears CLOEXEC only in its child immediately before exec.
    let mut command = Command::new(launcher);
    command
        .arg("writer-exec")
        .env(OWNERS_DIRECTORY_ENV, registry_directory()?)
        .stdin(Stdio::piped())
        .stdout(child_stdout)
        .stderr(child_stderr);
    let mut descriptors = vec![inherited.as_fd(), file.as_fd()];
    descriptors.extend(loans.iter().map(AsFd::as_fd));
    let mut group = OwnedCommandGroup::spawn_inheriting(command, &descriptors)?;
    drop(descriptors);
    drop(inherited);

    // From this point every error requests cleanup and retains this scope and
    // all borrowed resource guards until closure. A blocked wrapper cannot run
    // the command before its complete actual identity has been synced.
    let mut process = None;
    let mut cancelled = false;
    let launch_result = (|| -> io::Result<()> {
        process = Some(ProcessIdentity {
            pid: group.id(),
            process_group: group.id(),
            session: record.owner.session,
            start: birth(group.id())?,
            start_new_session: false,
        });
        persist_all(
            file,
            loans,
            &Record {
                process: process.clone(),
                ..record.clone()
            },
        )?;
        let mut launch = message.clone();
        let fields = launch
            .as_object_mut()
            .ok_or_else(|| invalid("writer launch must be an object"))?;
        fields.insert("root".to_owned(), json!(root));
        fields.insert("lock".to_owned(), json!(lock_path));
        let mut bytes = serde_json::to_vec(&launch).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > FRAME_BYTES {
            return Err(invalid("writer launch is oversized"));
        }
        let mut channel = group
            .take_stdin()
            .ok_or_else(|| invalid("writer launch channel missing"))?;
        write_launch(&mut channel, &bytes, input, deadline, &mut cancelled)?;
        drop(channel);
        emit(&json!({"state":"started","process":process,"owner":record.owner}))
    })();
    let mut reason = launch_result
        .err()
        .map(|error| format!("writer launch failed: {error}"));
    let mut notified = false;
    let mut descendant_exit_started = None;
    loop {
        match input.try_recv() {
            Ok(Ok(message)) if field(&message, "action") == "cancel" => {
                cancelled = true;
                reason.get_or_insert_with(|| "writer cancelled".to_owned());
            }
            Ok(Ok(_) | Err(_)) => {
                reason.get_or_insert_with(|| "writer control frame invalid during actual work".to_owned());
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                reason.get_or_insert_with(|| "writer client channel closed during actual work".to_owned());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        output.poll("writer output read failed", &mut reason);
        let status = match group.try_wait() {
            Ok(status) => status,
            Err(error) => {
                reason.get_or_insert_with(|| format!("writer scope observation failed: {error}"));
                None
            }
        };
        if status.is_none() && group.cleanup_pending() && !group.intervened() {
            let since = descendant_exit_started.get_or_insert_with(Instant::now);
            if since.elapsed() >= DESCENDANT_EXIT_GRACE {
                reason.get_or_insert_with(|| "writer leader exited with surviving owned work".to_owned());
            }
        }
        if started.elapsed() >= timeout {
            reason.get_or_insert_with(|| "writer deadline expired".to_owned());
        }
        if let Some(why) = &reason {
            if !notified {
                // This is a failed operation even if subsequent cleanup confirms
                // root exit zero. It can never authorize another writer stage.
                let _delivery = emit(&json!({"state":"still_running","result":"no_result",
                    "reason":why,"process":process,"owner":record.owner}));
                notified = true;
            }
            if status.is_none() {
                // Failures retain the actual child pin and are retried on later
                // polls. ECHILD latches lost custody and never sends a signal.
                let _cleanup = group.request_stop();
            }
        }
        if let Some(status) = status {
            let barrier_stopped = match barrier_closed(&barrier) {
                Ok(stopped) => stopped,
                Err(error) => {
                    reason.get_or_insert_with(|| format!("writer descriptor closure failed: {error}"));
                    false
                }
            };
            if barrier_stopped {
                let cleared = persist_all(
                    file,
                    loans,
                    &Record {
                        process: None,
                        ..record.clone()
                    },
                );
                if let Err(error) = &cleared {
                    reason.get_or_insert_with(|| format!("writer closed but ownership record cleanup failed: {error}"));
                }
                output.poll("writer final output read failed", &mut reason);
                let unavailable = reason.is_some() || group.intervened();
                let (stdout, stderr) = output.texts();
                // Physical closure is a distinct terminal fact even when the
                // command's result is unusable. Never lose it behind the earlier
                // provisional stopping frame or interpret exit zero as success.
                let result = json!({"state":"stopped","exit_code":code(status),
                    "stdout":stdout,"stderr":stderr,"process":process,
                    "owner":record.owner,"owned_work_stopped":true,"cancelled":cancelled,
                    "intervened":unavailable,"result":if unavailable {"no_result"} else {"result"},
                    "reason":reason});
                emit(&result)?;
                cleared?;
                return Ok(!unavailable);
            }
        }
        thread::sleep(POLL);
    }
}

/// Run one standalone writer supervisor.
///
/// # Errors
/// Returns admission, protocol, process-observation, or child-launch failures.
pub fn run_cli(args: &WriterArgs) -> io::Result<()> {
    if args.lock != lean_toolchain::shared_writer::project_lock_path(&args.root)? {
        return Err(invalid("writer lock differs from canonical project domain"));
    }
    let workspaces = lean_toolchain::shared_writer::inherited_writer_workspaces(&args.root)?;
    let inherited = lean_toolchain::shared_writer::inherited_writer_locks()?;
    if inherited.is_none() || args.root.exists() {
        canonical(&args.root)?;
    } else {
        canonical(
            args.root
                .parent()
                .ok_or_else(|| invalid("prospective writer root has no parent"))?,
        )?;
        if !matches!(
            args.root
                .file_name()
                .map(Path::new)
                .and_then(|name| name.components().next()),
            Some(std::path::Component::Normal(_))
        ) {
            return Err(invalid("prospective writer root needs one normal basename"));
        }
    }
    if sid(None)? != std::process::id() {
        setsid()?;
    }
    let input = control();
    let admission_closed = Cell::new(false);
    let cancelled = || {
        if !matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            admission_closed.set(true);
        }
        admission_closed.get()
    };
    let borrowed = inherited.is_some();
    // This guard coordinates admission only. Artifact EX aliases and process
    // fences still prevent readers after helper death or uncertain cleanup.
    let (locks, _demand) = if let Some(locks) = inherited {
        if locks.first().is_none_or(|(path, _)| path != &args.lock) {
            return Err(invalid("writer primary loan does not match its lock domain"));
        }
        // Inherited custody may also include source-entry locks. Only physical
        // artifact domains participate in native reader demand coordination.
        let paths = workspace_domains(&workspaces)?;
        let demand = WriterDemand::announce(&paths, duration(args.admission)?, &cancelled)?;
        (locks, demand)
    } else {
        lock_workspaces(&workspaces, &args.lock, duration(args.admission)?, &cancelled)?
    };
    validate_workspace_custody(&workspaces, &locks)?;
    let receipt = if borrowed {
        lean_toolchain::shared_writer::writer_lock_receipt(&locks)?
    } else {
        Value::Null
    };
    let paths: Vec<_> = locks.iter().map(|(path, _)| path.clone()).collect();
    let mut files = locks.into_iter().map(|(_, file)| file);
    let mut file = files.next().ok_or_else(|| invalid("writer primary loan missing"))?;
    let loans: Vec<_> = files.collect();
    let current = std::process::id();
    let record = Record {
        version: PROTOCOL,
        owner: Owner {
            version: PROTOCOL,
            pid: current,
            start: birth(current)?,
            session: current,
        },
        process: None,
    };
    persist_all(&mut file, &loans, &record)?;
    let asset_dir = tempfile::Builder::new()
        .prefix("lean-writer-assets-")
        .tempdir()?
        .keep()
        .canonicalize()?;
    let result = (|| -> io::Result<()> {
        send(&json!({"state":"locked","owner":record.owner,"lock_loans":receipt,"asset_dir":asset_dir}))?;
        while let Ok(message) = input.recv() {
            let mut message = message?;
            match field(&message, "action").as_str() {
                Some("run") => {
                    // Re-derive the declared graph immediately before this stage.
                    // Unknown or expanded graphs never release a command body.
                    for required in workspace_domains(&workspaces)? {
                        if !paths.contains(&required) {
                            return Err(invalid("writer dependency graph expanded after admission"));
                        }
                    }
                    for (path, held) in paths.iter().zip(std::iter::once(&file).chain(loans.iter())) {
                        let opened = held.metadata()?;
                        let named = fs::symlink_metadata(path)?;
                        if !named.is_file()
                            || opened.nlink() != 1
                            || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
                        {
                            return Err(invalid("writer domain inode changed before GO"));
                        }
                        if !lean_toolchain::native_guardian::protected_native_reads_stopped(
                            &lean_toolchain::native_guardian::registry_directory()?,
                            path,
                            (opened.dev(), opened.ino()),
                        )? {
                            return Err(invalid("native artifact readers remain unresolved before GO"));
                        }
                    }
                    let log = match normalize_owner_directory(&mut message, &registry_directory()?)
                        .and_then(|()| log::take_log(&mut message))
                    {
                        Ok(log) => log,
                        Err(error) => {
                            send(&json!({"state":"stage_refused","owner":record.owner,"reason":error.to_string()}))?;
                            continue;
                        }
                    };
                    let custody = Custody {
                        root: &args.root,
                        lock_path: &args.lock,
                        file: &mut file,
                        loans: &loans,
                        record: &record,
                    };
                    if !execute_with_launcher(custody, &message, log, &input, &std::env::current_exe()?, send)? {
                        // Ok(false) follows group and descriptor closure, the synced
                        // custody update, and delivery of stopped/no_result.
                        break;
                    }
                }
                Some("close" | "cancel") => break,
                _ => return Err(invalid("unsupported writer action")),
            }
        }
        Ok(())
    })();
    // execute retains custody after every launch until actual group closure.
    // Idle or pre-GO failures have no command that could still need these inputs.
    let cleanup = fs::remove_dir_all(&asset_dir);
    result.and(cleanup)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "isolated writer record fixtures assert exact expected outcomes"
)]
mod tests {
    use super::*;

    #[test]
    fn capture_reports_first_overflow_then_keeps_draining_without_growing_prefix() {
        let (read, write) = pipe().expect("test pipe");
        fcntl_setfl(&read, fcntl_getfl(&read).expect("read flags") | OFlags::NONBLOCK).expect("nonblocking reader");
        fcntl_setfl(&write, fcntl_getfl(&write).expect("write flags") | OFlags::NONBLOCK).expect("nonblocking writer");
        let mut capture = Capture {
            descriptor: read,
            bytes: Vec::new(),
            overflowed: false,
        };
        let chunk = [b'x'; 1024];
        // Every poll must consume the next chunk even after overflow. A reader
        // that only returns its overflow flag leaves these bytes in the pipe.
        for index in 0..256_usize {
            assert_eq!(rustix::io::write(&write, &chunk).expect("write chunk"), chunk.len());
            assert_eq!(capture.read_available().expect("drain chunk"), index >= 64);
            if index == 64 {
                assert!(capture.overflowed, "the first excess chunk reports overflow at once");
                assert_eq!(capture.bytes.len(), CAPTURE_BYTES);
            }
            // No queued chunk may survive because the overflow flag is already set.
            let mut leftover = [0; 1];
            assert_eq!(
                rustix::io::read(&capture.descriptor, &mut leftover),
                Err(rustix::io::Errno::AGAIN)
            );
            assert_eq!(
                capture.bytes.len(),
                index.saturating_add(1).saturating_mul(chunk.len()).min(CAPTURE_BYTES)
            );
        }
        assert!(capture.bytes.iter().all(|byte| *byte == b'x'));
        drop(write);
        assert!(capture.read_available().expect("overflow remains visible at EOF"));
    }

    #[test]
    fn capture_exact_limit_is_not_overflow() {
        let (read, write) = pipe().expect("test pipe");
        fcntl_setfl(&read, fcntl_getfl(&read).expect("read flags") | OFlags::NONBLOCK).expect("nonblocking reader");
        let mut capture = Capture {
            descriptor: read,
            bytes: Vec::new(),
            overflowed: false,
        };
        let chunk = [b'a'; 1024];
        for _ in 0..64 {
            assert_eq!(rustix::io::write(&write, &chunk).expect("write chunk"), chunk.len());
            assert!(!capture.read_available().expect("drain exact prefix"));
        }
        drop(write);
        assert!(!capture.read_available().expect("exact prefix EOF"));
        assert_eq!(capture.bytes.len(), CAPTURE_BYTES);
    }

    fn process_identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: 123,
            process_group: 123,
            session: 100,
            start: "original:123".to_owned(),
            start_new_session: false,
        }
    }

    #[test]
    fn reused_leader_birth_cannot_hold_completed_work() {
        let called = std::cell::Cell::new(false);
        let result = group_live_with(
            &process_identity(),
            |_| Ok("later:456".to_owned()),
            |_| {
                called.set(true);
                Ok(true)
            },
        );
        assert!(matches!(result, Ok(false)));
        assert!(!called.get());
    }

    #[test]
    fn missing_leader_does_not_hide_surviving_group_or_observation_failure() {
        let missing = |_| Err(io::Error::from(io::ErrorKind::NotFound));
        assert!(matches!(
            group_live_with(&process_identity(), missing, |_| Ok(true)),
            Ok(true)
        ));
        assert!(
            group_live_with(&process_identity(), missing, |_| Err(io::Error::from(
                io::ErrorKind::PermissionDenied
            )))
            .is_err()
        );
    }

    #[test]
    fn finite_timeout_and_owner_identity_are_strict() {
        assert!(duration(f64::NAN).is_err());
        assert!(duration(0.0).is_err());
        let mut file = tempfile::tempfile().expect("fixture");
        file.write_all(b"{}\n").expect("fixture");
        assert!(owner(&mut file).is_err());
    }

    #[test]
    fn torn_prelaunch_record_is_not_committed() {
        let mut file = tempfile::tempfile().expect("fixture");
        file.write_all(b"{\"version\":1").expect("fixture");
        assert!(owner(&mut file).expect("uncommitted").is_none());
    }

    #[test]
    fn helper_executable_is_set_once_and_reported_by_the_contract() {
        let directory = tempfile::tempdir().expect("fixture");
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::write(&first, "").expect("fixture");
        fs::write(&second, "").expect("fixture");
        use_helper_executable(&first).expect("first call");
        use_helper_executable(&first).expect("same file again");
        assert!(use_helper_executable(&second).is_err());
        assert_eq!(
            contract().expect("contract").executable,
            first.canonicalize().expect("fixture")
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "shared_writer/lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(all(test, unix))]
#[path = "shared_writer/sdk_contract_tests.rs"]
mod sdk_contract_tests;
