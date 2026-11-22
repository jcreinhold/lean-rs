//! Managed scratch operations. Callers must retain exclusive control of the
//! private scratch tree during cleanup and serialize changes to its directory entry.
use super::{File, MetadataExt, Path, PathBuf, invalid, io};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, mkdirat, open, openat, statat, unlinkat};
use std::ffi::CStr;
use std::io::Write as _;
use std::time::Instant;

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(super) struct ScratchParent {
    dir: File,
    path: PathBuf,
}

impl ScratchParent {
    pub(super) fn open(path: &Path, expected: (u64, u64)) -> io::Result<Self> {
        if !path.is_absolute() || path.canonicalize()? != path {
            return Err(invalid("expected canonical scratch parent"));
        }
        let dir = File::from(open(path, DIRECTORY, Mode::empty())?);
        let meta = dir.metadata()?;
        if (meta.dev(), meta.ino()) != expected {
            return Err(invalid("scratch parent changed"));
        }
        Ok(Self { dir, path: path.into() })
    }

    // Call only after shared admission. A registry-minted turn ID supplies one
    // fresh component; collisions are refusal, never reuse.
    pub(super) fn create(&self, turn: &str) -> io::Result<Scratch<'_>> {
        if turn.len() != 32 || !turn.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("invalid scratch turn"));
        }
        let name = format!("lean-writer-assets-{turn}");
        mkdirat(&self.dir, name.as_str(), Mode::RWXU)?;
        let dir = File::from(openat(&self.dir, name.as_str(), DIRECTORY, Mode::empty())?);
        Ok(Scratch {
            parent: self,
            name,
            dir,
        })
    }
    pub(super) fn pin_recovery(
        &self,
        expected: &lean_toolchain::native_guardian::writer_storage::StorageRoot,
        cleanup_started: bool,
    ) -> io::Result<(PathBuf, Option<Scratch<'_>>)> {
        if expected.path.parent() != Some(self.path.as_path()) {
            return Err(invalid("writer recovery scratch parent differs"));
        }
        let name = expected
            .path
            .file_name()
            .ok_or_else(|| invalid("writer recovery scratch name missing"))?;
        match openat(&self.dir, name, DIRECTORY, Mode::empty()) {
            Ok(dir) => {
                let dir = File::from(dir);
                let meta = dir.metadata()?;
                if (meta.dev(), meta.ino()) != (expected.device, expected.inode) {
                    return Err(invalid("writer recovery scratch inode changed"));
                }
                let scratch = Scratch {
                    parent: self,
                    name: name
                        .to_str()
                        .ok_or_else(|| invalid("writer recovery scratch name is invalid"))?
                        .into(),
                    dir,
                };
                scratch.verified_path()?;
                Ok((PathBuf::from(name), Some(scratch)))
            }
            Err(rustix::io::Errno::NOENT) if cleanup_started => {
                match statat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                    Err(rustix::io::Errno::NOENT) => Ok((PathBuf::from(name), None)),
                    _ => Err(invalid("writer recovery scratch absence is ambiguous")),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn finish_recovery(&self, (name, scratch): (PathBuf, Option<Scratch<'_>>)) -> io::Result<()> {
        match scratch {
            Some(scratch) => scratch.remove(),
            None => match statat(&self.dir, &name, AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => self.dir.sync_all(),
                _ => Err(invalid("writer recovery scratch absence changed")),
            },
        }
    }
    pub(super) fn finish_bounded_recovery(
        &self,
        pinned: (PathBuf, Option<Scratch<'_>>),
        until: Instant,
    ) -> io::Result<()> {
        let (name, scratch) = pinned;
        let budget = RecoveryBudget { until, remaining: 4096 };
        budget.check()?;
        match scratch {
            Some(scratch) => scratch.remove_inner(Some(budget)),
            None => self.finish_recovery((name, None)),
        }
    }
}

pub(super) struct Scratch<'a> {
    parent: &'a ScratchParent,
    name: String,
    dir: File,
}

impl Scratch<'_> {
    /// The helper owns the only writable descriptor. A fatal partial write
    /// leaves the file in this turn's scratch for exact Idle recovery.
    pub(super) fn write_handoff(&self, contents: &[u8]) -> io::Result<()> {
        let mut output = self.create_file(HANDOFF_NAME)?;
        output.write_all(contents)?;
        output.sync_all()?;
        drop(output);
        self.dir.sync_all()?;
        self.verified_path()?;
        Ok(())
    }

    pub(super) fn create_file(&self, name: &str) -> io::Result<File> {
        self.verified_path()?;
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        Ok(File::from(openat(&self.dir, name, flags, Mode::RUSR)?))
    }

    pub(super) fn verified_path(&self) -> io::Result<PathBuf> {
        let path = self.parent.path.join(&self.name);
        let named = std::fs::symlink_metadata(&path)?;
        let held = self.dir.metadata()?;
        if path.canonicalize()? != path || (named.dev(), named.ino()) != (held.dev(), held.ino()) {
            return Err(invalid("scratch path no longer names the held directory"));
        }
        Ok(path)
    }

    pub(super) fn binding(&self) -> io::Result<lean_toolchain::native_guardian::writer_storage::StorageRoot> {
        let path = self.verified_path()?;
        let held = self.dir.metadata()?;
        self.dir.sync_all()?;
        self.parent.dir.sync_all()?;
        Ok(lean_toolchain::native_guardian::writer_storage::StorageRoot {
            path,
            device: held.dev(),
            inode: held.ino(),
        })
    }

    pub(super) fn remove(self) -> io::Result<()> {
        self.remove_inner(None)
    }

    fn remove_inner(self, mut budget: Option<RecoveryBudget>) -> io::Result<()> {
        let held = rustix::fs::fstat(&self.dir)?;
        let check = || -> io::Result<()> {
            let st = statat(&self.parent.dir, self.name.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
            if st.st_ino != held.st_ino || st.st_dev != held.st_dev {
                return Err(invalid("scratch entry changed before cleanup"));
            }
            Ok(())
        };
        check()?;
        remove_contents(&self.dir, self.dir.metadata()?.dev(), budget.as_mut(), 0)?;
        if let Some(budget) = &budget {
            budget.check()?;
        }
        check()?;
        unlinkat(&self.parent.dir, self.name.as_str(), AtFlags::REMOVEDIR)?;
        self.parent.dir.sync_all()
    }
}

pub(super) const HANDOFF_NAME: &str = "handoff.json";

struct RecoveryBudget {
    until: Instant,
    remaining: usize,
}

impl RecoveryBudget {
    fn check(&self) -> io::Result<()> {
        if Instant::now() >= self.until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "writer recovery cleanup budget exhausted",
            ));
        }
        Ok(())
    }
}

