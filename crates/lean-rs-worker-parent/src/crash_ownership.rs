//! Kernel-bound worker ownership and the durable startup lifeline.
//! Serialized identities never authorize process signals. The independent guardian
//! handles server death; a surviving parent retains its actual child unreaped for
//! the shared owned-group fallback when that guardian fails.
use lean_toolchain::native_guardian::{self, NativeReadDomain};
pub use lean_toolchain::native_guardian::{
    NativeOwnerObservation as LeanWorkerOwnerObservation, NativeProcessIdentity as LeanWorkerOwnedProcess,
};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt as _, PermissionsExt};
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
pub(crate) const SOCKET_ENV: &str = "LEAN_RS_WORKER_PARENT_SOCKET";
pub(crate) const NONCE_ENV: &str = "LEAN_RS_WORKER_PARENT_NONCE";
fn read_startup_hello(stream: &mut UnixStream, deadline: Instant) -> io::Result<String> {
    // Darwin accepted sockets can inherit O_NONBLOCK. Normalize explicitly and
    // use the absolute startup budget rather than restarting a timeout after
    // every byte of a partial HELLO.
    stream.set_nonblocking(true)?;
    let mut line = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native startup HELLO deadline elapsed",
            ));
        }
        let mut byte = [0_u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "native startup HELLO closed",
                ));
            }
            Ok(_) => {
                line.extend_from_slice(&byte);
                if line.len() > 16384 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "native startup HELLO exceeds limit",
                    ));
                }
                if byte == *b"\n" {
                    stream.set_nonblocking(false)?;
                    return String::from_utf8(line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
                }
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                std::thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(error) => return Err(error),
        }
    }
}
fn write_before_deadline(stream: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let result = (|| {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "startup ACK deadline elapsed"));
            }
            #[cfg(test)]
            let offered = lifecycle_tests::startup_faults::write_limit(bytes.len());
            #[cfg(not(test))]
            let offered = bytes.len();
            let offered = bytes
                .get(..offered)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "startup ACK write limit exceeds frame"))?;
            match stream.write(offered) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "startup ACK closed")),
                Ok(n) => {
                    #[cfg(test)]
                    lifecycle_tests::startup_faults::after_write(n);
                    bytes = bytes.get(n..).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "startup ACK write exceeds offered bytes")
                    })?;
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                    std::thread::sleep(
                        Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    })();
    let restored = stream.set_nonblocking(false);
    result.and(restored)
}
fn running() -> String {
    "running".to_owned()
}
/// Durable startup evidence for one generation. This snapshot accepts legacy
/// and guarded startup records. Recovery records use a private registry format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkerOwnedGeneration {
    #[serde(deserialize_with = "deserialize_snapshot_version")]
    pub version: u32,
    pub scope: String,
    pub generation_id: String,
    pub parent: LeanWorkerOwnedProcess,
    pub process: LeanWorkerOwnedProcess,
    #[serde(default = "running")]
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian: Option<LeanWorkerOwnedProcess>,
    #[serde(default)]
    pub protected_domains: Vec<NativeReadDomain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_slot: Option<native_guardian::NativeCapacitySlot>,
}

fn deserialize_snapshot_version<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    match version {
        1 | 2 => Ok(version),
        _ => Err(serde::de::Error::custom(
            "unsupported native generation snapshot version",
        )),
    }
}

/// Link-free lifetime admission for an external native process group.
///
/// The controlled bootstrap must establish a private session, start the selected
/// guardian and wait for the durable ACK before spawning any native payload.
/// This facade adds no worker body protocol and never replays a command.
#[derive(Debug)]
pub struct LeanWorkerNativeScope(ParentLifetime);

/// Exact admitted group and its parent-to-guardian cleanup capability.
#[derive(Debug)]
pub struct LeanWorkerNativeScopeLease(crate::process::OwnedChild);

/// Startup failure with its original error and any outstanding cleanup owner.
#[derive(Debug)]
pub enum LeanWorkerNativeSpawnError {
    /// The SDK received no child handle. This covers errors before process
    /// creation and launch errors such as exec failure in a child reaped by
    /// the standard library. A failed dispatch write may still fence the registry.
    BeforeSpawn(io::Error),
    /// A child was created. Retain host resources until `custody_closed` is true.
    AfterSpawn {
        source: io::Error,
        pending: LeanWorkerNativePendingCleanup,
    },
}

impl std::fmt::Display for LeanWorkerNativeSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforeSpawn(source) => write!(f, "native startup before spawn: {source}"),
            Self::AfterSpawn { source, .. } => write!(f, "native startup after spawn: {source}"),
        }
    }
}
impl std::error::Error for LeanWorkerNativeSpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::BeforeSpawn(source) | Self::AfterSpawn { source, .. } => source,
        })
    }
}

