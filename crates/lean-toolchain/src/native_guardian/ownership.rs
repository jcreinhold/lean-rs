//! Shared exact identities and bounded owner-record admission.
use super::drain_slot::{self, Slot, State as SlotState};
use super::{MAX_OWNER_RECORDS, POLL, deadline, expired, field, invalid, persist_owner, read_owner, valid_nonce};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(target_os = "linux")]
use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

/// Kernel birth and scope observations; never a right to signal a serialized PID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProcessIdentity {
    pub version: u32,
    pub pid: u32,
    pub start: String,
    pub session: u32,
    pub process_group: u32,
}
/// Passive observation of one previously owned inherited group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeOwnerObservation {
    Absent,
    Present,
    Unresolved,
}
impl NativeProcessIdentity {
    /// Capture birth twice around scalar scope queries.
    /// # Errors
    /// Returns unavailable or changing kernel observations.
    pub fn capture(pid: u32) -> io::Result<Option<Self>> {
        let Some((start, _)) = native::snapshot(pid)? else {
            return Ok(None);
        };
        let (session, process_group) = native::scope(pid)?;
        if native::snapshot(pid)?.as_ref().map(|(start, _)| start) != Some(&start) {
            return Err(invalid("process birth changed during capture"));
        }
        Ok(Some(Self {
            version: 1,
            pid,
            start,
            session,
            process_group,
        }))
    }
    /// A mismatched birth or missing leader with group members stays unresolved.
    /// # Errors
    /// Returns failed group or process observations.
    pub fn observe(&self) -> io::Result<NativeOwnerObservation> {
        if self.version != 1
            || self.start.is_empty()
            || self.start.len() > 4096
            || self.pid <= 1
            || i32::try_from(self.pid).is_err()
            || self.pid != self.session
            || self.pid != self.process_group
        {
            return Ok(NativeOwnerObservation::Unresolved);
        }
        if !crate::process_group::group_present(self.process_group)? {
            return Ok(NativeOwnerObservation::Absent);
        }
        Ok(if Self::capture(self.pid)?.as_ref() == Some(self) {
            NativeOwnerObservation::Present
        } else {
            NativeOwnerObservation::Unresolved
        })
    }
    /// Check this exact process remains non-zombie.
    /// # Errors
    /// Failed queries remain errors, never an absence claim.
    pub fn is_live(&self) -> io::Result<bool> {
        if self.version != 1 || self.start.is_empty() {
            return Err(invalid("invalid process identity"));
        }
        Ok(native::snapshot(self.pid)?.is_some_and(|(start, live)| start == self.start && live))
    }
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            clippy::unused_self,
            reason = "exact stopped-state observation is only supported on macOS"
        )
    )]
    fn is_stopped(&self) -> io::Result<bool> {
        #[cfg(target_os = "macos")]
        {
            Ok(native::bsd_info(self.pid)?
                .is_some_and(|info| native::bsd_start(&info) == self.start && info.pbi_status == libc::SSTOP))
        }
        #[cfg(not(target_os = "macos"))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exact stopped state unavailable",
        ))
    }
}
static REGISTRY_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

/// Bind the process to one caller-selected owner directory before native work.
///
/// A repeated binding of the same path is allowed. Every cooperating worker
/// and writer must use this directory. Binding is read-only.
///
/// # Errors
/// Rejects an invalid directory, a different explicit environment setting,
/// or a different binding already fixed by this function or by lookup.
pub fn bind_registry_directory(root: &Path) -> io::Result<()> {
    if std::env::var_os(super::OWNERS_DIRECTORY_ENV).is_some_and(|value| value != root.as_os_str()) {
        return Err(invalid(format!(
            "native owner directory conflicts with {}",
            super::OWNERS_DIRECTORY_ENV
        )));
    }
    validate_registry_directory(root)?;
    let bound = REGISTRY_DIRECTORY.get_or_init(|| root.to_path_buf());
    if bound.as_os_str() != root.as_os_str() {
        return Err(invalid(format!(
            "native owner directory is already bound to {}",
            bound.display()
        )));
    }
    Ok(())
}

/// Resolve the process's owner directory without creating it.
///
/// The first successful lookup fixes the process's binding. Without an explicit
/// binding or environment selection, it uses the SDK's per-user data directory.
///
/// # Errors
/// Returns an unavailable local-data location or invalid directory identity.
pub fn registry_directory() -> io::Result<PathBuf> {
    if let Some(root) = REGISTRY_DIRECTORY.get() {
        validate_registry_directory(root)?;
        return Ok(root.clone());
    }
    let root = match std::env::var_os(super::OWNERS_DIRECTORY_ENV) {
        Some(root) => PathBuf::from(root),
        None => dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("local data directory unavailable"))?
            .join("lean-rs")
            .join("native-generations"),
    };
    validate_registry_directory(&root)?;
    let bound = REGISTRY_DIRECTORY.get_or_init(|| root);
    validate_registry_directory(bound)?;
    Ok(bound.clone())
}

/// Check a caller-selected owner directory without binding or creating it.
///
/// For a prospective directory, the closest existing ancestor must be canonical.
///
/// # Errors
/// Rejects an invalid or aliased path or an existing directory that is not private
/// and owned by the current user.
pub fn validate_registry_directory(root: &Path) -> io::Result<()> {
    if !root.is_absolute()
        || root.as_os_str().len() > 4096
        || root
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(invalid("native namespace must be a bounded absolute path"));
    }
    // Lookup is read-only. Only reservation creates the namespace. For a fresh
    // namespace validate the existing ancestor, without resolving a guessed
    // helper path or permitting an aliased ownership scope.
    match fs::symlink_metadata(root) {
        Ok(_) => validate_registry(root)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut ancestor = root.parent().ok_or_else(|| invalid("native namespace parent absent"))?;
            loop {
                match fs::symlink_metadata(ancestor) {
                    Ok(metadata) => {
                        if !metadata.is_dir() || ancestor.canonicalize()? != ancestor {
                            return Err(invalid("native namespace ancestor must be canonical"));
                        }
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        ancestor = ancestor
                            .parent()
                            .ok_or_else(|| invalid("native namespace ancestor absent"))?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Err(error) => return Err(error),
    }
    Ok(())
}
pub(super) fn validate_registry(root: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !root.is_absolute()
        || root.canonicalize()? != root
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid("native namespace must be canonical and private"));
    }
    Ok(())
}
fn checked_identity(value: &Value) -> io::Result<NativeProcessIdentity> {
    let identity: NativeProcessIdentity = serde_json::from_value(value.clone()).map_err(io::Error::other)?;
    if identity.version != 1
        || identity.start.is_empty()
        || identity.start.len() > 4096
        || [identity.pid, identity.session, identity.process_group]
            .iter()
            .any(|pid| *pid == 0 || i32::try_from(*pid).is_err())
    {
        return Err(invalid("invalid native process identity"));
    }
    Ok(identity)
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryClaim {
    boot: String,
    claimant: NativeProcessIdentity,
}

pub(super) fn validate_boot_uuid(boot: &str) -> io::Result<()> {
    if boot.len() == 36
        && boot.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        Ok(())
    } else {
        Err(invalid("invalid native boot session UUID"))
    }
}

/// The current kernel boot session, checked as a UUID. Tests use it to build
/// owner records for this boot.
#[cfg(test)]
pub(crate) fn current_boot_session() -> io::Result<String> {
    let boot = native::boot_session()?;
    validate_boot_uuid(&boot)?;
    Ok(boot)
}

fn recovery_claim(value: &Value) -> io::Result<RecoveryClaim> {
    let claim: RecoveryClaim =
        serde_json::from_value(field(value, "recovery_claim").clone()).map_err(io::Error::other)?;
    validate_boot_uuid(&claim.boot)?;
    checked_identity(field(field(value, "recovery_claim"), "claimant"))?;
    Ok(claim)
}