fn remove_contents(dir: &File, device: u64, mut budget: Option<&mut RecoveryBudget>, depth: usize) -> io::Result<()> {
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let name: &CStr = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if let Some(budget) = budget.as_deref_mut() {
            budget.check()?;
            if depth >= 32 {
                return Err(invalid("writer recovery scratch depth exceeds limit"));
            }
            budget.remaining = budget.remaining.checked_sub(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "writer recovery cleanup entry budget exhausted",
                )
            })?;
        }
        let st = statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(st.st_mode) == FileType::Directory {
            let child = File::from(openat(dir, name, DIRECTORY, Mode::empty())?);
            if child.metadata()?.dev() != device {
                return Err(invalid("scratch crosses a filesystem"));
            }
            remove_contents(&child, device, budget.as_deref_mut(), depth.saturating_add(1))?;
            unlinkat(dir, name, AtFlags::REMOVEDIR)?;
        } else {
            unlinkat(dir, name, AtFlags::empty())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    const TURN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn replacement_and_descriptor_relative_cleanup() -> io::Result<()> {
        let tmp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
        let base = tmp.path().canonicalize()?;
        let parent = base.join("parent");
        let moved = base.join("moved");
        fs::create_dir(&parent)?;
        let meta = fs::metadata(&parent)?;
        let id = (meta.dev(), meta.ino());
        let opened = ScratchParent::open(&parent, id)?;
        fs::rename(&parent, &moved)?;
        fs::create_dir(&parent)?;
        assert!(ScratchParent::open(&parent, id).is_err());
        let scratch = opened.create(TURN)?;
        assert!(scratch.verified_path().is_err());
        assert_eq!(fs::read_dir(&parent)?.count(), 0);
        let actual = moved.join(&scratch.name);
        fs::create_dir(actual.join("nested"))?;
        fs::write(actual.join("nested/file"), b"owned")?;
        std::os::unix::fs::symlink(&parent, actual.join("external"))?;
        scratch.remove()?;
        assert!(!actual.exists());
        assert!(parent.is_dir());
        assert_eq!(fs::read_dir(&parent)?.count(), 0);
        fs::remove_dir(&parent)?;
        fs::rename(&moved, &parent)?;
        let scratch = opened.create(TURN)?;
        let path = scratch.verified_path()?;
        scratch.remove()?;
        assert!(!path.exists());
        Ok(())
    }
}
