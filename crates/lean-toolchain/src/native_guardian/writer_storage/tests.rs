//! Two independent processes hold real disjoint project locks. Only the
//! free-space sample is fixed so that unrelated disk activity cannot move the boundary.
#![allow(
    clippy::unwrap_used,
    reason = "fixture failures must fail the exact custody assertion"
)]

use super::*;
use std::fs::{DirBuilder, OpenOptions};
use std::os::unix::fs::DirBuilderExt as _;
use std::process::{Child, Command, Stdio};
use std::time::SystemTime;

const CAPACITY: u64 = 12 * 1024;
const BUDGET: u64 = 8 * 1024;
const FLOOR: u64 = 2 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);
const CHILD_ROOT: &str = "LEAN_RS_STORAGE_ADMISSION_TEST_ROOT";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-state");
        fs::create_dir_all(&target).unwrap();
        let path = target.canonicalize().unwrap().join(format!(
            "writer-storage-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        for name in ["owners", "first", "second"] {
            DirBuilder::new().mode(0o700).create(path.join(name)).unwrap();
        }
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn expected(path: PathBuf) -> StorageRoot {
    let meta = fs::metadata(&path).unwrap();
    StorageRoot {
        path,
        device: meta.dev(),
        inode: meta.ino(),
    }
}
fn lock(project: &Path) -> (PathBuf, File) {
    let path = crate::shared_writer::project_lock_path(project).unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    file.try_lock().unwrap();
    (path, file)
}

fn publish_idle_records(locks: &[(PathBuf, File)], turn: &str) -> io::Result<()> {
    let pid = std::process::id();
    let record = serde_json::json!({
        "version": 1,
        "owner": {"version": 1, "pid": pid, "start": "fixture", "session": pid},
        "process": null,
        "storage_turn": turn,
        "command_phase": "idle"
    });
    for (_, file) in locks {
        let mut file = file.try_clone()?;
        file.set_len(0)?;
        file.rewind()?;
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    Ok(())
}

fn request(root: &Path, locks: &[(PathBuf, File)]) -> io::Result<WriterStorageCharge> {
    let registry = expected(root.join("owners"));
    reserve(
        &registry.path,
        &registry,
        locks,
        &[expected(root.to_owned())],
        (FLOOR, BUDGET, None),
        deadline(TIMEOUT)?,
        |_| Ok(CAPACITY),
    )
}

struct Helper(Child);
impl Drop for Helper {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}
fn launch(root: &Path, mode: &str) -> Helper {
    let name = format!("{}::storage_writer_child", module_path!().split_once("::").unwrap().1);
    let mut helper = Helper(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD_ROOT, root)
            .env(super::super::OWNERS_DIRECTORY_ENV, root.join("owners"))
            .env("LEAN_RS_STORAGE_ADMISSION_TEST_PHASE", mode)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now().checked_add(TIMEOUT).unwrap();
    while !root.join("ready").exists() {
        assert!(helper.0.try_wait().unwrap().is_none(), "child exited before readiness");
        assert!(Instant::now() < until, "child never reached its selected barrier");
        std::thread::sleep(Duration::from_millis(5));
    }
    helper
}

#[test]
fn storage_writer_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let locks = [lock(&root.join("first"))];
    let charge = reserve_writer_storage(
        &expected(root.join("owners")),
        &locks,
        &[expected(root.clone())],
        FLOOR,
        BUDGET,
        TIMEOUT,
    )
    .unwrap();
    let scratch = root.join(format!("scratch-{}", charge.turn()));
    let stage = std::env::var("LEAN_RS_STORAGE_ADMISSION_TEST_PHASE").unwrap() == "post";
    if stage {
        fs::create_dir(&scratch).unwrap();
        let outcome = Command::new("/bin/sh")
            .args(["-c", "printf payload > output"])
            .current_dir(&scratch)
            .status()
            .unwrap();
        assert!(outcome.success());
    }
    fs::write(root.join("turn"), charge.turn()).unwrap();
    fs::write(root.join("ready"), []).unwrap();
    std::io::stdin().read_exact(&mut [0]).unwrap();
    if stage {
        fs::remove_dir_all(&scratch).unwrap();
    }
    charge.settle_after_cleanup(&locks[0].1, Ok(()), TIMEOUT).unwrap();
}

#[test]
fn disjoint_writer_waits_through_payload_exit_and_final_cleanup() {
    let f = Fixture::new();
    let mut first = launch(&f.0, "post");
    let second = [lock(&f.0.join("second"))];
    let scratch =
        f.0.join(format!("scratch-{}", fs::read_to_string(f.0.join("turn")).unwrap()));
    assert_eq!(fs::read(scratch.join("output")).unwrap(), b"payload");
    assert_eq!(request(&f.0, &second).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    let owners = f.0.join("owners");
    assert_eq!(
        ownership::collect_closed_owner_records(&owners, TIMEOUT)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    first.0.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(first.0.wait().unwrap().success());
    assert!(!scratch.try_exists().unwrap());
    let next = request(&f.0, &second).unwrap();
    next.settle_after_cleanup(&second[0].1, Ok(()), TIMEOUT).unwrap();
    ownership::collect_closed_owner_records(&owners, TIMEOUT).unwrap();
    assert_eq!(fs::read_dir(&owners).unwrap().count(), 1);
}

#[test]
fn restart_retains_pre_go_and_post_payload_charges_and_fences_secondary_replacement() {
    for mode in ["pre", "post"] {
        let f = Fixture::new();
        let mut first = launch(&f.0, mode);
        first.0.kill().unwrap();
        assert!(!first.0.wait().unwrap().success());
        let second = lock(&f.0.join("second"));
        let previous = lock(&f.0.join("first"));
        let locks = [second, previous];
        assert_eq!(
            request(&f.0, &locks[..1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let registry = expected(f.0.join("owners"));
        let refusal = reserve(
            &registry.path,
            &registry,
            &locks,
            &[expected(f.0.clone())],
            (0, 1, None),
            deadline(TIMEOUT).unwrap(),
            |_| Ok(CAPACITY),
        )
        .unwrap_err();
        assert!(refusal.to_string().contains("prior writer storage custody"));
        let scratch =
            f.0.join(format!("scratch-{}", fs::read_to_string(f.0.join("turn")).unwrap()));
        assert_eq!(scratch.try_exists().unwrap(), mode == "post");
    }
}

#[test]
fn registry_binding_and_lowest_outstanding_root_are_checked() {
    let f = Fixture::new();
    let registry = expected(f.0.join("owners"));
    let first = [lock(&f.0.join("first"))];
    let second = [lock(&f.0.join("second"))];
    let old_root = expected(f.0.join("first"));
    assert!(
        reserve(
            &f.0.join("second"),
            &registry,
            &first,
            std::slice::from_ref(&old_root),
            (FLOOR, 1, None),
            deadline(TIMEOUT).unwrap(),
            |_| Ok(CAPACITY)
        )
        .is_err()
    );
    assert!(!registry.path.join(".admission.lock").exists());
    let charge = reserve(
        &registry.path,
        &registry,
        &first,
        std::slice::from_ref(&old_root),
        (FLOOR, 1, None),
        deadline(TIMEOUT).unwrap(),
        |_| Ok(CAPACITY),
    )
    .unwrap();
    let refused = reserve(
        &registry.path,
        &registry,
        &second,
        &[expected(f.0.clone())],
        (FLOOR, 1, None),
        deadline(TIMEOUT).unwrap(),
        |dir| {
            Ok(if dir.metadata()?.ino() == old_root.inode {
                FLOOR + 1
            } else {
                CAPACITY
            })
        },
    )
    .unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    let err = charge
        .settle_after_cleanup(&first[0].1, Err(io::Error::other("scratch removal failed")), TIMEOUT)
        .unwrap_err();
    assert_eq!(err.to_string(), "scratch removal failed");
    assert!(charges_remain(&registry.path).unwrap());
}

#[test]
fn simultaneous_disjoint_requests_cannot_both_spend_the_same_free_bytes() {
    let fixture = Fixture::new();
    let start = std::sync::Barrier::new(2);
    let enter = |project| {
        let locks = [lock(&fixture.0.join(project))];
        start.wait();
        (request(&fixture.0, &locks), locks)
    };
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| enter("first"));
        let second = enter("second");
        [first.join().unwrap(), second]
    });
    let mut admitted = 0;
    let mut refused = 0;
    for (result, locks) in results {
        match result {
            Ok(charge) => {
                charge.settle_after_cleanup(&locks[0].1, Ok(()), TIMEOUT).unwrap();
                admitted += 1;
            }
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::WouldBlock, "{e:?}");
                refused += 1;
            }
        }
    }
    assert_eq!((admitted, refused), (1, 1));
}

#[test]
fn scratch_binding_rejects_wrong_inode_and_fences_all_turn_locks() {
    let fixture = Fixture::new();
    let locks = [lock(&fixture.0.join("first")), lock(&fixture.0.join("second"))];
    let mut charge = request(&fixture.0, &locks).unwrap();
    let scratch = fixture.0.join(format!("lean-writer-assets-{}", charge.turn()));
    DirBuilder::new().mode(0o700).create(&scratch).unwrap();
    let original = expected(scratch);
    let displaced = fixture.0.join("displaced");
    fs::rename(&original.path, &displaced).unwrap();
    DirBuilder::new().mode(0o700).create(&original.path).unwrap();
    assert_ne!(original.inode, expected(original.path.clone()).inode);
    assert!(charge.bind_scratch(original.clone(), TIMEOUT).is_err());
    fs::remove_dir(&original.path).unwrap();
    fs::rename(displaced, &original.path).unwrap();
    assert!(charge.charge.scratch.is_none());
    charge.bind_scratch(original.clone(), TIMEOUT).unwrap();
    let root = charge.registry.open().unwrap();
    let dir = charge_directory(&root, false).unwrap().unwrap();
    assert_eq!(Charge::read(&dir, charge.turn()).unwrap().scratch, Some(original));
    let registry = expected(fixture.0.join("owners"));
    let refusal = reserve(
        &registry.path,
        &registry,
        &locks[1..],
        &[expected(fixture.0.clone())],
        (0, 1, None),
        deadline(TIMEOUT).unwrap(),
        |_| Ok(CAPACITY),
    )
    .unwrap_err();
    assert!(refusal.to_string().contains("prior writer storage custody"));
}

#[test]
fn interrupted_publication_preserves_the_exact_charge() -> io::Result<()> {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        let fixture = Fixture::new();
        let module = module_path!().split_once("::").unwrap().1;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                &format!("{module}::interrupted_publication_preserves_the_exact_charge"),
            ])
            .env(CHILD_ROOT, &fixture.0)
            .env(super::super::OWNERS_DIRECTORY_ENV, fixture.0.join("owners"))
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let registry = expected(root.join("owners"));
    let directory = registry.path.join(CHARGES);
    let staged = directory.join(STAGED_CHARGE);
    let first = [lock(&root.join("first"))];
    let disjoint = || -> io::Result<()> {
        let second = lock(&root.join("second"));
        reserve(
            &registry.path,
            &registry,
            std::slice::from_ref(&second),
            &[expected(root.clone())],
            (FLOOR, 1, None),
            deadline(TIMEOUT)?,
            |_| Ok(CAPACITY),
        )?
        .settle_after_cleanup(&second.1, Ok(()), TIMEOUT)
    };

    CHARGE_WRITE_CUT.set(Some(ChargeWriteCut::DuringWrite));
    assert!(request(&root, &first).is_err());
    let partial = fs::read(&staged)?;
    assert!(!partial.is_empty() && serde_json::from_slice::<Charge>(&partial).is_err());
    assert_eq!(fs::read_dir(&directory)?.count(), 1);
    disjoint()?;
    assert!(!staged.try_exists()?);

    let mut charge = request(&root, &first)?;
    let turn = charge.turn().to_owned();
    let entry = directory.join(&turn);
    let unbound = fs::read(&entry)?;
    let scratch = root.join(format!("lean-writer-assets-{turn}"));
    fs::create_dir(&scratch)?;
    let binding = expected(scratch.clone());
    CHARGE_WRITE_CUT.set(Some(ChargeWriteCut::BeforeRename));
    assert!(charge.bind_scratch_for_turn(binding.clone(), true, TIMEOUT).is_err());
    assert_eq!(fs::read(&entry)?, unbound);
    assert_eq!(
        serde_json::from_slice::<Charge>(&fs::read(&staged)?)?.scratch,
        Some(binding.clone())
    );
    disjoint()?;
    assert_eq!(fs::read(&entry)?, unbound);
    charge.bind_scratch_for_turn(binding.clone(), true, TIMEOUT)?;

    let bound = fs::read(&entry)?;
    publish_idle_records(&first, &turn)?;
    CHARGE_WRITE_CUT.set(Some(ChargeWriteCut::DuringWrite));
    assert!(charge.begin_command_only_scratch_cleanup(TIMEOUT).is_err());
    assert!(staged.try_exists()?);
    assert_eq!(fs::read(&entry)?, bound);
    disjoint()?;
    assert_eq!(fs::read(&entry)?, bound);
    assert!(!staged.try_exists()?);
    drop(charge);
    drop(first);

    let scratch_removed = std::cell::Cell::new(false);
    for cut in [ChargeWriteCut::AfterRename, ChargeWriteCut::DirectorySync] {
        let snapshot = WriterStorageTurn::inspect(&registry, &turn)?;
        let locks = snapshot.try_locks(deadline(TIMEOUT)?)?;
        CHARGE_WRITE_CUT.set(Some(cut));
        let result = snapshot.reclaim(
            &locks,
            |found, _| {
                assert_eq!(found, &binding);
                Ok(())
            },
            |()| {
                scratch_removed.set(true);
                Ok(())
            },
        );
        assert!(result.is_err() && CHARGE_WRITE_CUT.take().is_none());
        assert!(!scratch_removed.get());
        assert!(scratch.is_dir());
        assert!(!staged.try_exists()?);
        let marked = serde_json::from_slice::<Charge>(&fs::read(&entry)?)?;
        assert!(marked.scratch_cleanup_started);
        assert_eq!(marked.scratch.as_ref(), Some(&binding));
    }

    let snapshot = WriterStorageTurn::inspect(&registry, &turn)?;
    let locks = snapshot.try_locks(deadline(TIMEOUT)?)?;
    snapshot.reclaim(
        &locks,
        |found, marked| {
            assert_eq!(found, &binding);
            assert!(marked);
            Ok(())
        },
        |()| {
            fs::remove_dir(&scratch)?;
            File::open(&root)?.sync_all()
        },
    )?;
    assert!(!scratch.try_exists()?);
    assert!(!entry.try_exists()?);
    assert!(WriterStorageTurn::inspect(&registry, &turn).is_err());
    Ok(())
}

