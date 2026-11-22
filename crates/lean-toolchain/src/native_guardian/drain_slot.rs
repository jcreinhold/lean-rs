//! Descriptor-relative, single-offer storage inside one native generation.
use rustix::fs::{AtFlags, Dir, Mode, OFlags, mkdirat, openat, renameat, statat, unlinkat};
use std::fs::File;
use std::io;

use super::invalid;

pub(super) const NAME: &str = "drain-slot";
const PENDING: &str = "pending";
const REFUSED: &str = "refused";
const ACCEPTED: &str = "accepted";
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum State {
    Empty,
    Pending,
    Refused,
    Accepted,
}

pub(super) struct Slot(File);

fn checked_directory(parent: &File, name: &str) -> io::Result<File> {
    use std::os::unix::fs::MetadataExt as _;
    let file = File::from(openat(parent, name, DIRECTORY, Mode::empty())?);
    let metadata = file.metadata()?;
    if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o777 != 0o700 {
        return Err(invalid("native drain slot directory is not private"));
    }
    Ok(file)
}

fn optional_directory(parent: &File, name: &str) -> io::Result<Option<File>> {
    match checked_directory(parent, name) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn empty(file: &File) -> io::Result<bool> {
    for entry in Dir::read_from(file)? {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Ok(false);
        }
    }
    Ok(true)
}

impl Slot {
    pub(super) fn open(generation: &File) -> io::Result<Option<Self>> {
        Ok(optional_directory(generation, NAME)?.map(Self))
    }

    pub(super) fn create(generation: &File) -> io::Result<Self> {
        mkdirat(generation, NAME, PRIVATE)?;
        generation.sync_all()?;
        Ok(Self(checked_directory(generation, NAME)?))
    }

    fn marker(&self, name: &str) -> io::Result<Option<File>> {
        let marker = optional_directory(&self.0, name)?;
        if let Some(directory) = &marker
            && !empty(directory)?
        {
            return Err(invalid("native drain marker is not empty"));
        }
        Ok(marker)
    }

    pub(super) fn state(&self) -> io::Result<State> {
        // A new offer takes precedence over a previously consumed offer.
        let refused = self.marker(REFUSED)?.is_some();
        let pending = self.marker(PENDING)?.is_some();
        if self.marker(ACCEPTED)?.is_some() {
            Ok(State::Accepted)
        } else if pending {
            Ok(State::Pending)
        } else if refused || self.marker(REFUSED)?.is_some() {
            Ok(State::Refused)
        } else {
            Ok(State::Empty)
        }
    }

