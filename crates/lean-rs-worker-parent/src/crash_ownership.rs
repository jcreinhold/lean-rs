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
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt as _, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
pub(crate) const SOCKET_ENV: &str = "LEAN_RS_WORKER_PARENT_SOCKET";
pub(crate) const NONCE_ENV: &str = "LEAN_RS_WORKER_PARENT_NONCE";
fn running() -> String {
    "running".to_owned()
}
/// Durable evidence for one generation, published before runtime startup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanWorkerOwnedGeneration {
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

/// Link-free lifetime admission for an external native process group.
///
/// The controlled bootstrap must establish a private session, start the selected
/// guardian and wait for the durable ACK before spawning any native payload.
/// This facade adds no worker body protocol and never replays a command.
#[derive(Debug)]
pub struct LeanWorkerNativeScope(ParentLifetime);

/// Exact admitted group and its parent-to-guardian cleanup capability.
#[derive(Debug)]
pub struct LeanWorkerNativeScopeLease(ParentLifetimeLease);

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

    /// Bind the controlled bootstrap to this exact private lifetime.
    pub fn configure(&self, command: &mut Command) {
        self.0.configure(command);
    }

    /// Authenticate the actual bootstrap child and persist its owner record.
    /// Native payload startup remains forbidden until `acknowledge` succeeds.
    ///
    /// # Errors
    /// Refuses changed process birth, wrong session or guardian and late startup.
    pub fn accept(self, child_pid: u32, timeout: Duration) -> io::Result<LeanWorkerNativeScopeLease> {
        self.0.accept(child_pid, timeout).map(LeanWorkerNativeScopeLease)
    }
}

impl LeanWorkerNativeScopeLease {
    /// Release only this admitted bootstrap after its durable owner publication.
    ///
    /// # Errors
    /// Returns a broken or timed-out private guardian channel.
    pub fn acknowledge(&mut self) -> io::Result<()> {
        self.0.acknowledge()
    }

    /// Request cleanup from the sole guardian, without signalling a stored PID.
    ///
    /// # Errors
    /// Returns a failed shutdown of the private lifetime channel.
    pub fn request_stop(&self) -> io::Result<()> {
        self.0.request_stop()
    }

    /// Observe closure of the exact inherited payload group.
    ///
    /// # Errors
    /// Failed kernel observation remains an error, never a closure assertion.
    pub fn group_absent(&self) -> io::Result<bool> {
        self.0.group_absent()
    }

    /// Observe the exact guardian identity authenticated at admission.
    ///
    /// # Errors
    /// Failed process observations are not a guardian-death assertion.
    pub fn guardian_alive(&self) -> io::Result<bool> {
        self.0.guardian_alive()
    }

    /// Observe group closure and release of the guardian's read descriptors.
    ///
    /// # Errors
    /// Failed kernel observations never establish custody release.
    pub fn custody_closed(&self) -> io::Result<bool> {
        if !self.0.group_absent()? {
            return Ok(false);
        }
        match &self.0.guardian {
            Some(guardian) => guardian.is_live().map(|live| !live),
            None => Ok(false),
        }
    }

    /// Kernel identity authenticated for this actual bootstrap child.
    pub fn owner(&self) -> &LeanWorkerOwnedProcess {
        &self.0.owner
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
}
#[derive(Debug)]
pub(crate) struct ParentLifetimeLease {
    stream: UnixStream,
    pub(crate) owner: LeanWorkerOwnedProcess,
    guardian: Option<LeanWorkerOwnedProcess>,
    pub(crate) record: PathBuf,
    nonce: String,
    guarded: bool,
}
impl ParentLifetime {
    pub(crate) fn prepare(root: &Path) -> io::Result<Self> {
        Self::prepare_inner(root, false, Duration::from_secs(2), None)
    }
    #[cfg(test)]
    pub(crate) fn prepare_guarded(root: &Path, timeout: Duration) -> io::Result<Self> {
        Self::prepare_inner(root, true, timeout, None)
    }
    pub(crate) fn prepare_guarded_with_slot(
        root: &Path,
        timeout: Duration,
        slot: Option<&native_guardian::NativeCapacitySlot>,
    ) -> io::Result<Self> {
        Self::prepare_inner(root, true, timeout, slot)
    }
    fn prepare_inner(
        root: &Path,
        guarded: bool,
        timeout: Duration,
        slot: Option<&native_guardian::NativeCapacitySlot>,
    ) -> io::Result<Self> {
        let started = Instant::now();
        if guarded {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        } else {
            fs::create_dir_all(root)?;
        }
        let mut random = [0_u8; 24];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let mut nonce = String::with_capacity(48);
        for byte in random {
            write!(nonce, "{byte:02x}").map_err(io::Error::other)?;
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
            fs::create_dir(&directory)?;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
            directory
        };
        let socket_directory = PathBuf::from("/tmp").join(format!("lean-worker-{}", &nonce[..24]));
        let initialized = (|| {
            fs::create_dir(&socket_directory)?;
            fs::set_permissions(&socket_directory, fs::Permissions::from_mode(0o700))?;
            let listener = UnixListener::bind(socket_directory.join("parent.sock"))?;
            listener.set_nonblocking(true)?;
            Ok(Self {
                listener,
                directory: directory.clone(),
                socket_directory: socket_directory.clone(),
                nonce,
                guarded,
                capacity_slot: slot.cloned(),
            })
        })();
        if initialized.is_err() && guarded {
            let _record = fs::remove_file(directory.join("owner.json"));
            let _directory = fs::remove_dir(&directory);
            let _socket = fs::remove_file(socket_directory.join("parent.sock"));
            let _socket_directory = fs::remove_dir(socket_directory);
        }
        initialized
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
    pub(crate) fn accept(self, child_pid: u32, timeout: Duration) -> io::Result<ParentLifetimeLease> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "worker startup deadline elapsed"))?;
        let (stream, _) = loop {
            match self.listener.accept() {
                Ok(pair) => break pair,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        };
        stream.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))?;
        let mut line = String::new();
        BufReader::new(&stream).take(16384).read_line(&mut line)?;
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
            phase: running(),
            guardian,
            protected_domains: Vec::new(),
            capacity_slot: self.capacity_slot.clone(),
        };
        let record = self.directory.join("owner.json");
        let temporary = self.directory.join("owner.write");
        let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
        file.write_all(&serde_json::to_vec(&generation).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(temporary, &record)?;
        File::open(&self.directory)?.sync_all()?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))?;
        fs::remove_file(self.socket_directory.join("parent.sock"))?;
        fs::remove_dir(&self.socket_directory)?;
        Ok(ParentLifetimeLease {
            stream,
            owner,
            guardian: generation.guardian,
            record,
            nonce: self.nonce.clone(),
            guarded: self.guarded,
        })
    }
}
impl ParentLifetimeLease {
    pub(crate) fn acknowledge(&mut self) -> io::Result<()> {
        if self.guarded {
            let mut bytes = serde_json::to_vec(
                &serde_json::json!({"state":"durable-owner-ready","nonce":self.nonce,"record":self.record}),
            )
            .map_err(io::Error::other)?;
            bytes.push(b'\n');
            self.stream.write_all(&bytes)
        } else {
            self.stream.write_all(b"durable-owner-ready\n")
        }
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
        if self.guarded {
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