#[test]
fn exact_recoverers_release_partial_locks_and_settle_once() -> io::Result<()> {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        let fixture = Fixture::new();
        let module = module_path!().split_once("::").unwrap().1;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                &format!("{module}::exact_recoverers_release_partial_locks_and_settle_once"),
            ])
            .env(CHILD_ROOT, &fixture.0)
            .env(super::super::OWNERS_DIRECTORY_ENV, fixture.0.join("owners"))
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let registry = expected(root.join("owners"));
    let first_project = root.join("first");
    let second_project = root.join("second");
    let (turn, scratch) = {
        let locks = [lock(&first_project), lock(&second_project)];
        let digest = "a".repeat(64);
        let mut charge = reserve(
            &registry.path,
            &registry,
            &locks,
            &[expected(root.clone())],
            (FLOOR, BUDGET, Some(&digest)),
            deadline(TIMEOUT)?,
            |_| Ok(CAPACITY),
        )?;
        let turn = charge.turn().to_owned();
        let scratch = root.join(format!("lean-writer-assets-{turn}"));
        fs::create_dir(&scratch)?;
        charge.bind_scratch_for_turn(expected(scratch.clone()), true, TIMEOUT)?;
        publish_idle_records(&locks, &turn)?;
        (turn, scratch)
    };

    let snapshot = WriterStorageTurn::inspect(&registry, &turn)?;
    {
        let second = lock(&second_project);
        let refusal = snapshot.try_locks(deadline(TIMEOUT)?).unwrap_err();
        assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock);
        let _first = lock(&first_project);
        drop(second);
    }

    let entry = registry.path.join(CHARGES).join(&turn);
    let original = fs::read(&entry)?;
    let no_pin = |_: &StorageRoot, _: bool| Err::<(), _>(io::Error::other("unexpected scratch pin"));
    let admitted: Charge = serde_json::from_slice(&original)?;
    let mut shortened = admitted.clone();
    shortened.secondary_locks.clear();
    let mut reordered = admitted.clone();
    let (path, device, inode) = reordered.secondary_locks.first_mut().unwrap();
    std::mem::swap(path, &mut reordered.primary);
    std::mem::swap(device, &mut reordered.device);
    std::mem::swap(inode, &mut reordered.inode);
    let mut changed_digest = admitted.clone();
    changed_digest.handoff_sha256 = Some("b".repeat(64));
    for changed in [shortened, reordered, changed_digest] {
        fs::write(&entry, serde_json::to_vec(&changed)?)?;
        let refusal = WriterStorageTurn::inspect(&registry, &turn).unwrap_err();
        assert!(refusal.to_string().contains("original turn scope"));
    }
    let mut without_scope = admitted;
    without_scope.scope_nonce = None;
    without_scope.secondary_locks.clear();
    fs::write(&entry, serde_json::to_vec(&without_scope)?)?;
    {
        let legacy = WriterStorageTurn::inspect(&registry, &turn)?;
        let locks = legacy.try_locks(deadline(TIMEOUT)?)?;
        let refusal = legacy.reclaim(&locks, no_pin, |()| Ok(())).unwrap_err();
        assert!(refusal.to_string().contains("lacks an original scope binding"));
    }
    {
        let second = [lock(&second_project)];
        let refusal = ensure_no_writer_storage_charge(&second, TIMEOUT).unwrap_err();
        assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock);
        let refusal = request(&root, &second).unwrap_err();
        assert!(refusal.to_string().contains("prior writer storage custody"));
    }
    fs::write(&entry, &original)?;

    {
        let retained = lock(&second_project);
        let read_write = |path: &Path| OpenOptions::new().read(true).write(true).open(path);
        let first_path = crate::shared_writer::project_lock_path(&first_project)?;
        let claimed = [
            (first_path.clone(), read_write(&first_path)?),
            (retained.0.clone(), read_write(&retained.0)?),
        ];
        let refusal = snapshot.reclaim(&claimed, no_pin, |()| Ok(())).unwrap_err();
        assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(fs::read(&entry)?, original);
    }

    {
        let locks = snapshot.try_locks(deadline(TIMEOUT)?)?;
        let secondary = &locks.last().unwrap().0;
        let idle = fs::read(secondary)?;
        let record: serde_json::Value = serde_json::from_slice(&idle)?;
        let mut no_disposition = record.clone();
        no_disposition.as_object_mut().unwrap().remove("process");
        let mut running = record.clone();
        running
            .as_object_mut()
            .unwrap()
            .insert("command_phase".to_owned(), serde_json::json!("running"));
        let mut with_process = record;
        with_process
            .as_object_mut()
            .unwrap()
            .insert("process".to_owned(), serde_json::json!({}));
        for incomplete in [no_disposition, running, with_process] {
            fs::write(secondary, format!("{incomplete}\n"))?;
            let refusal = snapshot.reclaim(&locks, no_pin, |()| Ok(())).unwrap_err();
            assert!(refusal.to_string().contains("missing durable Idle record"));
            assert_eq!(fs::read(&entry)?, original);
        }
        fs::write(secondary, idle)?;
    }

    {
        let mut reversed = snapshot.try_locks(deadline(TIMEOUT)?)?;
        assert_eq!(reversed.len(), 2);
        reversed.reverse();
        let pin_called = std::cell::Cell::new(false);
        let refusal = snapshot
            .reclaim(
                &reversed,
                |_, _| {
                    pin_called.set(true);
                    Ok(())
                },
                |()| Err(io::Error::other("reversed lock evidence reached cleanup")),
            )
            .unwrap_err();
        assert!(refusal.to_string().contains("lock union changed"));
        assert!(!pin_called.get());
        assert_eq!(fs::read(&entry)?, original);
        assert!(scratch.is_dir());
    }

    let contender = WriterStorageTurn::inspect(&registry, &turn)?;
    let remove_scratch = || {
        fs::remove_dir(&scratch)?;
        File::open(&root)?.sync_all()
    };
    let (cleaning, during_cleanup) = std::sync::mpsc::channel();
    let (checked, during_attempt) = std::sync::mpsc::channel();
    std::thread::scope(|scope| -> io::Result<()> {
        let winner = scope.spawn(move || {
            let locks = snapshot.try_locks(deadline(TIMEOUT)?)?;
            snapshot.reclaim(
                &locks,
                |_, marked| {
                    assert!(!marked);
                    Ok(())
                },
                |()| {
                    cleaning.send(()).map_err(io::Error::other)?;
                    during_attempt.recv_timeout(TIMEOUT).map_err(io::Error::other)?;
                    remove_scratch()
                },
            )
        });
        during_cleanup.recv_timeout(TIMEOUT).map_err(io::Error::other)?;
        let refusal = contender.try_locks(deadline(TIMEOUT)?).unwrap_err();
        assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock);
        checked.send(()).map_err(io::Error::other)?;
        winner.join().unwrap()
    })?;
    assert!(!scratch.try_exists()?);
    assert!(!entry.try_exists()?);
    assert!(contender.try_locks(deadline(TIMEOUT)?).is_err());
    assert!(WriterStorageTurn::inspect(&registry, &turn).is_err());
    Ok(())
}
