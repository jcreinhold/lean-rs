//! Explicit storage selection for participating full writer turns.
use super::scratch::{HANDOFF_NAME, Scratch, ScratchParent};
use super::{Duration, File, MetadataExt, Path, PathBuf, invalid, io};
use lean_toolchain::native_guardian::writer_storage::{
    StorageRoot, WriterStorageCharge, WriterStorageTurn, reclaim_idle_writer_storage, recover_disjoint_writer_storage,
    reserve_writer_storage_with_approval, try_reclaim_idle_writer_storage,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

// Two hex digits per input byte, with room for the envelope in a 1 MiB control frame.
pub(super) const MAX_HANDOFF_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    version: u8,
    floor_bytes: u64,
    budget_bytes: u64,
    #[serde(default)]
    command_only: bool,
    #[serde(default, deserialize_with = "present_digest")]
    handoff_sha256: Option<String>,
    #[serde(default, deserialize_with = "present_generation")]
    approval_generation: Option<String>,
    roots: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    role: String,
    path: PathBuf,
    device: u64,
    inode: u64,
}

// An explicitly supplied null must not downgrade a handoff request.
fn present_digest<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Option<String>, D::Error> {
    let digest = String::deserialize(input)?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(serde::de::Error::custom("expected a lowercase SHA-256 handoff digest"));
    }
    Ok(Some(digest))
}

fn present_generation<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Option<String>, D::Error> {
    let generation = String::deserialize(input)?;
    if generation.len() != 32
        || !generation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(serde::de::Error::custom(
            "expected a 32-digit lowercase writer approval generation",
        ));
    }
    Ok(Some(generation))
}

impl Entry {
    fn root(&self) -> StorageRoot {
        StorageRoot {
            path: self.path.clone(),
            device: self.device,
            inode: self.inode,
        }
    }
}

impl Request {
    pub(super) fn requires_handoff(&self) -> bool {
        self.handoff_sha256.is_some()
    }

