//! Explicit storage selection for participating full writer turns.
use super::scratch::ScratchParent;
use super::{Duration, File, MetadataExt, Path, PathBuf, invalid, io};
use lean_toolchain::native_guardian::writer_storage::{
    StorageRoot, WriterStorageCharge, reclaim_idle_writer_storage, reserve_writer_storage,
};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    version: u8,
    floor_bytes: u64,
    budget_bytes: u64,
    #[serde(default)]
    command_only: bool,
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
        let charge = reserve_writer_storage(
            &self.entry("registry")?.root(),
            locks,
            &roots,
            self.floor_bytes,
            self.budget_bytes,
            timeout,
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
                for (_, file) in locks.iter().skip(1) {
                    if super::owner(&mut file.try_clone()?)?.as_ref() != Some(&record) {
                        return Err(invalid("writer recovery lock records do not agree on an idle turn"));
                    }
                }
                parent.pin_recovery(scratch, cleanup_started)
            },
            |scratch| parent.finish_recovery(scratch),
        )
    }
}
