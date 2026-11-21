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
fn request(root: &Path, locks: &[(PathBuf, File)]) -> io::Result<WriterStorageCharge> {
    let registry = expected(root.join("owners"));
    reserve(
        &registry.path,
        &registry,
        locks,
        &[expected(root.to_owned())],
        (FLOOR, BUDGET),
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
            (0, 1),
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
            (FLOOR, 1),
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
        (FLOOR, 1),
        deadline(TIMEOUT).unwrap(),
        |_| Ok(CAPACITY),
    )
    .unwrap();
    let refused = reserve(
        &registry.path,
        &registry,
        &second,
        &[expected(f.0.clone())],
        (FLOOR, 1),
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
        (0, 1),
        deadline(TIMEOUT).unwrap(),
        |_| Ok(CAPACITY),
    )
    .unwrap_err();
    assert!(refusal.to_string().contains("prior writer storage custody"));
}