    pub(super) fn stage_handoff(
        &self,
        message: &serde_json::Value,
        turn: &str,
        scratch: &Scratch<'_>,
    ) -> io::Result<serde_json::Value> {
        let expected = self
            .handoff_sha256
            .as_deref()
            .ok_or_else(|| invalid("handoff was not admitted"))?;
        if message.get("turn").and_then(serde_json::Value::as_str) != Some(turn) {
            return Err(invalid("wrong handoff turn"));
        }
        let encoded = message
            .get("data_hex")
            .and_then(serde_json::Value::as_str)
            .filter(|_| message.as_object().is_some_and(|fields| fields.len() == 3))
            .ok_or_else(|| invalid("invalid handoff stage frame"))?;
        let len = encoded.len() / 2;
        if len > MAX_HANDOFF_BYTES || (len as u64) > self.budget_bytes || !encoded.len().is_multiple_of(2) {
            return Err(invalid("invalid handoff size or writer budget exceeded"));
        }
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "the matched ASCII ranges give values 0 through 15"
        )]
        fn digit(byte: u8) -> io::Result<u8> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(invalid("expected lowercase hex handoff bytes")),
            }
        }
        let bytes = encoded
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[a, b]| Ok(digit(a)? << 4 | digit(b)?))
            .collect::<io::Result<Vec<_>>>()?;
        // Compare the bounded received bytes to the original admission. A
        // stage request supplies neither a path nor its own expected digest.
        let digest = Sha256::digest(&bytes);
        for (byte, &[a, b]) in digest.iter().zip(expected.as_bytes().as_chunks::<2>().0) {
            if *byte != (digit(a)? << 4 | digit(b)?) {
                return Err(invalid("handoff bytes differ from the admitted digest"));
            }
        }
        scratch.write_handoff(&bytes)?;
        Ok(
            serde_json::json!({"state":"handoff_staged","turn":turn,"handoff_sha256":expected,
            "bytes":len,"name":HANDOFF_NAME}),
        )
    }

    pub(super) fn command_only(&self) -> bool {
        self.command_only
    }

    pub(super) fn parse(input: &str) -> io::Result<Self> {
        if input.len() > 16 * 1024 {
            return Err(invalid("oversized writer storage request"));
        }
        let request: Self = serde_json::from_str(input)?;
        if request.version != 1 || request.budget_bytes == 0 || request.roots.len() > 48 {
            return Err(invalid("invalid writer storage request"));
        }
        if request.handoff_sha256.is_some() && !request.command_only {
            return Err(invalid("immutable handoff requires command-only storage"));
        }
        if request.handoff_sha256.is_some() != request.approval_generation.is_some() {
            return Err(invalid(
                "source-bound writer requires both handoff_sha256 and approval_generation",
            ));
        }
        let mut roles = std::collections::BTreeSet::new();
        for entry in &request.roots {
            if entry.role.is_empty()
                || entry.role.len() > 64
                || !entry
                    .role
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || !roles.insert(&entry.role)
            {
                return Err(invalid("invalid or duplicate storage role"));
            }
        }
        for required in ["scratch_parent", "registry", "project"] {
            request.entry(required)?;
        }
        Ok(request)
    }

    fn entry(&self, role: &str) -> io::Result<&Entry> {
        self.roots
            .iter()
            .find(|entry| entry.role == role)
            .ok_or_else(|| invalid(format!("missing storage role {role}")))
    }

    pub(super) fn verify(&self) -> io::Result<()> {
        for entry in &self.roots {
            let meta = std::fs::symlink_metadata(&entry.path)?;
            if !meta.is_dir()
                || entry.path.canonicalize()?.as_os_str() != entry.path.as_os_str()
                || (meta.dev(), meta.ino()) != (entry.device, entry.inode)
            {
                return Err(invalid("writer storage root changed"));
            }
        }
        Ok(())
    }

    pub(super) fn recover_before_admission(
        &self,
        project: &Path,
        next_primary: &Path,
        timeout: Duration,
    ) -> io::Result<()> {
        if self.entry("project")?.path.as_os_str() != project.as_os_str() {
            return Err(invalid("storage project differs from writer project"));
        }
        self.verify()?;
        let registry = self.entry("registry")?.root();
        recover_disjoint_writer_storage(
            &registry,
            next_primary,
            timeout.min(Duration::from_millis(250)),
            |turn, root, locks, until| {
                let (_, first) = locks.first().ok_or_else(|| invalid("writer recovery lock missing"))?;
                let record = super::owner(&mut first.try_clone()?)?
                    .ok_or_else(|| invalid("writer recovery owner is incomplete"))?;
                if record.command_phase != Some(super::CommandPhase::Idle)
                    || record.process.is_some()
                    || record.storage_turn.as_deref() != Some(turn)
                {
                    return Err(invalid("writer recovery owner is not the selected idle turn"));
                }
                Self::check_secondary_records(locks, &record)?;
                let parent = ScratchParent::open(&root.path, (root.device, root.inode))?;
                try_reclaim_idle_writer_storage(
                    &registry,
                    locks,
                    turn,
                    |scratch, started| parent.pin_recovery(scratch, started),
                    |scratch| parent.finish_bounded_recovery(scratch, until),
                )
            },
        )
    }

    fn check_secondary_records(locks: &[(PathBuf, File)], record: &super::Record) -> io::Result<()> {
        for (_, file) in locks.iter().skip(1) {
            if super::owner(&mut file.try_clone()?)?.as_ref() != Some(record) {
                return Err(invalid("writer recovery lock records do not agree on an idle turn"));
            }
        }
        Ok(())
    }

    pub(super) fn reserve(
        &self,
        project: &Path,
        locks: &[(PathBuf, File)],
        timeout: Duration,
    ) -> io::Result<(ScratchParent, WriterStorageCharge)> {
        if self.entry("project")?.path.as_os_str() != project.as_os_str() {
            return Err(invalid("storage project differs from writer project"));
        }
        self.verify()?;
        let s = self.entry("scratch_parent")?;
        let parent = ScratchParent::open(&s.path, (s.device, s.inode))?;
        if self.command_only {
            self.reclaim(&parent, locks, timeout)?;
        }
        let roots = self.roots.iter().map(Entry::root).collect::<Vec<_>>();
        let charge = reserve_writer_storage_with_approval(
            &self.entry("registry")?.root(),
            locks,
            &roots,
            self.floor_bytes,
            self.budget_bytes,
            timeout,
            self.handoff_sha256.as_deref().zip(self.approval_generation.as_deref()),
        )?;
        Ok((parent, charge))
    }
    fn reclaim(&self, parent: &ScratchParent, locks: &[(PathBuf, File)], timeout: Duration) -> io::Result<()> {
        let Some((_, first)) = locks.first() else {
            return Err(invalid("writer recovery primary lock is missing"));
        };
        let Some(record) = super::owner(&mut first.try_clone()?)? else {
            return Ok(());
        };
        if record.command_phase != Some(super::CommandPhase::Idle) || record.process.is_some() {
            return Ok(());
        }
        let turn = record
            .storage_turn
            .as_deref()
            .ok_or_else(|| invalid("writer recovery turn is missing"))?;
        reclaim_idle_writer_storage(
            &self.entry("registry")?.root(),
            locks,
            turn,
            timeout,
            |scratch, cleanup_started| {
                Self::check_secondary_records(locks, &record)?;
                parent.pin_recovery(scratch, cleanup_started)
            },
            |scratch| parent.finish_recovery(scratch),
        )
    }
}