    pub(super) fn offer(&self) -> io::Result<()> {
        match mkdirat(&self.0, PENDING, PRIVATE) {
            Ok(()) => Ok(()),
            Err(rustix::io::Errno::EXIST) if self.marker(PENDING)?.is_some() => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn mark_accepted(&self) -> io::Result<()> {
        self.move_pending(ACCEPTED)
    }

    // Sole consumer, under the existing admission lock. Keep the original
    // directory open across the rename so its inode cannot be reused.
    pub(super) fn refuse(&self) -> io::Result<()> {
        self.move_pending(REFUSED)
    }

    fn move_pending(&self, destination: &str) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let original = self
            .marker(PENDING)?
            .ok_or_else(|| invalid("pending drain marker vanished"))?;
        self.marker(destination)?;
        if let Err(error) = renameat(&self.0, PENDING, &self.0, destination) {
            let original = original.metadata()?;
            let observed = self.marker(destination)?.map(|file| file.metadata()).transpose()?;
            if observed.is_none_or(|metadata| (metadata.dev(), metadata.ino()) != (original.dev(), original.ino())) {
                return Err(error.into());
            }
        }
        self.sync()
    }

    pub(super) fn sync(&self) -> io::Result<()> {
        self.0.sync_all()
    }

    pub(super) fn collect_closed(generation: &File) -> io::Result<()> {
        let Some(slot) = Self::open(generation)? else {
            // A previous removal may have returned an uncertain directory sync.
            return generation.sync_all();
        };
        for entry in Dir::read_from(&slot.0)? {
            match entry?.file_name().to_bytes() {
                b"." | b".." | b"pending" | b"refused" | b"accepted" => {}
                _ => return Err(invalid("unknown native drain slot entry")),
            }
        }
        for name in [PENDING, REFUSED, ACCEPTED] {
            if slot.marker(name)?.is_some() {
                unlinkat(&slot.0, name, AtFlags::REMOVEDIR)?;
            }
        }
        // A sender may still hold a slot descriptor. A concurrent insertion
        // makes removal fail, and the caller must preserve owner.json.
        unlinkat(generation, NAME, AtFlags::REMOVEDIR)?;
        generation.sync_all()
    }
}

pub(super) fn exists(generation: &File) -> io::Result<bool> {
    match statat(generation, NAME, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{NAME, Slot, State, exists};
    use std::fs::{self, File};
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn create() -> std::io::Result<Self> {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "drain-slot-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&root)?;
            Ok(Self(root))
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _removed = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn duplicate_offer_coalesces_and_fresh_pending_precedes_refusal() -> std::io::Result<()> {
        let root = TestRoot::create()?;
        let generation = File::open(&root.0)?;
        let slot = Slot::create(&generation)?;
        assert_eq!(slot.state()?, State::Empty);
        slot.offer()?;
        slot.offer()?;
        assert_eq!(slot.state()?, State::Pending);
        slot.refuse()?;
        assert_eq!(slot.state()?, State::Refused);
        slot.offer()?;
        assert_eq!(slot.state()?, State::Pending);
        slot.refuse()?;
        assert_eq!(slot.state()?, State::Refused);
        Slot::collect_closed(&generation)?;
        assert!(!exists(&generation)?);
        Ok(())
    }

    #[test]
    fn removed_slot_cannot_be_reopened_through_stale_descriptor() -> std::io::Result<()> {
        let root = TestRoot::create()?;
        let generation = File::open(&root.0)?;
        let slot = Slot::create(&generation)?;
        assert!(exists(&generation)?);
        Slot::collect_closed(&generation)?;
        let recreated = Slot::create(&generation)?;
        assert!(slot.offer().is_err());
        assert_eq!(recreated.state()?, State::Empty);
        Slot::collect_closed(&generation)
    }

    #[test]
    fn late_offer_keeps_existing_generation_until_collection_retries() -> std::io::Result<()> {
        let root = TestRoot::create()?;
        let generation = File::open(&root.0)?;
        let slot = Slot::create(&generation)?;
        let owner = root.0.join("owner.json");
        fs::write(&owner, b"owner evidence")?;

        // This is the interleaving after the collector observes an empty slot
        // and before the final descriptor-relative rmdir.
        assert_eq!(slot.state()?, State::Empty);
        slot.offer()?;
        assert_eq!(
            rustix::fs::unlinkat(&generation, NAME, rustix::fs::AtFlags::REMOVEDIR),
            Err(rustix::io::Errno::NOTEMPTY)
        );
        assert_eq!(fs::read(&owner)?, b"owner evidence");
        Slot::collect_closed(&generation)?;
        assert!(!exists(&generation)?);
        Ok(())
    }

    #[test]
    fn unknown_slot_content_preserves_owner() -> std::io::Result<()> {
        let root = TestRoot::create()?;
        let generation = File::open(&root.0)?;
        let _slot = Slot::create(&generation)?;
        let owner = root.0.join("owner.json");
        std::fs::write(&owner, b"owner evidence")?;
        std::fs::write(root.0.join(NAME).join("unknown"), b"")?;
        assert!(Slot::collect_closed(&generation).is_err());
        assert!(exists(&generation)?);
        assert_eq!(std::fs::read(owner)?, b"owner evidence");
        Ok(())
    }
}