pub(super) fn validate_owner_header(value: &Value) -> io::Result<()> {
    let version = field(value, "version")
        .as_u64()
        .ok_or_else(|| invalid("native owner version missing"))?;
    if !matches!(version, 1..=4)
        || field(value, "scope").as_str() != Some(format!("inherited-process-group-v{version}").as_str())
        || !field(value, "generation_id").as_str().is_some_and(valid_nonce)
    {
        return Err(invalid("invalid native owner header"));
    }
    if value.as_object().is_none_or(|record| {
        record.keys().any(|key| {
            !matches!(
                key.as_str(),
                "version"
                    | "scope"
                    | "phase"
                    | "generation_id"
                    | "parent"
                    | "process"
                    | "guardian"
                    | "protected_domains"
                    | "planned_read_domains"
                    | "capacity_slot"
                    | "recovery_claim"
                    | "original_worker_boot"
                    | "guardian_drain_protocol"
                    | "guardian_drain"
                    | "guardian_zombie_closure"
            )
        })
    }) {
        return Err(invalid("unknown native owner field"));
    }
    checked_identity(field(value, "parent"))?;
    if let Some(slot) = value.get("capacity_slot").filter(|value| !value.is_null()) {
        if version == 1 {
            return Err(invalid("legacy native owner cannot bind an actor slot"));
        }
        serde_json::from_value::<NativeCapacitySlot>(slot.clone())
            .map_err(io::Error::other)?
            .validate()?;
    }
    let phase = field(value, "phase");
    let original_boot = value.get("original_worker_boot");
    let claim = value.get("recovery_claim");
    if let Some(drain) = value.get("guardian_drain")
        && (version < 3
            || phase != "running"
            || original_boot.is_none()
            || !matches!(drain.as_str(), Some("available" | "requested")))
    {
        return Err(invalid("invalid original guardian drain state"));
    }
    if let Some(protocol) = value.get("guardian_drain_protocol")
        && (protocol != SLOT_PROTOCOL || value.get("guardian_drain").is_none())
    {
        return Err(invalid("invalid guardian drain protocol"));
    }
    if let Some(closed) = value.get("guardian_zombie_closure")
        && (closed != true || field(value, "guardian_drain") != "requested")
    {
        return Err(invalid("invalid original guardian closure evidence"));
    }
    if version >= 3 {
        if phase != "running" || (original_boot.is_none() && claim.is_none()) {
            return Err(invalid(
                "private native owner must be running and carry recovery evidence",
            ));
        }
        if let Some(boot) = original_boot {
            validate_boot_uuid(boot.as_str().ok_or_else(|| invalid("invalid original worker boot"))?)?;
        }
        if claim.is_some() {
            recovery_claim(value)?;
        }
    } else if original_boot.is_some() || claim.is_some() {
        return Err(invalid("native recovery evidence requires owner version 3 or later"));
    }
    if version == 4 {
        if original_boot.is_none()
            || field(value, "protected_domains")
                .as_array()
                .is_none_or(|domains| !domains.is_empty())
        {
            return Err(invalid(
                "planned native reads require boot-bound owner without acquired domains",
            ));
        }
        let plans: Vec<super::PlannedReadDomain> =
            serde_json::from_value(field(value, "planned_read_domains").clone()).map_err(io::Error::other)?;
        if plans.is_empty()
            || plans.len() > super::MAX_READ_DOMAINS
            || !plans
                .iter()
                .zip(plans.iter().skip(1))
                .all(|(a, b)| a.lock_path < b.lock_path)
            || plans
                .iter()
                .any(|plan| !plan.project_root.is_absolute() || !plan.lock_path.is_absolute() || plan.inode == 0)
        {
            return Err(invalid("invalid planned native read domains"));
        }
    } else if value.get("planned_read_domains").is_some() {
        return Err(invalid("planned native reads require owner version 4"));
    }
    if phase == "preparing" || phase == "preparing_closed" || phase == "dispatching" {
        if version != 2
            || !field(value, "process").is_null()
            || !field(value, "guardian").is_null()
            || field(value, "protected_domains")
                .as_array()
                .is_none_or(|domains| !domains.is_empty())
        {
            return Err(invalid("invalid preparing native owner"));
        }
        return Ok(());
    }
    if phase != "running" && phase != "bootstrap" && phase != "bootstrap_closed" && !(version == 1 && phase.is_null()) {
        return Err(invalid("unknown native owner phase"));
    }
    if matches!(phase.as_str(), Some("bootstrap" | "bootstrap_closed")) && version != 2 {
        return Err(invalid("bootstrap requires guarded owner"));
    }
    let process = checked_identity(field(value, "process"))?;
    if process.pid <= 1 || process.pid != process.session || process.pid != process.process_group {
        return Err(invalid("invalid native owner scope"));
    }
    if version != 1 {
        let guardian = checked_identity(field(value, "guardian"))?;
        let pins_root_session = guardian.session == process.pid;
        let has_own_group = guardian.process_group == guardian.pid && guardian.pid != process.pid;
        if !pins_root_session || !has_own_group {
            return Err(invalid("invalid native guardian scope"));
        }
    }
    if version != 1 || value.get("protected_domains").is_some() {
        let domains: Vec<super::NativeReadDomain> =
            serde_json::from_value(field(value, "protected_domains").clone()).map_err(io::Error::other)?;
        if domains.len() > super::MAX_READ_DOMAINS {
            return Err(invalid("native owner domain union exceeds limit"));
        }
        let mut seen = BTreeSet::new();
        for domain in domains {
            if domain.epoch.is_empty()
                || domain.epoch.len() > 4096
                || !domain.project_root.is_absolute()
                || !domain.lock_path.is_absolute()
                || !seen.insert(domain.lock_path)
            {
                return Err(invalid("invalid native protected domain structure"));
            }
        }
    }
    Ok(())
}
pub(super) fn payload_closed(value: &Value) -> io::Result<bool> {
    validate_owner_header(value)?;
    if field(value, "phase") == "preparing_closed" {
        return Ok(true);
    }
    if field(value, "phase") == "preparing" {
        return checked_identity(field(value, "parent"))?.is_live().map(|live| !live);
    }
    if field(value, "phase") == "bootstrap_closed" {
        return checked_identity(field(value, "guardian"))?.is_live().map(|v| !v);
    }
    if matches!(field(value, "phase").as_str(), Some("dispatching" | "bootstrap")) {
        return Ok(false); // Pre-Running custody requires a durable guardian closure receipt.
    }
    let process = checked_identity(field(value, "process"))?;
    #[cfg(target_os = "macos")]
    if field(value, "guardian_zombie_closure") == true {
        // A live original guardian still owns descriptor custody.
        // The receipt records an irreversible sole-zombie observation, so
        // closure need not wait for the stopped service to reap that child.
        verify_original_worker_boot(value)?;
        return checked_identity(field(value, "guardian"))?.is_live().map(|live| !live);
    }
    let absent = !crate::process_group::group_present(process.process_group)?;
    Ok(absent)
}
// Record retention includes the descriptor custodian, independently of compute
// occupancy. A closed payload may leave a live guardian holding artifact loans.
pub(super) fn record_can_be_removed(value: &Value) -> io::Result<bool> {
    if !payload_closed(value)? {
        return Ok(false);
    }
    if field(value, "version") != 1
        && matches!(
            field(value, "phase").as_str(),
            Some("running" | "bootstrap" | "bootstrap_closed")
        )
    {
        return checked_identity(field(value, "guardian"))?.is_live().map(|live| !live);
    }
    Ok(true)
}
/// Retire only this caller's exact undispatched Preparing reservation.
///
/// Dispatch publication precedes every possible spawn. A retired
/// Preparing record therefore cannot have spawned a child; keeping
/// it parseable permits bounded input cleanup retries while this caller lives.
/// # Errors
/// Refuses another owner, a running generation or failed durable publication.
#[doc(hidden)]
pub fn retire_preparing_owner_record(record: &Path, nonce: &str) -> io::Result<()> {
    let mut value = read_owner(record)?;
    validate_owner_header(&value)?;
    let parent =
        NativeProcessIdentity::capture(std::process::id())?.ok_or_else(|| invalid("native parent identity absent"))?;
    if field(&value, "generation_id") != nonce || checked_identity(field(&value, "parent"))? != parent {
        return Err(invalid("preparing retirement belongs to another exact owner"));
    }
    if field(&value, "phase") == "preparing_closed" {
        return Ok(());
    }
    if field(&value, "phase") != "preparing" {
        return Err(invalid("only undispatched preparation can retire"));
    }
    value
        .as_object_mut()
        .ok_or_else(|| invalid("native owner object missing"))?
        .insert("phase".to_owned(), json!("preparing_closed"));
    persist_owner(record, &value)
}