/// Retained local process/tracker ownership after failed startup.
///
/// Dropping this value delegates cleanup, but does not certify closure. Unknown
/// authenticated identity remains fenced; direct exit alone is not closure.
#[derive(Debug)]
pub struct LeanWorkerNativePendingCleanup {
    child: Box<crate::process::OwnedChild>,
    custody: BootstrapCustody,
}
impl LeanWorkerNativePendingCleanup {
    /// Read the last observed cleanup state without polling or requesting stop.
    /// This may start a caller's bounded cleanup wait, but is neither a closure
    /// certificate nor authority to release retained resources. A false value
    /// also does not prove the process is live or custody is closed.
    #[must_use]
    pub fn cleanup_pending(&self) -> bool {
        self.child.cleanup_pending()
    }
    fn new(mut child: crate::process::OwnedChild, custody: BootstrapCustody) -> Self {
        let _stop = child.kill(); // Preserve the original startup error.
        Self {
            child: Box::new(child),
            custody,
        }
    }
    /// Observe exact group and guardian closure independently of wait status.
    /// # Errors
    /// A failed OS observation is not closure.
    pub fn custody_closed(&self) -> io::Result<bool> {
        self.custody.closed()
    }
    /// Poll owned cleanup. ECHILD remains an error, even if custody is closed.
    /// # Errors
    /// Preserves wait errors without inventing an exit receipt.
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
}

impl LeanWorkerNativeScope {
    /// Reserve the existing registry and optional shared capacity slot.
    ///
    /// # Errors
    /// Refuses invalid configuration, full capacity and unresolved old ownership.
    pub fn prepare(
        registry: &Path,
        timeout: Duration,
        slot: Option<&native_guardian::NativeCapacitySlot>,
    ) -> io::Result<Self> {
        ParentLifetime::prepare_guarded_with_slot(registry, timeout, slot).map(Self)
    }

    /// Reserve the same guarded scope with its temporary socket under an explicit private directory.
    /// The durable generation record and optional capacity slot remain in `registry`.
    ///
    /// # Errors
    /// Returns an error before admission if the socket root is noncanonical, is not owned
    /// by the caller with mode 0700, or cannot fit the platform's Unix socket path limit.
    pub fn prepare_with_socket_directory(
        registry: &Path,
        timeout: Duration,
        slot: Option<&native_guardian::NativeCapacitySlot>,
        socket_directory: &Path,
    ) -> io::Result<Self> {
        ParentLifetime::prepare_with_socket_directory(registry, Some(socket_directory), true, timeout, slot).map(Self)
    }

    /// Create the private input directory owned by this preparing generation.
    /// # Errors
    /// Refuses aliased or invalid owned directories.
    pub fn input_directory(&self) -> io::Result<PathBuf> {
        native_guardian::create_generation_inputs(&self.0.directory)
    }

    /// Bind the controlled bootstrap to this exact private lifetime.
    pub fn configure(&self, command: &mut Command) {
        self.0.configure(command);
    }

    /// Spawn and adopt the controlled bootstrap under one retained cleanup owner.
    /// HELLO waiting is synchronous and bounded by `timeout`, not promptly
    /// cancellable mid-wait. After return, the caller must check cancellation
    /// and its deadline before ACK. Host keepalive ownership remains the caller's.
    /// # Errors
    /// Distinguishes errors without a child handle from errors with retained cleanup.
    pub fn spawn(
        mut self,
        command: &mut Command,
        timeout: Duration,
    ) -> Result<LeanWorkerNativeScopeLease, LeanWorkerNativeSpawnError> {
        let started = Instant::now();
        self.0.configure(command);
        let tracker = crate::process::LeanWorkerProcessTracker::new();
        let custody = self.0.custody.clone();
        let mut child = tracker
            .spawn_guarded(command, &mut self.0)
            .map_err(LeanWorkerNativeSpawnError::BeforeSpawn)?;
        let lease = match self.0.accept(child.id(), timeout.saturating_sub(started.elapsed())) {
            Ok(lease) => lease,
            Err(source) => {
                return Err(LeanWorkerNativeSpawnError::AfterSpawn {
                    source,
                    pending: LeanWorkerNativePendingCleanup::new(child, custody),
                });
            }
        };
        if let Err(source) = child.attach_lifetime(lease) {
            return Err(LeanWorkerNativeSpawnError::AfterSpawn {
                source,
                pending: LeanWorkerNativePendingCleanup::new(child, custody),
            });
        }
        Ok(LeanWorkerNativeScopeLease(child))
    }
}