/// Both operations take an instantaneous EX snapshot. Inspection never marks
/// cleanup, and recovery obtains its own observation instead of consuming a preview.
pub(super) fn exact_turn(registry: &StorageRoot, turn: &str, recover: bool) -> (serde_json::Value, bool) {
    use serde_json::json;
    let mut reservation = serde_json::Value::Null;
    let mut phase = None;
    let mut cleanup_finished = false;
    let result = (|| -> io::Result<()> {
        let snapshot = WriterStorageTurn::inspect(registry, turn)?;
        reservation = serde_json::to_value(&snapshot)?;
        let until = std::time::Instant::now()
            .checked_add(Duration::from_secs(5))
            .ok_or_else(|| invalid("invalid recovery deadline"))?;
        let locks = snapshot.try_locks(until)?;
        let (_, primary) = locks.first().ok_or_else(|| invalid("primary lock is missing"))?;
        let record = super::owner(&mut primary.try_clone()?)?
            .ok_or_else(|| invalid("missing complete original owner record"))?;
        phase = record.command_phase;
        if record.storage_turn.as_deref() != Some(turn) {
            return Err(invalid("owner does not identify the selected turn"));
        }
        Request::check_secondary_records(&locks, &record)?;
        let (parent, scratch, started) = snapshot.scratch()?;
        if record.command_phase != Some(super::CommandPhase::Idle) || record.process.is_some() {
            return Err(invalid(
                "missing durable Idle transition confirming command and delegated write closure",
            ));
        }
        let parent = ScratchParent::open(&parent.path, (parent.device, parent.inode))?;
        if recover {
            snapshot.reclaim(
                &locks,
                |binding, marked| parent.pin_recovery(binding, marked),
                |pinned| {
                    parent.finish_recovery(pinned)?;
                    cleanup_finished = true;
                    Ok(())
                },
            )?;
        } else {
            snapshot.check_idle_owner(&locks)?;
            parent.pin_recovery(scratch, started)?;
        }
        Ok(())
    })();
    let state = match (&result, recover) {
        (Err(_), true) if cleanup_finished => "settlement_unconfirmed",
        (Err(_), _) => "refused",
        (Ok(()), true) => "settled",
        (Ok(()), false) => "eligible",
    };
    let ok = result.is_ok();
    let report = json!({"version":1,"turn":turn,"registry":registry,"reservation":reservation,
        "command_phase":phase,"old_result":"unknown","state":state,"reason":result.err().map(|e|e.to_string())});
    (report, ok)
}