/// Serialize a parent startup publication against admission and guardian closure.
/// # Errors
/// Refuses a changed generation/parent/state or any uncertain durable write.
#[doc(hidden)]
pub fn publish_startup_owner(record: &Path, value: &Value, expected: &str, timeout: Duration) -> io::Result<()> {
    let _lock = lock_publication(record, timeout)?;
    let current = read_owner(record)?;
    validate_owner_header(&current)?;
    validate_owner_header(value)?;
    let parent = NativeProcessIdentity::capture(std::process::id())?.ok_or_else(|| invalid("parent absent"))?;
    if !matches!(
        (expected, field(value, "phase").as_str()),
        ("dispatching", Some("bootstrap")) | ("bootstrap", Some("running"))
    ) {
        return Err(invalid("invalid startup phase transition"));
    }
    if field(&current, "phase") != expected
        || field(&current, "generation_id") != field(value, "generation_id")
        || checked_identity(field(&current, "parent"))? != parent
        || field(&current, "parent") != field(value, "parent")
    {
        return Err(invalid("startup publication no longer owns this phase"));
    }
    if expected == "dispatching" {
        return super::persist_owner_at(record, value, "write");
    }

    let worker = checked_identity(field(&current, "process"))?;
    if field(value, "version") != 2
        || field(value, "process") != field(&current, "process")
        || field(value, "guardian") != field(&current, "guardian")
        || NativeProcessIdentity::capture(worker.pid)?.as_ref() != Some(&worker)
        || !worker.is_live()?
    {
        return Err(invalid("native startup worker identity changed before Running"));
    }
    let boot = match native::boot_session() {
        Ok(boot) => boot,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound | io::ErrorKind::Unsupported
            ) =>
        {
            // Ordinary startup can proceed without future recovery authority.
            return super::persist_owner_at(record, value, "write");
        }
        Err(error) => return Err(error),
    };
    validate_boot_uuid(&boot)?;
    if NativeProcessIdentity::capture(worker.pid)?.as_ref() != Some(&worker) || !worker.is_live()? {
        return Err(invalid("native startup worker changed during boot observation"));
    }
    let mut running = value.clone();
    let object = running
        .as_object_mut()
        .ok_or_else(|| invalid("native owner object missing"))?;
    object.insert("version".to_owned(), json!(3));
    object.insert("scope".to_owned(), json!("inherited-process-group-v3"));
    object.insert("original_worker_boot".to_owned(), json!(boot));
    validate_owner_header(&running)?;
    super::persist_owner_at(record, &running, "write")
}

pub(super) fn verify_original_worker_boot(value: &Value) -> io::Result<()> {
    validate_owner_header(value)?;
    let original = field(value, "original_worker_boot")
        .as_str()
        .ok_or_else(|| invalid("original worker boot was never witnessed"))?;
    if !original.eq_ignore_ascii_case(&native::boot_session()?) {
        return Err(invalid("original worker belongs to another boot"));
    }
    Ok(())
}
pub(super) fn lock_publication(record: &Path, timeout: Duration) -> io::Result<File> {
    let root = record
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("owner registry missing"))?;
    validate_registry(root)?;
    lock_admission(root, deadline(timeout)?, false)
}

pub(super) fn try_lock_publication(record: &Path) -> io::Result<File> {
    let root = record
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("owner registry missing"))?;
    validate_registry(root)?;
    let mutex = open_admission_at(&File::open(root)?, false)?;
    mutex.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => busy("native publication lock is busy"),
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(mutex)
}

/// Exact original scope expected by a managed replacement requester.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeGuardianDrainBinding {
    pub registry_device: u64,
    pub registry_inode: u64,
    pub generation: String,
    pub boot: String,
    pub parent: NativeProcessIdentity,
    pub worker: NativeProcessIdentity,
    pub guardian: NativeProcessIdentity,
}

const SLOT_PROTOCOL: &str = "slot-v1";

/// Generation-level status of the original guardian's one drain slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeGuardianDrainStatus {
    Empty,
    Pending,
    Refused,
    Accepted,
}

fn slot_protocol(value: &Value) -> bool {
    field(value, "guardian_drain_protocol") == SLOT_PROTOCOL
}

fn require_legacy_drain_advertisement(value: &Value) -> io::Result<()> {
    if value.get("guardian_drain_protocol").is_some()
        || !matches!(field(value, "guardian_drain").as_str(), Some("available" | "requested"))
    {
        return Err(invalid("original guardian did not advertise legacy drain"));
    }
    validate_owner_header(value)
}

pub(super) fn matches_original_guardian_binding(
    value: &Value,
    expected: &NativeGuardianDrainBinding,
) -> io::Result<()> {
    verify_original_worker_boot(value)?;
    if field(value, "generation_id") != expected.generation.as_str()
        || !field(value, "original_worker_boot")
            .as_str()
            .is_some_and(|boot| boot.eq_ignore_ascii_case(&expected.boot))
        || checked_identity(field(value, "parent"))? != expected.parent
        || checked_identity(field(value, "process"))? != expected.worker
        || checked_identity(field(value, "guardian"))? != expected.guardian
    {
        return Err(invalid("original guardian drain binding changed"));
    }
    Ok(())
}