impl LeanWorkerNativeScopeLease {
    /// Read the last observed cleanup state without polling or requesting stop.
    /// This may start a caller's bounded cleanup wait, but is neither a closure
    /// certificate nor authority to release retained resources. A false value
    /// also does not prove the process is live or custody is closed.
    #[must_use]
    pub fn cleanup_pending(&self) -> bool {
        self.0.cleanup_pending()
    }
    /// Take one pipe once; the lease retains process and cleanup ownership.
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.0.stdin.take()
    }
    /// Take one output pipe once without exposing the child wait/signal handle.
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.0.stdout.take()
    }
    /// Take one diagnostic pipe once without exposing process ownership.
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.0.stderr.take()
    }
    /// Request owned cleanup through the exact child/group and guardian channel.
    /// # Errors
    /// Failed stop retains custody for later polling.
    pub fn kill(&mut self) -> io::Result<()> {
        self.0.kill()
    }
    /// Return this generation's existing private inputs without creating them.
    /// # Errors
    /// Refuses missing, aliased or invalid owned directories.
    pub fn input_directory(&self) -> io::Result<PathBuf> {
        native_guardian::generation_inputs(
            self.lifetime()?
                .record
                .parent()
                .ok_or_else(|| io::Error::other("owner directory missing"))?,
        )
    }

    /// Remove only owned inputs after the payload group and guardian closed.
    /// # Errors
    /// Returns pending custody or bounded no-follow cleanup failure.
    pub fn cleanup_inputs(&self) -> io::Result<()> {
        if !self.custody_closed()? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native input custody still active",
            ));
        }
        native_guardian::cleanup_generation_inputs(
            self.lifetime()?
                .record
                .parent()
                .ok_or_else(|| io::Error::other("owner directory missing"))?,
        )
    }

    /// Release only this admitted bootstrap after its durable owner publication.
    ///
    /// # Errors
    /// Returns a broken or timed-out private guardian channel.
    pub fn acknowledge(&mut self) -> io::Result<()> {
        self.0.acknowledge_lifetime()
    }

    /// Observe the exact Running guardian scope for an external managed reader.
    ///
    /// # Errors
    /// Refuses an unavailable, unguarded or changed generation.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn managed_guardian_binding(&self) -> io::Result<native_guardian::NativeGuardianDrainBinding> {
        self.lifetime()?.managed_guardian_binding()
    }

    /// Arm the already admitted external reader's original deadline after read retention.
    ///
    /// Keep the bootstrap blocked until it has acknowledged the entire expected read
    /// union and this call has verified the guardian's exact deadline ACK. The
    /// caller must match the stored admission boot to the expected binding.
    ///
    /// # Errors
    /// Refuses stale identity, missing retained reads, expired time or an
    /// unconfirmed ACK. Never release the payload or retry an unconfirmed arm.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn arm_admitted_reader_deadline(
        &mut self,
        expected: &native_guardian::NativeGuardianDrainBinding,
        attempt: [u8; 16],
        stage: u64,
        expires_ns: u64,
    ) -> io::Result<()> {
        self.lifetime()?.validate_managed_scope(expected)?;
        let lease = self
            .0
            .lifetime
            .as_mut()
            .ok_or_else(|| io::Error::other("native custody lease missing"))?;
        lease.arm_deadline(
            &ManagedRequestDeadline {
                attempt,
                stage,
                expires_ns,
            },
            "arm_reader_hard_deadline",
            None,
        )
    }

    /// Request cleanup from the sole guardian, without signalling a stored PID.
    ///
    /// # Errors
    /// Returns a failed shutdown of the private lifetime channel.
    pub fn request_stop(&self) -> io::Result<()> {
        self.lifetime()?.request_stop()
    }

    /// Observe closure of the exact inherited payload group.
    ///
    /// # Errors
    /// Failed kernel observation remains an error, never a closure assertion.
    pub fn group_absent(&self) -> io::Result<bool> {
        self.lifetime()?.group_absent()
    }

    /// Observe the exact guardian identity authenticated at admission.
    ///
    /// # Errors
    /// Failed process observations are not a guardian-death assertion.
    pub fn guardian_alive(&self) -> io::Result<bool> {
        self.lifetime()?.guardian_alive()
    }

    /// Observe group closure and release of the guardian's read descriptors.
    ///
    /// # Errors
    /// Failed kernel observations never establish custody release.
    pub fn custody_closed(&self) -> io::Result<bool> {
        self.lifetime()?.custody_closed()
    }

    /// Kernel identity authenticated for this actual bootstrap child.
    ///
    /// # Errors
    /// Returns an error when the admitted lifetime is unavailable.
    pub fn owner(&self) -> io::Result<&LeanWorkerOwnedProcess> {
        Ok(&self.lifetime()?.owner)
    }
    fn lifetime(&self) -> io::Result<&ParentLifetimeLease> {
        self.0
            .lifetime
            .as_ref()
            .ok_or_else(|| io::Error::other("native custody lease missing"))
    }
    /// Poll the actual owned group and guardian; never infer exit from a numeric PID.
    /// # Errors
    /// Returns wait or custody observation errors without releasing capacity.
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }
}

