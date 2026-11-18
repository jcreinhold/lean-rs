//! Shared exact identities and bounded owner-record admission.
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
}
/// The one default native namespace shared by all cooperating managed writers.
/// # Errors
/// Returns unavailable local-data location or invalid directory identity.
pub fn registry_directory() -> io::Result<PathBuf> {
    let root = match std::env::var_os(super::OWNERS_DIRECTORY_ENV) {
        Some(root) => PathBuf::from(root),
        None => dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("local data directory unavailable"))?
            .join("lean-host-mcp")
            .join("native-generations"),
    };
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
    match fs::symlink_metadata(&root) {
        Ok(_) => validate_registry(&root)?,
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
    Ok(root)
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
fn validate_owner_header(value: &Value) -> io::Result<()> {
    let version = field(value, "version")
        .as_u64()
        .ok_or_else(|| invalid("native owner version missing"))?;
    if !matches!(version, 1 | 2)
        || field(value, "scope").as_str() != Some(format!("inherited-process-group-v{version}").as_str())
        || !field(value, "generation_id").as_str().is_some_and(valid_nonce)
    {
        return Err(invalid("invalid native owner header"));
    }
    checked_identity(field(value, "parent"))?;
    if let Some(slot) = value.get("capacity_slot").filter(|value| !value.is_null()) {
        if version != 2 {
            return Err(invalid("legacy native owner cannot bind an actor slot"));
        }
        serde_json::from_value::<NativeCapacitySlot>(slot.clone())
            .map_err(io::Error::other)?
            .validate()?;
    }
    let phase = field(value, "phase");
    if phase == "preparing" || phase == "preparing_closed" {
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
    if phase != "running" && !(version == 1 && phase.is_null()) {
        return Err(invalid("unknown native owner phase"));
    }
    let process = checked_identity(field(value, "process"))?;
    if process.pid <= 1 || process.pid != process.session || process.pid != process.process_group {
        return Err(invalid("invalid native owner scope"));
    }
    if version == 2 {
        let guardian = checked_identity(field(value, "guardian"))?;
        let pins_root_session = guardian.session == process.pid;
        let has_own_group = guardian.process_group == guardian.pid && guardian.pid != process.pid;
        if !pins_root_session || !has_own_group {
            return Err(invalid("invalid native guardian scope"));
        }
    }
    if version == 2 || value.get("protected_domains").is_some() {
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
    let process = checked_identity(field(value, "process"))?;
    crate::process_group::group_present(process.process_group).map(|present| !present)
}
// Record retention includes the descriptor custodian, independently of compute
// occupancy. A closed payload may leave a live guardian holding artifact loans.
fn record_can_be_removed(value: &Value) -> io::Result<bool> {
    if !payload_closed(value)? {
        return Ok(false);
    }
    if field(value, "version") == 2 && field(value, "phase") == "running" {
        return checked_identity(field(value, "guardian"))?.is_live().map(|live| !live);
    }
    Ok(true)
}
/// Retire only this caller's exact undispatched Preparing reservation.
///
/// Running publication always precedes the native startup ACK. A retired
/// Preparing record therefore cannot have admitted native body work; keeping
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
        return Err(invalid("unrecognized unpublished native directory"));
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
/// Reserve bounded generation and actor capacity under the same admission lock.
///
/// Live groups keep their actor slot after server death. A successful passive
/// whole-group absence observation releases capacity, never a proof outcome.
/// A still-live guardian retains its recovery record without occupying that slot.
/// Preparing records certify no body dispatch and may be collected after their
/// exact parent dies. A child paused before its startup ACK may still exist, but
/// cannot load runtime/artifacts or execute a request before promotion to Running.
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
    let mutex = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".admission.lock"))?;
    loop {
        if std::time::Instant::now() >= until {
            return Err(expired());
        }
        match mutex.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => thread::sleep(POLL),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
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
            // Keep the committed record parseable through partial asset cleanup.
            super::cleanup_generation_inputs(&entry.path())?;
            for name in ["owner.write", "owner.guardian-write"] {
                match fs::remove_file(entry.path().join(name)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            fs::remove_file(record)?;
            fs::remove_dir(entry.path())?;
        } else {
            occupied.push(value);
        }
    }
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
    #[cfg(target_os = "macos")]
    use super::invalid;
    use super::io;
    #[cfg(target_os = "linux")]
    use super::{PathBuf, read_process};
    #[cfg(target_os = "macos")]
    pub(super) fn snapshot(pid: u32) -> io::Result<Option<(String, bool)>> {
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
                0,
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
        Ok(Some((
            format!("macos:{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
            info.pbi_status != libc::SZOMB,
        )))
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