fn read_bound_owner_at(generation: &File, record: &Path) -> io::Result<Value> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::io::Read as _;

    let open = || {
        openat(
            generation,
            "owner.json",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
    };
    let mut file = open()?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.nlink() != 1 || opened.len() > super::MAX_OWNER_BYTES as u64 {
        return Err(invalid("native drain owner record identity or size invalid"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take((super::MAX_OWNER_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    let named = open()?.metadata()?;
    if (opened.dev(), opened.ino()) != (named.dev(), named.ino())
        || named.nlink() != 1
        || bytes.len() > super::MAX_OWNER_BYTES
    {
        return Err(busy("native drain owner record changed during bounded read"));
    }
    super::validate_owner_record_path(record, serde_json::from_slice(&bytes).map_err(io::Error::other)?)
}

fn drain_slot(registry: &Path, expected: &NativeGuardianDrainBinding) -> io::Result<(Value, Slot)> {
    if !valid_nonce(&expected.generation) {
        return Err(invalid("invalid native recovery generation"));
    }
    validate_boot_uuid(&expected.boot)?;
    validate_registry(registry)?;
    let namespace = File::from(rustix::fs::open(
        registry,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    let root = namespace.metadata()?;
    if (root.dev(), root.ino()) != (expected.registry_device, expected.registry_inode) {
        return Err(invalid("original guardian registry identity changed"));
    }
    let name = format!("worker-{}", expected.generation);
    let generation = File::from(rustix::fs::openat(
        &namespace,
        name.as_str(),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    let directory = registry.join(name);
    validate_registry(&directory)?;
    let bound = generation.metadata()?;
    let current = fs::symlink_metadata(&directory)?;
    if (bound.dev(), bound.ino()) != (current.dev(), current.ino()) {
        return Err(invalid("original guardian generation directory changed"));
    }
    let value = read_bound_owner_at(&generation, &directory.join("owner.json"))?;
    matches_original_guardian_binding(&value, expected)?;
    if !slot_protocol(&value) {
        return Err(invalid("original guardian did not advertise the slot drain protocol"));
    }
    let slot = Slot::open(&generation)?.ok_or_else(|| invalid("original guardian drain slot unavailable"))?;
    let current = fs::symlink_metadata(&directory)?;
    if (bound.dev(), bound.ino()) != (current.dev(), current.ino()) {
        return Err(invalid("original guardian generation directory changed"));
    }
    Ok((value, slot))
}

/// Observe closure of the exact retained guardian generation under admission.
///
/// This observes native custody only. A missing record supplies no proof.
///
/// # Errors
/// Refuses a missing or changed binding, an unavailable observation, or an
/// admission lock that cannot be acquired within the caller's budget.
#[doc(hidden)]
pub fn observe_original_guardian_closure(
    registry: &Path,
    expected: &NativeGuardianDrainBinding,
    timeout: Duration,
) -> io::Result<bool> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let until = deadline(timeout)?;
    if !valid_nonce(&expected.generation) {
        return Err(invalid("invalid native recovery generation"));
    }
    validate_boot_uuid(&expected.boot)?;
    validate_registry(registry)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let namespace = File::from(open(registry, flags, Mode::empty())?);
    let _lock = lock_admission_at(&namespace, until, false)?;
    let check_namespace = || {
        validate_registry(registry)?;
        let current = fs::symlink_metadata(registry)?;
        let opened = namespace.metadata()?;
        if (current.dev(), current.ino()) != (expected.registry_device, expected.registry_inode)
            || (opened.dev(), opened.ino()) != (current.dev(), current.ino())
        {
            return Err(invalid("original guardian registry identity changed"));
        }
        Ok(())
    };
    check_namespace()?;
    let name = format!("worker-{}", expected.generation);
    let generation = File::from(openat(&namespace, name.as_str(), flags, Mode::empty())?);
    let directory = registry.join(name);
    validate_registry(&directory)?;
    let value = read_bound_owner_at(&generation, &directory.join("owner.json"))?;
    matches_original_guardian_binding(&value, expected)?;
    if field(&value, "phase") != "running" {
        return Err(invalid(
            "original guardian closure requires the retained running record",
        ));
    }
    let closed = record_can_be_removed(&value)?;
    check_namespace()?;
    let current = fs::symlink_metadata(directory)?;
    let retained = generation.metadata()?;
    if (current.dev(), current.ino()) != (retained.dev(), retained.ino()) {
        return Err(invalid("original guardian generation directory changed"));
    }
    Ok(closed)
}

/// Offer drain to the original guardian without acquiring admission ownership.
/// Success means the exact generation's offer was inserted or coalesced.
/// # Errors
/// Refuses missing, mismatched, legacy or unavailable custody.
#[doc(hidden)]
pub fn submit_original_guardian_drain(registry: &Path, expected: &NativeGuardianDrainBinding) -> io::Result<()> {
    let (value, slot) = drain_slot(registry, expected)?;
    if field(&value, "guardian_drain") == "requested" || slot.state()? == SlotState::Accepted {
        return Ok(());
    }
    if !expected.guardian.is_live()? {
        return Err(busy("original guardian is unavailable"));
    }
    slot.offer()?;
    #[cfg(all(test, target_os = "macos"))]
    super::tests::pause_slot_sender()?;
    Ok(())
}

/// Observe an exact generation's offer independently of submission.
/// Acceptance confirms the guardian's durable decision, never custody closure.
/// # Errors
/// Missing or inconsistent evidence leaves the observation unknown.
#[doc(hidden)]
pub fn observe_original_guardian_drain(
    registry: &Path,
    expected: &NativeGuardianDrainBinding,
) -> io::Result<NativeGuardianDrainStatus> {
    let (value, slot) = drain_slot(registry, expected)?;
    match slot.state()? {
        SlotState::Accepted => {
            // The guardian may have published its decision after our first read.
            let (current, _) = drain_slot(registry, expected)?;
            if field(&current, "guardian_drain") != "requested" {
                return Err(invalid("guardian acceptance marker has no matching decision"));
            }
            Ok(NativeGuardianDrainStatus::Accepted)
        }
        _ if field(&value, "guardian_drain") == "requested" => Ok(NativeGuardianDrainStatus::Pending),
        SlotState::Pending => Ok(NativeGuardianDrainStatus::Pending),
        SlotState::Refused => Ok(NativeGuardianDrainStatus::Refused),
        SlotState::Empty => Ok(NativeGuardianDrainStatus::Empty),
    }
}

/// Ask one original guardian to drain an exact generation in the expected registry.
///
/// The registry and authenticated owner binding are checked under the same
/// admission lock as publication. Success means durable delivery only. The
/// original custodian still owns stop and closure, and only while supported
/// by the existing stopped-parent protocol.
///
/// # Errors
/// Refuses a changed binding, a responsive or absent parent, an unavailable
/// guardian, or indeterminate validation or publication.
#[doc(hidden)]
pub fn request_bound_original_guardian_drain(
    registry: &Path,
    expected: &NativeGuardianDrainBinding,
    timeout: Duration,
) -> io::Result<()> {
    let until = deadline(timeout)?;
    if !valid_nonce(&expected.generation) {
        return Err(invalid("invalid native recovery generation"));
    }
    validate_boot_uuid(&expected.boot)?;
    let root = File::from(rustix::fs::open(
        registry,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    let record = registry.join(format!("worker-{}/owner.json", expected.generation));
    require_legacy_drain_advertisement(&read_owner(&record)?)?;
    let _lock = lock_admission_at(&root, until, false)?;
    let matches_registry = |metadata: &fs::Metadata| {
        (metadata.dev(), metadata.ino()) == (expected.registry_device, expected.registry_inode)
    };
    let check_registry = || {
        validate_registry(registry)?;
        if !matches_registry(&root.metadata()?) || !matches_registry(&fs::symlink_metadata(registry)?) {
            return Err(invalid("original guardian registry identity changed"));
        }
        Ok(())
    };
    check_registry()?;
    let generation_dir = registry.join(format!("worker-{}", expected.generation));
    validate_registry(&generation_dir)?;
    let mut value = read_owner(&record)?;
    require_legacy_drain_advertisement(&value)?;
    verify_original_worker_boot(&value)?;
    if field(&value, "generation_id") != expected.generation.as_str()
        || !field(&value, "original_worker_boot")
            .as_str()
            .is_some_and(|boot| boot.eq_ignore_ascii_case(&expected.boot))
        || checked_identity(field(&value, "parent"))? != expected.parent
        || checked_identity(field(&value, "guardian"))? != expected.guardian
    {
        return Err(invalid("original guardian drain binding changed"));
    }
    check_guardian_drain_worker(&value, &expected.worker)?;
    check_guardian_drain_eligibility(&value)?;
    check_registry()?;
    publish_guardian_drain_request(&record, &mut value)
}

/// Ask the original live guardian to drain a scope whose service is stopped.
///
/// This is a request to the existing custodian. Success confirms only durable
/// delivery to the registry. An armed stage relies on its existing guardian
/// deadline or lifeline. The reader/writer gate still observes closure.
/// The supplied record and worker must come from a trusted managed scope.
///
/// # Errors
/// Refuses unavailable or changed authority, including responsive services and
/// guardians that did not advertise this path during startup.
#[doc(hidden)]
pub fn request_original_guardian_drain(
    record: &Path,
    worker: &NativeProcessIdentity,
    timeout: Duration,
) -> io::Result<()> {
    require_legacy_drain_advertisement(&read_owner(record)?)?;
    let _lock = lock_publication(record, timeout)?;
    validate_registry(
        record
            .parent()
            .ok_or_else(|| invalid("native owner directory missing"))?,
    )?;
    let mut value = read_owner(record)?;
    require_legacy_drain_advertisement(&value)?;
    verify_original_worker_boot(&value)?;
    check_guardian_drain_worker(&value, worker)?;
    check_guardian_drain_eligibility(&value)?;
    publish_guardian_drain_request(record, &mut value)
}

fn check_guardian_drain_worker(value: &Value, worker: &NativeProcessIdentity) -> io::Result<()> {
    if checked_identity(field(value, "process"))? != *worker
        || NativeProcessIdentity::capture(worker.pid)?.as_ref() != Some(worker)
        || !matches!(field(value, "guardian_drain").as_str(), Some("available" | "requested"))
    {
        return Err(invalid("original guardian drain binding changed or unavailable"));
    }
    Ok(())
}

pub(super) fn publish_guardian_drain_request(record: &Path, value: &mut Value) -> io::Result<()> {
    if field(value, "guardian_drain") == "requested" {
        return super::sync_owner_directory_for(record, "recovery-write");
    }
    value
        .as_object_mut()
        .ok_or_else(|| invalid("native owner object missing"))?
        .insert("guardian_drain".to_owned(), json!("requested"));
    persist_recovery_owner(record, value)
}

fn check_guardian_drain_eligibility(value: &Value) -> io::Result<()> {
    let parent = checked_identity(field(value, "parent"))?;
    let guardian = checked_identity(field(value, "guardian"))?;
    if !parent.is_stopped()? || !guardian.is_live()? || guardian.is_stopped()? {
        return Err(busy("original service is not stopped or guardian is unavailable"));
    }
    Ok(())
}

pub(super) fn publish_guardian_drain_support(
    record: &Path,
    worker: &NativeProcessIdentity,
    guardian: &NativeProcessIdentity,
    use_slot: bool,
) -> io::Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let _lock = lock_publication(record, Duration::from_secs(2))?;
    let mut value = read_owner(record)?;
    validate_owner_header(&value)?;
    if value.get("original_worker_boot").is_none() {
        return Ok(false);
    }
    verify_original_worker_boot(&value)?;
    if checked_identity(field(&value, "process"))? != *worker
        || checked_identity(field(&value, "guardian"))? != *guardian
        || guardian.pid != std::process::id()
        || field(&value, "phase") != "running"
        || value.get("guardian_drain").is_some()
    {
        return Err(invalid("original guardian drain advertisement differs"));
    }
    value
        .as_object_mut()
        .ok_or_else(|| invalid("native owner object missing"))?
        .insert("guardian_drain".to_owned(), json!("available"));
    if use_slot {
        let directory = record.parent().ok_or_else(|| invalid("owner directory missing"))?;
        validate_registry(directory)?;
        Slot::create(&File::open(directory)?)?;
        value
            .as_object_mut()
            .ok_or_else(|| invalid("native owner object missing"))?
            .insert("guardian_drain_protocol".to_owned(), json!(SLOT_PROTOCOL));
    }
    persist_owner(record, &value)?;
    Ok(true)
}

pub(super) fn consume_guardian_drain_slot(
    record: &Path,
    guardian: &NativeProcessIdentity,
    sampled_stopped: &mut bool,
) -> io::Result<bool> {
    let directory = record.parent().ok_or_else(|| invalid("owner directory missing"))?;
    let generation = File::open(directory)?;
    let slot = Slot::open(&generation)?.ok_or_else(|| invalid("original guardian drain slot unavailable"))?;
    if !*sampled_stopped && slot.state()? != SlotState::Pending {
        return Ok(false);
    }
    let registry = directory.parent().ok_or_else(|| invalid("owner registry missing"))?;
    let _lock = try_lock_admission_at(&File::open(registry)?)?;
    let mut value = read_owner(record)?;
    verify_original_worker_boot(&value)?;
    if !slot_protocol(&value)
        || checked_identity(field(&value, "guardian"))? != *guardian
        || guardian.pid != std::process::id()
    {
        return Err(invalid("original guardian slot owner differs"));
    }
    if !*sampled_stopped {
        if slot.state()? != SlotState::Pending {
            return Ok(false);
        }
        // If an earlier refusal renamed successfully but failed to sync, finish
        // its durability work before deciding the freshly inserted offer.
        slot.sync()?;
        let parent = checked_identity(field(&value, "parent"))?;
        let worker = checked_identity(field(&value, "process"))?;
        if !parent.is_stopped()? || NativeProcessIdentity::capture(worker.pid)?.as_ref() != Some(&worker) {
            slot.refuse()?;
            return Ok(false);
        }
        *sampled_stopped = true;
    }
    publish_guardian_drain_request(record, &mut value)?;
    // This marker witnesses the already synced decision. Failure may obscure
    // observation, but must not inhibit the original guardian's drain.
    let _observation = slot.mark_accepted();
    Ok(true)
}

pub(super) fn original_guardian_drain_requested(record: &Path, guardian: &NativeProcessIdentity) -> io::Result<bool> {
    let value = read_owner(record)?;
    validate_owner_header(&value)?;
    require_legacy_drain_advertisement(&value)?;
    if checked_identity(field(&value, "guardian"))? != *guardian || guardian.pid != std::process::id() {
        return Err(invalid("original guardian drain owner differs"));
    }
    if field(&value, "guardian_drain") != "requested" {
        return Ok(false);
    }
    verify_original_worker_boot(&value)?;
    // Admission checked the stopped service under the publication lock.
    // Like an explicit stop, the committed request remains in effect if
    // that service later resumes. The guardian alone drains its own scope.
    Ok(true)
}

#[cfg(target_os = "macos")]
pub(super) fn matches_zombie_birth(worker: &NativeProcessIdentity) -> io::Result<bool> {
    Ok(native::bsd_info_with_zombies(worker.pid, true)?.is_some_and(|info| {
        info.pbi_status == libc::SZOMB
            && info.pbi_pgid == worker.process_group
            && native::bsd_start(&info) == worker.start
    }))
}

#[cfg(target_os = "macos")]
pub(super) fn publish_guardian_zombie_closure(
    record: &Path,
    worker: &NativeProcessIdentity,
    guardian: &NativeProcessIdentity,
    autonomous_expiry: bool,
) -> io::Result<bool> {
    if autonomous_expiry
        && (!matches_zombie_birth(worker)? || !crate::process_group::observed_group_singleton(worker.pid)?)
    {
        // Do not wait on registry publication before sending the first stop.
        return Ok(false);
    }
    let _lock = lock_publication(record, Duration::from_secs(2))?;
    let mut value = read_owner(record)?;
    verify_original_worker_boot(&value)?;
    if field(&value, "phase") != "running"
        || (!autonomous_expiry && field(&value, "guardian_drain") != "requested")
        || checked_identity(field(&value, "process"))? != *worker
        || checked_identity(field(&value, "guardian"))? != *guardian
        || guardian.pid != std::process::id()
    {
        return Err(invalid("original guardian closure scope differs"));
    }
    super::native::validate_pin(worker.pid)?;
    if field(&value, "guardian_zombie_closure") == true {
        super::sync_owner_directory_for(record, "closure-write")?;
        return Ok(true);
    }
    // The pin excludes numeric group recycling. Matching zombie birth on
    // both sides of a complete singleton census rules out a surviving leaf
    // and the original worker cannot run or fork again.
    if !matches_zombie_birth(worker)?
        || !crate::process_group::observed_group_singleton(worker.pid)?
        || !matches_zombie_birth(worker)?
    {
        return Ok(false);
    }
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("native owner object missing"))?;
    // An autonomous decision is published together with the exact closure evidence.
    object.insert("guardian_drain".to_owned(), json!("requested"));
    object.insert("guardian_zombie_closure".to_owned(), json!(true));
    super::persist_closure_owner(record, &value)?;
    Ok(true)
}

/// Publish the first recovery intent for an exact running worker after loss
/// of the original service and its guardian. This arbitrates intent only.
/// It conveys no authority to signal, take custody or reuse the worker slot.
/// A later attempt can replace only an exact non-live claimant from the same
/// boot, after rechecking the original worker anchor under the admission lock.
/// The UUID identifies the claimant's boot. The first claim on a version 2
/// record cannot establish the original worker's boot. Any later operation
/// that can signal or reclaim custody must verify that binding independently.
///
/// # Errors
/// Returns WouldBlock for a live owner or claimant. Refuses changed scopes,
/// cross-boot claims and unavailable observations or failed publication.
#[doc(hidden)]
pub fn claim_native_recovery(record: &Path, expected: &NativeProcessIdentity, timeout: Duration) -> io::Result<()> {
    let _lock = lock_publication(record, timeout)?;
    validate_registry(
        record
            .parent()
            .ok_or_else(|| invalid("native owner directory missing"))?,
    )?;
    let mut value = read_owner(record)?;
    validate_owner_header(&value)?;
    if field(&value, "version") == 1
        || field(&value, "phase") != "running"
        || checked_identity(field(&value, "process"))? != *expected
    {
        return Err(invalid("native recovery scope changed"));
    }
    let boot = native::boot_session()?;
    validate_boot_uuid(&boot)?;
    if let Some(original) = field(&value, "original_worker_boot").as_str()
        && !original.eq_ignore_ascii_case(&boot)
    {
        return Err(invalid("original worker belongs to another boot"));
    }
    if value.get("recovery_claim").is_some() {
        let prior = recovery_claim(&value)?;
        if !prior.boot.eq_ignore_ascii_case(&boot) {
            return Err(invalid("native recovery claim belongs to another boot"));
        }
        if prior.claimant.is_live()? {
            return Err(busy("native recovery claim still owns this scope"));
        }
    }
    if checked_identity(field(&value, "parent"))?.is_live()?
        || checked_identity(field(&value, "guardian"))?.is_live()?
    {
        return Err(busy("original native recovery owner is live"));
    }
    if NativeProcessIdentity::capture(expected.pid)?.as_ref() != Some(expected) || !expected.is_live()? {
        return Err(invalid("native recovery payload anchor is unavailable"));
    }
    let claimant =
        NativeProcessIdentity::capture(std::process::id())?.ok_or_else(|| invalid("native recoverer absent"))?;
    let legacy = field(&value, "version") == 2;
    let owner = value
        .as_object_mut()
        .ok_or_else(|| invalid("native owner object missing"))?;
    if legacy {
        owner.insert("version".to_owned(), json!(3));
        owner.insert("scope".to_owned(), json!("inherited-process-group-v3"));
    }
    owner.insert("recovery_claim".to_owned(), json!(RecoveryClaim { boot, claimant }));
    persist_recovery_owner(record, &value)
}

fn persist_recovery_owner(record: &Path, value: &Value) -> io::Result<()> {
    // An eligible retry has the sole publication lock. Only owner.json is
    // committed evidence. A prior staging file cannot grant claim authority.
    let staged = record.with_extension("recovery-write");
    match fs::symlink_metadata(&staged) {
        Ok(metadata)
            if metadata.is_file()
                && metadata.nlink() == 1
                && metadata.uid() == rustix::process::getuid().as_raw()
                && metadata.len() <= super::MAX_OWNER_BYTES as u64 =>
        {
            fs::remove_file(staged)?;
        }
        Ok(_) => return Err(invalid("invalid native recovery staging file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    super::persist_owner_at(record, value, "recovery-write")
}

/// Mark possible dispatch durably before invoking the spawn syscall.
/// # Errors
/// Refuses non-preparing state, another owner, or uncertain publication.
#[doc(hidden)]
pub fn dispatch_owner_record(record: &Path, nonce: &str) -> io::Result<()> {
    transition_preparing(record, nonce, "preparing", "dispatching")
}
/// Retire dispatch only when the sole launch owner proves no child was started:
/// a synchronous veto before the spawn call, or a spawn error without a child handle.
/// # Errors
/// Refuses a changed owner/state or failed durable publication.
#[doc(hidden)]
pub fn retire_unspawned_owner_record(record: &Path, nonce: &str) -> io::Result<()> {
    transition_preparing(record, nonce, "dispatching", "preparing_closed")
}
/// Close this caller's dispatched generation once it has reaped the direct
/// child, provided it never sent the ACK, and report whether custody ended.
///
/// The ACK follows durable Running. Without it the guardian holds no read
/// loans and the worker never started Lean, so the record fences nothing and
/// its parent may close it. A guardian that published `bootstrap_closed` first
/// keeps its receipt; custody then ends when that guardian exits. A missing
/// record was collected, which only happens to a closed one.
/// # Errors
/// Refuses another owner, a phase past bootstrap, or an uncertain write. A
/// busy publication lock returns `WouldBlock`, so a poller tries again.
#[doc(hidden)]
pub fn close_unacknowledged_owner_record(record: &Path, nonce: &str) -> io::Result<bool> {
    let _lock = try_lock_publication(record)?;
    let mut value = match read_owner(record) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    validate_owner_header(&value)?;
    let parent =
        NativeProcessIdentity::capture(std::process::id())?.ok_or_else(|| invalid("native parent identity absent"))?;
    if field(&value, "generation_id") != nonce || checked_identity(field(&value, "parent"))? != parent {
        return Err(invalid("unacknowledged closure belongs to another exact owner"));
    }
    match field(&value, "phase").as_str() {
        Some("preparing_closed") => Ok(true),
        Some("bootstrap_closed") => payload_closed(&value),
        Some("dispatching") => {
            value
                .as_object_mut()
                .ok_or_else(|| invalid("native owner object missing"))?
                .insert("phase".to_owned(), json!("preparing_closed"));
            persist_owner(record, &value)?;
            Ok(true)
        }
        _ => Err(invalid("only an unacknowledged dispatch closes after a direct reap")),
    }
}
fn transition_preparing(record: &Path, nonce: &str, from: &str, to: &str) -> io::Result<()> {
    let mut value = read_owner(record)?;
    validate_owner_header(&value)?;
    let parent =
        NativeProcessIdentity::capture(std::process::id())?.ok_or_else(|| invalid("native parent identity absent"))?;
    if field(&value, "generation_id") != nonce
        || checked_identity(field(&value, "parent"))? != parent
        || field(&value, "phase") != from
    {
        return Err(invalid("native dispatch transition differs from exact owner/state"));
    }
    value
        .as_object_mut()
        .ok_or_else(|| invalid("native owner object missing"))?
        .insert("phase".to_owned(), json!(to));
    persist_owner(record, &value)
}

// Called only under .admission.lock. A native spawn is permitted only AFTER
// initial Preparing publication commits owner.json. Missing committed ownership
// therefore means pre-spawn publication, or GC of an already closed group.
// An existing but malformed owner.json is never eligible for this recovery.
fn collect_unpublished(directory: &Path) -> io::Result<()> {
    if !directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("worker-"))
        .is_some_and(valid_nonce)
    {
        // This also refuses a 0.10 `.writer-storage-v1` charge directory.
        return Err(invalid(format!(
            "unrecognized native registry directory {}",
            directory.display()
        )));
    }
    if drain_slot::exists(&File::open(directory)?)? {
        return Err(busy("ownerless guardian drain slot is unresolved"));
    }
    let mut temporary = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if temporary.len() >= 2
            || !matches!(entry.file_name().to_str(), Some("owner.write" | "owner.guardian-write"))
            || !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > super::MAX_OWNER_BYTES as u64
        {
            return Err(invalid("unrecognized unpublished native record contents"));
        }
        temporary.push(entry.path());
    }
    for path in temporary {
        fs::remove_file(path)?;
    }
    fs::remove_dir(directory)
}
/// Resolve an initial publication interrupted before native spawn. A writer may
/// not classify an in-flight reservation; a busy mutex is explicit backpressure.
pub(super) fn read_or_collect_unpublished(root: &Path, directory: &Path) -> io::Result<Option<Value>> {
    let mutex = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".admission.lock"))?;
    match mutex.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(busy("native owner publication in progress")),
        Err(std::fs::TryLockError::Error(error)) => return Err(error),
    }
    match read_owner(&directory.join("owner.json")) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A concurrent closed-record collector may already have removed it.
            match fs::symlink_metadata(directory) {
                Ok(_) => collect_unpublished(directory)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
/// An actor owns at most one main generation and one semantic companion.
///
/// One global preparation utility may coexist without consuming an actor key.
/// The exact parent incarnation binds an actor key while any group remains.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCapacitySlot {
    pub key: String,
    pub scope: String,
    pub limit: usize,
}
impl NativeCapacitySlot {
    fn validate(&self) -> io::Result<()> {
        if self.key.is_empty()
            || self.key.len() > 256
            || !matches!(self.scope.as_str(), "main" | "semantic" | "preparation")
            || self.limit == 0
            || self.limit > MAX_OWNER_RECORDS
        {
            return Err(invalid("invalid native actor capacity slot"));
        }
        Ok(())
    }
}
fn busy(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, message)
}
struct OccupiedSlot {
    parent: NativeProcessIdentity,
    scopes: BTreeSet<String>,
}
fn admit_slot(
    values: &[Value],
    requested: Option<&NativeCapacitySlot>,
    parent: &NativeProcessIdentity,
) -> io::Result<()> {
    let mut slots: BTreeMap<String, OccupiedSlot> = BTreeMap::new();
    let mut preparation_key: Option<String> = None;
    for value in values {
        // A retained guardian record still counts toward the namespace bound,
        // but a confirmed-closed payload no longer consumes actor capacity.
        if payload_closed(value)? {
            continue;
        }
        let recorded = match value.get("capacity_slot").filter(|value| !value.is_null()) {
            Some(value) => Some(serde_json::from_value::<NativeCapacitySlot>(value.clone()).map_err(io::Error::other)?),
            None => None,
        };
        let (Some(requested), Some(recorded)) = (requested, recorded.as_ref()) else {
            if requested.is_some() || recorded.is_some() {
                return Err(busy(
                    "live unscoped native generations prevent actor capacity admission",
                ));
            }
            continue;
        };
        recorded.validate()?;
        if recorded.limit != requested.limit {
            return Err(busy("occupied native actor capacity uses a different limit"));
        }
        if recorded.scope == "preparation" {
            if preparation_key.replace(recorded.key.clone()).is_some() {
                return Err(invalid("conflicting native preparation ownership"));
            }
            continue;
        }
        let recorded_parent: NativeProcessIdentity =
            serde_json::from_value(field(value, "parent").clone()).map_err(io::Error::other)?;
        let slot = slots.entry(recorded.key.clone()).or_insert_with(|| OccupiedSlot {
            parent: recorded_parent.clone(),
            scopes: BTreeSet::new(),
        });
        if slot.parent != recorded_parent || !slot.scopes.insert(recorded.scope.clone()) {
            return Err(invalid("conflicting native actor ownership"));
        }
    }
    if preparation_key.as_ref().is_some_and(|key| slots.contains_key(key)) {
        return Err(invalid("preparation and resident native actor keys overlap"));
    }
    if let Some(requested) = requested {
        if requested.scope == "preparation" {
            if preparation_key.is_some() {
                return Err(busy("native preparation capacity occupied"));
            }
            if slots.contains_key(&requested.key) {
                return Err(busy("preparation key belongs to a resident native actor"));
            }
            return Ok(());
        }
        if preparation_key.as_ref() == Some(&requested.key) {
            return Err(busy("native actor key belongs to a preparation utility"));
        }
        if let Some(slot) = slots.get(&requested.key) {
            if slot.parent != *parent {
                return Err(busy("native actor key belongs to another parent incarnation"));
            }
            if slot.scopes.contains(&requested.scope) {
                return Err(busy("native actor scope still occupied"));
            }
        } else if slots.len() >= requested.limit {
            return Err(busy("native actor capacity occupied"));
        }
    }
    Ok(())
}
/// Reserve an unscoped owner record, retaining the finite namespace backstop.
/// # Errors
/// Returns busy/capacity, malformed ownership or failed durable-write errors.
pub fn reserve_owner_record(root: &Path, nonce: &str, timeout: Duration) -> io::Result<PathBuf> {
    reserve_owner_record_with_slot(root, nonce, timeout, None)
}
fn collect_records_locked(root: &Path, until: std::time::Instant) -> io::Result<Vec<Value>> {
    let mut occupied = Vec::new();
    let mut seen = 0_usize;
    for entry in fs::read_dir(root)? {
        if std::time::Instant::now() >= until {
            return Err(expired());
        }
        let entry = entry?;
        if entry.file_name() == ".admission.lock" {
            continue;
        }
        seen = seen
            .checked_add(1)
            .ok_or_else(|| invalid("native owner count overflow"))?;
        if seen > MAX_OWNER_RECORDS {
            return Err(invalid("native namespace exceeds owner limit"));
        }
        if !entry.file_type()?.is_dir() {
            return Err(invalid("unexpected native namespace entry"));
        }
        let record = entry.path().join("owner.json");
        let value = match read_owner(&record) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                collect_unpublished(&entry.path())?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if record_can_be_removed(&value)? {
            // While the stopped broker cannot reap its terminal worker, the
            // existing receipt is the only attributable proof of closure.
            let retain_receipt = field(&value, "guardian_zombie_closure") == true
                && crate::process_group::group_present(checked_identity(field(&value, "process"))?.process_group)?;
            // Keep the committed record parseable through partial asset cleanup.
            let directory = File::open(entry.path())?;
            if slot_protocol(&value) || drain_slot::exists(&directory)? {
                loop {
                    match Slot::collect_closed(&directory) {
                        Ok(()) => break,
                        Err(error) if error.raw_os_error() == Some(libc::ENOTEMPTY) => {
                            if std::time::Instant::now() >= until {
                                return Err(busy("late guardian drain offer interrupted collection"));
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            super::cleanup_generation_inputs(&entry.path())?;
            for name in [
                "owner.write",
                "owner.guardian-write",
                "owner.closure-write",
                "owner.recovery-write",
            ] {
                match fs::remove_file(entry.path().join(name)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            if retain_receipt {
                continue;
            }
            fs::remove_file(record)?;
            fs::remove_dir(entry.path())?;
        } else {
            occupied.push(value);
        }
    }
    Ok(occupied)
}
fn lock_admission(root: &Path, until: std::time::Instant, create: bool) -> io::Result<File> {
    lock_admission_at(&File::open(root)?, until, create)
}
pub(super) fn lock_admission_at(root: &File, until: std::time::Instant, create: bool) -> io::Result<File> {
    let mutex = open_admission_at(root, create)?;
    loop {
        if std::time::Instant::now() >= until {
            return Err(expired());
        }
        match mutex.try_lock() {
            Ok(()) => return Ok(mutex),
            Err(std::fs::TryLockError::WouldBlock) => thread::sleep(POLL),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}
fn open_admission_at(root: &File, create: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let existing = || openat(root, ".admission.lock", flags, Mode::empty());
    let mutex = File::from(match existing() {
        Err(rustix::io::Errno::NOENT) if create => {
            // Concurrent first reservations share the winning inode. Never
            // replace or truncate a published mutex.
            match openat(
                root,
                ".admission.lock",
                flags | OFlags::CREATE | OFlags::EXCL,
                Mode::RUSR | Mode::WUSR,
            ) {
                Err(rustix::io::Errno::EXIST) => existing()?,
                result => result?,
            }
        }
        result => result?,
    });
    let metadata = mutex.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(invalid("native admission lock must be an owned regular file"));
    }
    Ok(mutex)
}

pub(super) fn try_lock_admission_at(root: &File) -> io::Result<File> {
    let mutex = open_admission_at(root, false)?;
    match mutex.try_lock() {
        Ok(()) => Ok(mutex),
        Err(std::fs::TryLockError::WouldBlock) => Err(busy("native admission lock is busy")),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}
/// Collect only owner records whose existing custody predicate proves closure.
///
/// This acquires the same admission lock and runs the same bounded collector as
/// generation reservation, but never creates a generation or signals a process.
/// # Errors
/// Returns busy while any record remains occupied, or propagates malformed,
/// unknown, timed-out or failed durable-cleanup observations.
#[doc(hidden)]
pub fn collect_closed_owner_records(root: &Path, timeout: Duration) -> io::Result<()> {
    let until = deadline(timeout)?;
    validate_registry(root)?;
    let _mutex = lock_admission(root, until, false)?;
    check_drained_locked(root, until)
}

pub(super) fn check_drained_locked(root: &Path, until: std::time::Instant) -> io::Result<()> {
    let occupied = collect_records_locked(root, until)?;
    File::open(root)?.sync_all()?;
    if !occupied.is_empty() {
        return Err(busy("native generation custody remains open"));
    }
    Ok(())
}
/// Reserve bounded generation and actor capacity under the same admission lock.
///
/// Live groups keep their actor slot after server death. A successful passive
/// whole-group absence observation releases capacity, never a proof outcome.
/// A still-live guardian retains its recovery record without occupying that slot.
/// Preparing records certify no possible spawn and may be collected after their
/// exact parent dies. Dispatching records remain fenced without durable identity.
/// Bootstrap records require group and guardian closure; Running preserves the
/// separate payload-only compute occupancy policy.
/// # Errors
/// Returns bounded backpressure, invalid ownership, or durable-write errors.
pub fn reserve_owner_record_with_slot(
    root: &Path,
    nonce: &str,
    timeout: Duration,
    slot: Option<&NativeCapacitySlot>,
) -> io::Result<PathBuf> {
    let until = deadline(timeout)?;
    if !valid_nonce(nonce) {
        return Err(invalid("native owner nonce invalid"));
    }
    if let Some(slot) = slot {
        slot.validate()?;
    }
    let parent =
        NativeProcessIdentity::capture(std::process::id())?.ok_or_else(|| invalid("native parent identity absent"))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?;
    validate_registry(root)?;
    let _mutex = lock_admission(root, until, true)?;
    let occupied = collect_records_locked(root, until)?;
    if occupied.len() >= MAX_OWNER_RECORDS {
        return Err(busy("native generation capacity occupied"));
    }
    admit_slot(&occupied, slot, &parent)?;
    if std::time::Instant::now() >= until {
        return Err(expired());
    }
    let directory = root.join(format!("worker-{nonce}"));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let record = directory.join("owner.json");
    persist_owner(
        &record,
        &json!({"version":2,"scope":"inherited-process-group-v2","phase":"preparing","generation_id":nonce,"parent":parent,"process":null,"guardian":null,"protected_domains":[],"capacity_slot":slot}),
    )?;
    File::open(root)?.sync_all()?;
    Ok(record)
}
/// Validate a bounded serialized identity without reading arbitrary-size files.
#[cfg(target_os = "linux")]
pub(super) fn read_process(path: &Path) -> io::Result<Option<(String, bool)>> {
    let mut bytes = String::new();
    match File::open(path) {
        Ok(file) => {
            file.take(u64::try_from(super::MAX_OWNER_BYTES).map_err(io::Error::other)?)
                .read_to_string(&mut bytes)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let rest = bytes.rsplit_once(')').ok_or_else(|| invalid("invalid process stat"))?.1;
    let state = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| invalid("missing process state"))?;
    let start = rest
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| invalid("missing process birth"))?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    Ok(Some((
        format!("linux:{}:{start}", boot.trim()),
        state != "Z" && state != "X",
    )))
}
mod native {
    #![allow(
        unsafe_code,
        reason = "narrow scalar kernel identity queries with checked output storage"
    )]
    #[cfg(not(target_os = "linux"))]
    use super::invalid;
    use super::io;
    #[cfg(target_os = "linux")]
    use super::{PathBuf, read_process};
    #[cfg(target_os = "macos")]
    pub(super) fn boot_session() -> io::Result<String> {
        let mut bytes = [0_u8; 37];
        let mut size = bytes.len();
        // SAFETY: fixed read-only sysctl with bounded writable output and no input.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        let bytes = bytes
            .get(..size)
            .ok_or_else(|| invalid("boot session UUID exceeded output"))?;
        std::ffi::CStr::from_bytes_with_nul(bytes)
            .map_err(io::Error::other)?
            .to_str()
            .map(str::to_owned)
            .map_err(io::Error::other)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn boot_session() -> io::Result<String> {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|boot| boot.trim().to_owned())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn boot_session() -> io::Result<String> {
        Err(invalid("native boot session identity unavailable"))
    }
    #[cfg(target_os = "macos")]
    pub(super) fn bsd_info(pid: u32) -> io::Result<Option<libc::proc_bsdinfo>> {
        bsd_info_with_zombies(pid, false)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn bsd_info_with_zombies(pid: u32, include_zombies: bool) -> io::Result<Option<libc::proc_bsdinfo>> {
        let native = i32::try_from(pid).map_err(io::Error::other)?;
        if native <= 0 {
            return Err(invalid("invalid process identity PID"));
        }
        // SAFETY: zeroed proc_bsdinfo is valid writable C output storage.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info);
        // SAFETY: errno is thread-local, and the wrapper may return zero without
        // setting it. Read immediately after this one scalar query.
        unsafe {
            *libc::__error() = 0;
        }
        // SAFETY: info is writable for the supplied checked length; no pointer escapes.
        let got = unsafe {
            libc::proc_pidinfo(
                native,
                libc::PROC_PIDTBSDINFO,
                u64::from(include_zombies),
                (&raw mut info).cast(),
                i32::try_from(size).map_err(io::Error::other)?,
            )
        };
        let error = io::Error::last_os_error();
        if got == 0 {
            return if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(None)
            } else {
                Err(if error.raw_os_error() == Some(0) {
                    invalid("empty process identity observation")
                } else {
                    error
                })
            };
        }
        if usize::try_from(got).ok() != Some(size) || info.pbi_pid != pid {
            return Err(invalid("incomplete process identity observation"));
        }
        Ok(Some(info))
    }
    #[cfg(target_os = "macos")]
    pub(super) fn bsd_start(info: &libc::proc_bsdinfo) -> String {
        format!("macos:{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec)
    }
    #[cfg(target_os = "macos")]
    pub(super) fn snapshot(pid: u32) -> io::Result<Option<(String, bool)>> {
        Ok(bsd_info(pid)?.map(|info| (bsd_start(&info), info.pbi_status != libc::SZOMB)))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn snapshot(pid: u32) -> io::Result<Option<(String, bool)>> {
        read_process(&PathBuf::from(format!("/proc/{pid}/stat")))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn snapshot(_pid: u32) -> io::Result<Option<(String, bool)>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native identity backend unavailable",
        ))
    }
    pub(super) fn scope(pid: u32) -> io::Result<(u32, u32)> {
        let pid = i32::try_from(pid).map_err(io::Error::other)?;
        // SAFETY: scalar scope observations retain no pointers and send no signal.
        let session = unsafe { libc::getsid(pid) };
        // SAFETY: same scalar identity observation boundary.
        let group = unsafe { libc::getpgid(pid) };
        if session < 0 || group < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((session as u32, group as u32))
    }
}