/// Shared from before spawn through every cleanup path. Unknown is not absent.
#[derive(Clone, Debug, Default)]
pub(crate) struct BootstrapCustody {
    state: Arc<Mutex<Option<AuthenticatedBootstrap>>>,
    ordinary: bool,
}
#[derive(Debug)]
struct AuthenticatedBootstrap {
    stream: UnixStream,
    owner: LeanWorkerOwnedProcess,
    guardian: Option<LeanWorkerOwnedProcess>,
}
impl BootstrapCustody {
    pub(crate) fn for_spawn(guarded: bool) -> Self {
        Self {
            ordinary: !guarded,
            ..Self::default()
        }
    }
    /// Only an owned direct-child wait status permits the ordinary pre-HELLO case.
    /// This does not establish group absence and must never handle ECHILD.
    pub(crate) fn closed_after_direct_reap(&self) -> io::Result<bool> {
        if self.ordinary
            && self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
        {
            return Ok(true);
        }
        self.closed()
    }
    pub(crate) fn closed(&self) -> io::Result<bool> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((owner, guardian)) = state
            .as_ref()
            .map(|state| (state.owner.clone(), state.guardian.clone()))
        else {
            return Ok(false);
        };
        drop(state);
        Ok(!lean_toolchain::process_group::group_present(owner.process_group)?
            && guardian.as_ref().map_or(Ok(true), |g| g.is_live().map(|v| !v))?)
    }
    pub(crate) fn request_stop(&self) {
        if let Some(state) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            let _stop = state.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[derive(Debug)]
pub(crate) struct ParentLifetime {
    listener: UnixListener,
    directory: PathBuf,
    socket_directory: PathBuf,
    nonce: String,
    guarded: bool,
    capacity_slot: Option<native_guardian::NativeCapacitySlot>,
    pub(crate) custody: BootstrapCustody,
    dispatched: bool,
}
#[derive(Debug)]
pub(crate) struct ParentLifetimeLease {
    stream: UnixStream,
    pub(crate) owner: LeanWorkerOwnedProcess,
    guardian: Option<LeanWorkerOwnedProcess>,
    pub(crate) record: PathBuf,
    nonce: String,
    guarded: bool,
    generation: LeanWorkerOwnedGeneration,
    deadline: Instant,
    running_published: bool,
}
// Validate before creating a socket or reserving durable ownership. The root is
// caller-controlled; it must remain private and stable throughout preparation.
fn validate_socket_root(root: &Path, socket: &Path) -> io::Result<()> {
    if !root.is_absolute() || root.canonicalize()? != root {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lifeline socket root must be absolute and canonical",
        ));
    }
    let metadata = fs::symlink_metadata(root)?;
    let uid = rustix::process::geteuid().as_raw();
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "lifeline socket root must be owned by this user with mode 0700",
        ));
    }
    SocketAddr::from_pathname(socket)?;
    Ok(())
}
impl ParentLifetime {
    #[cfg(test)]
    pub(crate) fn prepare(root: &Path) -> io::Result<Self> {
        Self::prepare_inner(root, false, Duration::from_secs(2), None, None)
    }
    #[cfg(test)]
    pub(crate) fn prepare_guarded(root: &Path, timeout: Duration) -> io::Result<Self> {
        Self::prepare_inner(root, true, timeout, None, None)
    }
    pub(crate) fn prepare_guarded_with_slot(
        root: &Path,
        timeout: Duration,
        slot: Option<&native_guardian::NativeCapacitySlot>,
    ) -> io::Result<Self> {
        Self::prepare_inner(root, true, timeout, slot, None)
    }
    pub(crate) fn prepare_with_socket_directory(
        root: &Path,
        socket_root: Option<&Path>,
        guarded: bool,
        timeout: Duration,
        slot: Option<&native_guardian::NativeCapacitySlot>,
    ) -> io::Result<Self> {
        Self::prepare_inner(root, guarded, timeout, slot, socket_root)
    }
    fn prepare_inner(
        root: &Path,
        guarded: bool,
        timeout: Duration,
        slot: Option<&native_guardian::NativeCapacitySlot>,
        socket_root: Option<&Path>,
    ) -> io::Result<Self> {
        let started = Instant::now();
        let mut random = [0_u8; 24];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let mut nonce = String::with_capacity(48);
        for byte in random {
            write!(nonce, "{byte:02x}").map_err(io::Error::other)?;
        }
        let socket_directory = socket_root
            .unwrap_or_else(|| Path::new("/tmp"))
            .join(format!("lean-worker-{}", &nonce[..24]));
        if let Some(socket_root) = socket_root {
            validate_socket_root(socket_root, &socket_directory.join("parent.sock"))?;
        }
        #[cfg(test)]
        let socket_directory = lifecycle_tests::NEXT_SOCKET_DIRECTORY
            .take()
            .unwrap_or(socket_directory);
        let mut created_directory = false;
        let mut bound_socket = false;
        let initialized = (|| {
            fs::DirBuilder::new().mode(0o700).create(&socket_directory)?;
            created_directory = true;
            fs::set_permissions(&socket_directory, fs::Permissions::from_mode(0o700))?;
            let listener = UnixListener::bind(socket_directory.join("parent.sock"))?;
            bound_socket = true;
            listener.set_nonblocking(true)?;
            if guarded {
                fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?;
            } else {
                fs::create_dir_all(root)?;
            }
            let directory = if guarded {
                native_guardian::reserve_owner_record_with_slot(
                    root,
                    &nonce,
                    timeout.saturating_sub(started.elapsed()).min(Duration::from_secs(2)),
                    slot,
                )?
                .parent()
                .ok_or_else(|| io::Error::other("owner directory missing"))?
                .to_path_buf()
            } else {
                let directory = root.join(format!("worker-{nonce}"));
                fs::DirBuilder::new().mode(0o700).create(&directory)?;
                directory
            };
            Ok(Self {
                listener,
                directory,
                socket_directory: socket_directory.clone(),
                nonce,
                guarded,
                capacity_slot: slot.cloned(),
                custody: BootstrapCustody::for_spawn(guarded),
                dispatched: false,
            })
        })();
        if initialized.is_err() {
            if bound_socket {
                let _socket = fs::remove_file(socket_directory.join("parent.sock"));
            }
            if created_directory {
                let _socket_directory = fs::remove_dir(socket_directory);
            }
        }
        initialized
    }
    pub(crate) fn dispatch(&mut self) -> io::Result<()> {
        // Set first: a rename/fsync error may still have committed dispatching.
        self.dispatched = true;
        if self.guarded {
            native_guardian::dispatch_owner_record(&self.directory.join("owner.json"), &self.nonce)?;
        }
        Ok(())
    }
    pub(crate) fn no_spawn(&self) -> io::Result<()> {
        if self.guarded {
            native_guardian::retire_unspawned_owner_record(&self.directory.join("owner.json"), &self.nonce)?;
        }
        Ok(())
    }
    pub(crate) fn configure(&self, command: &mut Command) {
        command
            .env(SOCKET_ENV, self.socket_directory.join("parent.sock"))
            .env(NONCE_ENV, &self.nonce);
        if self.guarded
            && let Some(root) = self.directory.parent()
        {
            command.env(native_guardian::OWNERS_DIRECTORY_ENV, root);
        }
    }
    pub(crate) fn accept(mut self, child_pid: u32, timeout: Duration) -> io::Result<ParentLifetimeLease> {
        // Legacy test callers configure/spawn separately. Never retire their possible spawn.
        self.dispatched = true;
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "worker startup deadline elapsed"))?;
        let (mut stream, _) = loop {
            match self.listener.accept() {
                Ok(pair) => break pair,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "native startup connection deadline elapsed",
                    ));
                }
                Err(error) => return Err(error),
            }
        };
        let line = read_startup_hello(&mut stream, deadline)?;
        let guardian_pid = if self.guarded {
            let hello: serde_json::Value = serde_json::from_str(&line).map_err(io::Error::other)?;
            if hello.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(native_guardian::PROTOCOL))
                || hello.get("nonce").and_then(serde_json::Value::as_str) != Some(&self.nonce)
                || hello.get("worker_pid").and_then(serde_json::Value::as_u64) != Some(u64::from(child_pid))
            {
                return Err(io::Error::other("native guardian authentication failed"));
            }
            Some(
                hello
                    .get("guardian_pid")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                    .ok_or_else(|| io::Error::other("native guardian PID missing"))?,
            )
        } else {
            if line != format!("{} {child_pid}\n", self.nonce) {
                return Err(io::Error::other("child lifeline authentication failed"));
            }
            None
        };
        let owner =
            LeanWorkerOwnedProcess::capture(child_pid)?.ok_or_else(|| io::Error::other("owned child disappeared"))?;
        if owner.session != child_pid || owner.process_group != child_pid {
            return Err(io::Error::other("worker did not establish private session"));
        }
        let guardian = guardian_pid
            .map(|pid| {
                let guard = LeanWorkerOwnedProcess::capture(pid)?
                    .ok_or_else(|| io::Error::other("native guardian disappeared"))?;
                if guard.session != child_pid || guard.process_group != pid || pid == child_pid {
                    return Err(io::Error::other("native guardian does not pin owned session"));
                }
                Ok(guard)
            })
            .transpose()?;
        // Install authenticated cleanup evidence before any fallible publication.
        *self
            .custody
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(AuthenticatedBootstrap {
            stream,
            owner: owner.clone(),
            guardian: guardian.clone(),
        });
        let stream = self
            .custody
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or_else(|| io::Error::other("authenticated bootstrap missing"))?
            .stream
            .try_clone()?;
        let generation = LeanWorkerOwnedGeneration {
            version: if self.guarded { 2 } else { 1 },
            scope: if self.guarded {
                "inherited-process-group-v2"
            } else {
                "inherited-process-group-v1"
            }
            .to_owned(),
            generation_id: self.nonce.clone(),
            parent: LeanWorkerOwnedProcess::capture(std::process::id())?
                .ok_or_else(|| io::Error::other("parent process identity unavailable"))?,
            process: owner.clone(),
            phase: if self.guarded {
                "bootstrap".to_owned()
            } else {
                running()
            },
            guardian,
            protected_domains: Vec::new(),
            capacity_slot: self.capacity_slot.clone(),
        };
        let record = self.directory.join("owner.json");
        if self.guarded {
            native_guardian::publish_startup_owner(
                &record,
                &serde_json::to_value(&generation).map_err(io::Error::other)?,
                "dispatching",
                deadline.saturating_duration_since(Instant::now()),
            )?;
        } else {
            let temporary = self.directory.join("owner.write");
            let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
            file.write_all(&serde_json::to_vec(&generation).map_err(io::Error::other)?)?;
            file.sync_all()?;
            fs::rename(temporary, &record)?;
            File::open(&self.directory)?.sync_all()?;
        }
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))?;
        fs::remove_file(self.socket_directory.join("parent.sock"))?;
        fs::remove_dir(&self.socket_directory)?;
        Ok(ParentLifetimeLease {
            stream,
            owner,
            guardian: generation.guardian.clone(),
            record,
            nonce: self.nonce.clone(),
            guarded: self.guarded,
            generation,
            deadline,
            running_published: false,
        })
    }
}
/// One original managed attempt/stage and its immutable same-boot deadline.
pub(crate) struct ManagedRequestDeadline {
    pub(crate) attempt: [u8; 16],
    pub(crate) stage: u64,
    pub(crate) expires_ns: u64,
}

impl ManagedRequestDeadline {
    pub(crate) fn remaining(&self) -> io::Result<Duration> {
        let remaining =
            Duration::from_nanos(self.expires_ns).saturating_sub(native_guardian::guardian_deadline_ticks()?);
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "managed request deadline elapsed",
            ));
        }
        Ok(remaining)
    }
}

impl ParentLifetimeLease {
    pub(crate) fn publish_running(&mut self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "startup deadline elapsed"));
        }
        #[cfg(test)]
        lifecycle_tests::startup_faults::before_running_publication(&self.record);
        self.generation.phase = running();
        if self.guarded {
            native_guardian::publish_startup_owner(
                &self.record,
                &serde_json::to_value(&self.generation).map_err(io::Error::other)?,
                "bootstrap",
                self.deadline.saturating_duration_since(Instant::now()),
            )?;
        }
        self.stream
            .set_write_timeout(Some(self.deadline.saturating_duration_since(Instant::now())))?;
        self.running_published = true;
        Ok(())
    }

    /// Correlation with the actual guarded admission, not authority to signal.
    pub(crate) fn guarded_generation_id(&self) -> Option<&str> {
        self.guarded.then_some(self.nonce.as_str())
    }

    pub(crate) fn acknowledge(&mut self) -> io::Result<()> {
        if !self.running_published {
            return Err(io::Error::other("startup ACK requires adopted durable Running"));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "startup ACK deadline elapsed"));
        }
        if self.guarded {
            let mut bytes = serde_json::to_vec(
                &serde_json::json!({"state":"durable-owner-ready","nonce":self.nonce,"record":self.record}),
            )
            .map_err(io::Error::other)?;
            bytes.push(b'\n');
            write_before_deadline(&mut self.stream, &bytes, self.deadline)
        } else {
            write_before_deadline(&mut self.stream, b"durable-owner-ready\n", self.deadline)
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn managed_guardian_binding(&self) -> io::Result<native_guardian::NativeGuardianDrainBinding> {
        let observed = native_guardian::observe_managed_guardian_binding(&self.record)?;
        self.validate_managed_scope(&observed)?;
        Ok(observed)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn validate_managed_scope(
        &self,
        expected: &native_guardian::NativeGuardianDrainBinding,
    ) -> io::Result<()> {
        if !self.running_published
            || self.guarded_generation_id() != Some(expected.generation.as_str())
            || self.owner != expected.worker
            || self.guardian.as_ref() != Some(&expected.guardian)
            || self.generation.parent != expected.parent
            || std::process::id() != expected.parent.pid
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed grant names another receiver",
            ));
        }
        native_guardian::validate_managed_guardian_scope(&self.record, expected)
    }

    pub(crate) fn prepare_managed_request(&mut self, arm: &ManagedRequestDeadline) -> io::Result<()> {
        self.deadline_exchange(
            arm,
            "prepare_hard_deadline",
            "hard_deadline_prepared",
            None,
            Some(arm.expires_ns),
        )
    }

    pub(crate) fn arm_managed_request(&mut self, arm: &ManagedRequestDeadline, command: Option<u64>) -> io::Result<()> {
        let op = if command.is_some() {
            "arm_fenced_hard_deadline"
        } else {
            "arm_hard_deadline"
        };
        self.arm_deadline(arm, op, command)
    }

    pub(crate) fn arm_warm_request(&mut self, arm: &ManagedRequestDeadline, command: u64) -> io::Result<()> {
        self.arm_deadline(arm, "arm_warm_hard_deadline", Some(command))
    }

    fn arm_deadline(&mut self, arm: &ManagedRequestDeadline, op: &str, command: Option<u64>) -> io::Result<()> {
        self.deadline_exchange(arm, op, "hard_deadline_armed", command, Some(arm.expires_ns))
    }

    pub(crate) fn finish_managed_request(&mut self, arm: &ManagedRequestDeadline, command: u64) -> io::Result<()> {
        self.deadline_exchange(
            arm,
            "finish_hard_deadline",
            "hard_deadline_finished",
            Some(command),
            None,
        )
    }

    fn deadline_exchange(
        &mut self,
        arm: &ManagedRequestDeadline,
        op: &str,
        state: &str,
        command: Option<u64>,
        expires_ns: Option<u64>,
    ) -> io::Result<()> {
        if !self.guarded || !self.running_published {
            return Err(io::Error::other(
                "managed request requires an acknowledged independent guardian",
            ));
        }
        let base = Instant::now();
        let until = base
            .checked_add(arm.remaining()?.min(Duration::from_secs(2)))
            .ok_or_else(|| io::Error::other("invalid managed ACK deadline"))?;
        let mut binding = serde_json::Map::from_iter([
            ("protocol".to_owned(), serde_json::json!(native_guardian::PROTOCOL)),
            ("nonce".to_owned(), serde_json::json!(self.nonce)),
            ("attempt".to_owned(), serde_json::json!(arm.attempt)),
            ("stage".to_owned(), serde_json::json!(arm.stage)),
        ]);
        if let Some(command) = command {
            binding.insert("command".to_owned(), serde_json::json!(command));
        }
        if let Some(expires_ns) = expires_ns {
            binding.insert("expires_ns".to_owned(), serde_json::json!(expires_ns));
        }
        let mut request = binding.clone();
        request.insert("op".to_owned(), serde_json::json!(op));
        binding.insert("state".to_owned(), serde_json::json!(state));
        let expected = serde_json::Value::Object(binding);
        let mut bytes = serde_json::to_vec(&request).map_err(io::Error::other)?;
        bytes.push(b'\n');
        write_before_deadline(&mut self.stream, &bytes, until)?;
        let reply: serde_json::Value =
            serde_json::from_str(&read_startup_hello(&mut self.stream, until)?).map_err(io::Error::other)?;
        if reply != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "managed guardian deadline ACK differs",
            ));
        }
        arm.remaining().map(|_| ())
    }

    pub(crate) fn request_stop(&self) -> io::Result<()> {
        self.stream.shutdown(std::net::Shutdown::Both)
    }
    pub(crate) fn uses_guardian(&self) -> bool {
        self.guarded
    }
    pub(crate) fn guardian_alive(&self) -> io::Result<bool> {
        match &self.guardian {
            Some(guardian) => guardian.is_live(),
            None => Ok(!self.guarded),
        }
    }
    pub(crate) fn group_absent(&self) -> io::Result<bool> {
        lean_toolchain::process_group::group_present(self.owner.process_group).map(|present| !present)
    }
    pub(crate) fn retirement_closed(&self) -> io::Result<bool> {
        if self.uses_guardian() {
            self.custody_closed()
        } else {
            self.group_absent()
        }
    }

    fn custody_closed(&self) -> io::Result<bool> {
        if !self.group_absent()? {
            return Ok(false);
        }
        match &self.guardian {
            Some(guardian) => guardian.is_live().map(|live| !live),
            None => Ok(false),
        }
    }
    pub(crate) fn remove_if_absent(&self) -> io::Result<()> {
        if !self.group_absent()? {
            return Err(io::Error::other("owned process group has not stopped"));
        }
        // Keep guarded records parseable through failed publication cleanup.
        // The next bounded registry admission collects this closed generation
        // under its mutex, including all known protocol temporary files.
        if self.guarded {
            return Ok(());
        }
        match fs::remove_file(&self.record) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(directory) = self.record.parent() {
            File::open(directory)?.sync_all()?;
        }
        Ok(())
    }
}
impl Drop for ParentLifetime {
    fn drop(&mut self) {
        let _socket = fs::remove_file(self.socket_directory.join("parent.sock"));
        let _socket_directory = fs::remove_dir(&self.socket_directory);
        if self.guarded && !self.dispatched {
            let record = self.directory.join("owner.json");
            if fs::symlink_metadata(&record)
                .ok()
                .filter(|metadata| {
                    metadata.is_file()
                        && metadata.nlink() == 1
                        && metadata.len() <= native_guardian::MAX_OWNER_BYTES as u64
                })
                .and_then(|_| {
                    let mut bytes = Vec::new();
                    File::open(&record)
                        .ok()?
                        .take((native_guardian::MAX_OWNER_BYTES as u64).saturating_add(1))
                        .read_to_end(&mut bytes)
                        .ok()?;
                    (bytes.len() <= native_guardian::MAX_OWNER_BYTES).then_some(bytes)
                })
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|value| {
                    value.get("phase").and_then(serde_json::Value::as_str) == Some("preparing")
                        && value.get("generation_id").and_then(serde_json::Value::as_str) == Some(&self.nonce)
                })
            {
                let _cleanup = (|| -> io::Result<()> {
                    // Persist undispatched retirement before bounded cleanup.
                    // A partial deletion must not pin the one utility slot to
                    // this still-live parent until process exit.
                    native_guardian::retire_preparing_owner_record(&record, &self.nonce)?;
                    native_guardian::cleanup_generation_inputs(&self.directory)?;
                    for name in ["owner.write", "owner.guardian-write"] {
                        match fs::remove_file(self.directory.join(name)) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                    }
                    fs::remove_file(record)?;
                    fs::remove_dir(&self.directory)
                })();
            }
        }
    }
}
impl Drop for ParentLifetimeLease {
    fn drop(&mut self) {
        let _shutdown = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "isolated OS identity tests assert their own process")]
mod tests {
    use super::*;

    #[test]
    fn failed_bootstrap_publication_preserves_dispatching() {
        let root = std::env::temp_dir().join(format!("lean-publication-failure-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut lifetime = ParentLifetime::prepare_guarded(&root, Duration::from_secs(1)).unwrap();
        lifetime.dispatch().unwrap();
        let record = lifetime.directory.join("owner.json");
        let original = std::fs::read(&record).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        // Synthetic serialized identities only: no process is launched or signalled.
        let mut worker = LeanWorkerOwnedProcess::capture(std::process::id()).unwrap().unwrap();
        worker.pid = 1234;
        worker.session = 1234;
        worker.process_group = 1234;
        let mut guardian = worker.clone();
        guardian.pid = 1235;
        guardian.process_group = 1235;
        let object = value.as_object_mut().unwrap();
        object.insert("phase".into(), serde_json::json!("bootstrap"));
        object.insert("process".into(), serde_json::to_value(worker).unwrap());
        object.insert("guardian".into(), serde_json::to_value(guardian).unwrap());
        std::fs::write(lifetime.directory.join("owner.write"), b"collision").unwrap();
        let error =
            native_guardian::publish_startup_owner(&record, &value, "dispatching", Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&record).unwrap(), original);
        drop(lifetime);
        assert!(record.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backpressured_ack_obeys_original_deadline() {
        let (mut sender, _receiver) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(25);
        let error = write_before_deadline(&mut sender, &vec![0; 4 * 1024 * 1024], deadline).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            write_before_deadline(&mut sender, b"ACK", deadline).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn dispatched_preparing_drop_keeps_exact_reservation() {
        let root = std::env::temp_dir().join(format!("lean-dispatch-drop-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut lifetime = ParentLifetime::prepare_guarded(&root, Duration::from_secs(1)).unwrap();
        let record = lifetime.directory.join("owner.json");
        lifetime.dispatch().unwrap();
        let custody = lifetime.custody.clone();
        drop(lifetime);
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        assert_eq!(value.get("phase"), Some(&serde_json::json!("dispatching")));
        assert!(!custody.closed().unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_spawn_error_retires_only_dispatching() {
        let root = std::env::temp_dir().join(format!("lean-no-spawn-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut lifetime = ParentLifetime::prepare_guarded(&root, Duration::from_secs(1)).unwrap();
        assert!(lifetime.no_spawn().is_err());
        lifetime.dispatch().unwrap();
        lifetime.no_spawn().unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(lifetime.directory.join("owner.json")).unwrap()).unwrap();
        assert_eq!(value.get("phase"), Some(&serde_json::json!("preparing_closed")));
        drop(lifetime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scoped_deadline_veto_after_durable_dispatch_retires_only_when_published() -> io::Result<()> {
        use crate::process::{CheckedSpawnError, LeanWorkerProcessTracker};
        use crate::session::{LeanWorkerCancellationToken, check_cancelled};
        use crate::supervisor::LeanWorkerError;

        struct TestRoot(PathBuf);
        impl Drop for TestRoot {
            fn drop(&mut self) {
                // The test owns this private registry and never launches a payload.
                drop(fs::remove_dir_all(&self.0));
            }
        }

        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .ok_or_else(|| io::Error::other("worker-parent workspace root missing"))?
            .join("target")
            .canonicalize()?;
        // A long checkout path can hold the registry but not a Unix socket.
        // Use the same private IPC base as the shared-reader tests.
        let socket_root = std::env::var_os("LEAN_RS_TEST_IPC_BASE").map(PathBuf::from);
        for collide in [false, true] {
            let name = format!("og{}{}", std::process::id(), usize::from(collide));
            let path = target.join(name);
            fs::DirBuilder::new().mode(0o700).create(&path)?;
            let root = TestRoot(path);
            let registry = root.0.join("registry");
            let slot = native_guardian::NativeCapacitySlot {
                key: "one-actor".to_owned(),
                scope: "main".to_owned(),
                limit: 1,
            };
            let mut lifetime = ParentLifetime::prepare_with_socket_directory(
                &registry,
                Some(socket_root.as_deref().unwrap_or(&root.0)),
                true,
                Duration::from_secs(1),
                Some(&slot),
            )?;
            let record = lifetime.directory.join("owner.json");
            let nonce = lifetime.nonce.clone();
            let marker = root.0.join("payload-started");
            let mut command = Command::new("/bin/sh");
            command.args(["-c", r#"printf started > "$1""#, "--"]).arg(&marker);
            let tracker = LeanWorkerProcessTracker::new();
            // Use a real expired monotonic deadline directly at the inner
            // gate so the caller's earlier check cannot skip this boundary.
            let expiry = LeanWorkerCancellationToken::new().with_deadline(Instant::now());
            let failure = tracker
                .spawn_guarded_checked(&mut command, &mut lifetime, || {
                    let durable: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
                    assert_eq!(
                        durable.get("generation_id").and_then(serde_json::Value::as_str),
                        Some(nonce.as_str())
                    );
                    assert_eq!(
                        durable.get("phase").and_then(serde_json::Value::as_str),
                        Some("dispatching")
                    );
                    if collide {
                        drop(
                            OpenOptions::new()
                                .create_new(true)
                                .write(true)
                                .open(record.with_extension("guardian-write"))
                                .unwrap(),
                        );
                    }
                    check_cancelled("scoped_test_launch", Some(&expiry))
                })
                .unwrap_err();
            let CheckedSpawnError::Refused {
                reason,
                retirement_error,
            } = failure
            else {
                return Err(io::Error::other(format!("expected the in-owner veto: {failure:?}")));
            };
            assert!(matches!(
                reason,
                LeanWorkerError::Timeout { operation: "scoped_test_launch", resource, .. } if !resource.work_entered_child
            ));
            assert_eq!(
                retirement_error.as_ref().map(io::Error::kind),
                collide.then_some(io::ErrorKind::AlreadyExists)
            );
            assert_eq!(tracker.active_processes(), 0);
            assert!(!marker.exists());
            drop(lifetime);
            let durable: serde_json::Value = serde_json::from_slice(&fs::read(&record)?)?;
            let phase = if collide { "dispatching" } else { "preparing_closed" };
            assert_eq!(durable.get("phase").and_then(serde_json::Value::as_str), Some(phase));
            let next = native_guardian::reserve_owner_record_with_slot(
                &registry,
                &"1".repeat(48),
                Duration::from_secs(1),
                Some(&slot),
            );
            if collide {
                assert_eq!(next.unwrap_err().kind(), io::ErrorKind::WouldBlock);
                assert!(record.exists(), "the unresolved dispatch must remain fenced");
            } else {
                assert!(next?.is_file());
                assert!(!record.exists(), "the closed reservation is collectable");
            }
        }
        Ok(())
    }

    #[test]
    fn exact_birth_capture_never_accepts_a_forged_incarnation() {
        let actual = LeanWorkerOwnedProcess::capture(std::process::id()).unwrap().unwrap();
        assert!(!actual.start.is_empty());
        let mut forged = actual.clone();
        forged.start.push_str("-different");
        assert_ne!(actual, forged);
        assert_eq!(forged.observe().unwrap(), LeanWorkerOwnerObservation::Unresolved);
    }

    #[test]
    fn malformed_session_descriptor_never_proves_absence() {
        let actual = LeanWorkerOwnedProcess::capture(std::process::id()).unwrap().unwrap();
        let mut forged = actual;
        forged.version = 99;
        assert_eq!(forged.observe().unwrap(), LeanWorkerOwnerObservation::Unresolved);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "isolated lifecycle tests own every child and journal"
)]
#[allow(
    unsafe_code,
    reason = "isolated pre-body fixtures establish their own session and exit only themselves"
)]
#[path = "crash_ownership/tests.rs"]
mod lifecycle_tests;
