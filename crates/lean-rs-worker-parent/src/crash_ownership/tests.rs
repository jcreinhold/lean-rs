//! Shared subprocess fixtures for crash-ownership scenarios.

use super::*;
use crate::{crash_ownership, process};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn directory() -> PathBuf {
    let path = PathBuf::from("/tmp").join(format!(
        "proofs-lifeline-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

thread_local! {
    pub(super) static NEXT_SOCKET_DIRECTORY: std::cell::Cell<Option<PathBuf>> = const { std::cell::Cell::new(None) };
}

#[test]
fn preparing_socket_collision_preserves_foreign_listener_and_releases_slot() {
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    let root = directory().canonicalize().unwrap();
    let registry = root.join("owners");
    let timeout = Duration::from_secs(2);
    let resident_slot = native_guardian::NativeCapacitySlot {
        key: "resident".into(),
        scope: "main".into(),
        limit: 1,
    };
    let resident = LeanWorkerNativeScope::prepare(&registry, timeout, Some(&resident_slot)).unwrap();
    let resident_record = resident.0.directory.join("owner.json");
    let original = fs::read(&resident_record).unwrap();
    let identity = |path: &Path| {
        let m = fs::symlink_metadata(path).unwrap();
        (m.dev(), m.ino())
    };
    let resident_identity = identity(&resident_record);
    let foreign = root.join("foreign");
    fs::create_dir(&foreign).unwrap();
    let socket = foreign.join("parent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let before = (identity(&foreign), identity(&socket));
    let slot = native_guardian::NativeCapacitySlot {
        key: "candidate".into(),
        scope: "preparation".into(),
        limit: 1,
    };
    NEXT_SOCKET_DIRECTORY.set(Some(foreign.clone()));
    let error = LeanWorkerNativeScope::prepare(&registry, timeout, Some(&slot)).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
    assert_eq!((identity(&foreign), identity(&socket)), before);
    let _client = UnixStream::connect(&socket).unwrap();
    let _accepted = listener.accept().unwrap();
    let generations = || {
        fs::read_dir(&registry)
            .unwrap()
            .map(Result::unwrap)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>()
    };
    assert_eq!(generations(), std::slice::from_ref(&resident.0.directory));
    let successor = LeanWorkerNativeScope::prepare(&registry, timeout, Some(&slot)).unwrap();
    assert!(successor.0.directory.join("owner.json").is_file());
    assert_eq!(identity(&resident_record), resident_identity);
    assert_eq!(fs::read(&resident_record).unwrap(), original);
    drop(successor);
    drop(resident);
    drop(listener);
    fs::remove_dir_all(root).unwrap();
}

fn wait_for(mut predicate: impl FnMut() -> bool) {
    let started = Instant::now();
    while !predicate() {
        assert!(started.elapsed() < Duration::from_secs(4), "condition did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn child(root: &Path) -> (crash_ownership::ParentLifetime, Child) {
    let owner_root = if root.join("input-custody-mode").exists() {
        root.join("owners")
    } else {
        root.to_path_buf()
    };
    let lifetime = crash_ownership::ParentLifetime::prepare(&owner_root).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::fixture_child_entry",
            "--nocapture",
        ])
        .env("PROOFS_LIFELINE_CHILD", root)
        .env_remove("PROOFS_LIFELINE_BROKER");
    lifetime.configure(&mut command);
    (lifetime, command.spawn().unwrap())
}

#[test]
fn fixture_child_entry() {
    let Some(root) = std::env::var_os("PROOFS_LIFELINE_CHILD") else {
        return;
    };
    if fake_child_bootstrap().is_err() {
        return;
    }
    if std::env::var_os("PROOFS_LIFELINE_LATE_FORK").is_some() {
        fs::write(Path::new(&root).join("before-fork"), std::process::id().to_string()).unwrap();
        wait_for(|| Path::new(&root).join("fork-now").exists());
        let mut descendant = Command::new("/bin/sleep").arg("0.8").spawn().unwrap();
        let descendant_pid = descendant.id();
        std::thread::spawn(move || {
            let _exit = descendant.wait();
        });
        fs::write(Path::new(&root).join("forked"), descendant_pid.to_string()).unwrap();
        return;
    }
    let mut command = if Path::new(&root).join("input-custody-mode").exists() {
        let mut command = Command::new("python3");
        command
            .args([
                "-c",
                r"
import os,pathlib,sys,time
root=pathlib.Path(sys.argv[1])
for fd in range(3,4096):
    try: os.close(fd)
    except OSError: pass
source=pathlib.Path((root/'input-path').read_text())/'source.lean'
with (root/'input-reading').open('ab',buffering=0) as output:
    until=time.monotonic()+3
    while time.monotonic()<until:
        assert source.read_bytes()==b'owned generation input'
        output.write(b'x')
        time.sleep(.01)
",
            ])
            .arg(&root);
        command
    } else {
        let mut command = Command::new("/bin/sleep");
        command.arg("0.8");
        command
    };
    let mut descendant = command.spawn().unwrap();
    let descendant_pid = descendant.id();
    std::thread::spawn(move || {
        let _exit = descendant.wait();
    });
    fs::write(
        Path::new(&root).join("body"),
        format!("{} {}", std::process::id(), descendant_pid),
    )
    .unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn fixture_broker_entry() {
    let Some(root) = std::env::var_os("PROOFS_LIFELINE_BROKER") else {
        return;
    };
    let root = Path::new(&root);
    let (lifetime, child) = child(root);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let lease = lifetime.accept(owned.id(), Duration::from_secs(2)).unwrap();
    if root.join("input-custody-mode").exists() {
        let inputs = lean_toolchain::native_guardian::create_generation_inputs(lease.record.parent().unwrap()).unwrap();
        fs::write(inputs.join("source.lean"), b"owned generation input").unwrap();
        fs::write(root.join("input-path"), inputs.to_string_lossy().as_bytes()).unwrap();
    }
    owned.attach_lifetime(lease).unwrap();
    owned.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("body").exists());
    fs::write(root.join("broker-ready"), owned.id().to_string()).unwrap();
    let started = Instant::now();
    loop {
        if root.join("input-custody-mode").exists() && started.elapsed() > Duration::from_secs(6) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A deterministic child implementation of the lifeline protocol, before
/// any test body. Production bootstrap is tested in the child crate.
fn fake_child_bootstrap() -> std::io::Result<()> {
    use std::io::{BufRead, Read, Write};
    // SAFETY: isolated test process creates its own session before work.
    if unsafe { libc::setsid() } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let socket = std::env::var_os(SOCKET_ENV).ok_or_else(|| std::io::Error::other("missing fixture parent socket"))?;
    let nonce = std::env::var(NONCE_ENV).map_err(std::io::Error::other)?;
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    let hello = format!("{nonce} {}\n", std::process::id());
    let root = PathBuf::from(std::env::var_os("PROOFS_LIFELINE_CHILD").unwrap());
    if root.join("trickle-hello").exists() {
        for byte in hello.as_bytes() {
            stream.write_all(&[*byte])?;
            std::thread::sleep(Duration::from_millis(40));
        }
    } else if root.join("fragment-hello").exists() {
        let (first, rest) = hello.as_bytes().split_first().unwrap();
        stream.write_all(&[*first])?;
        std::thread::sleep(Duration::from_millis(80));
        stream.write_all(rest)?;
    } else {
        stream.write_all(hello.as_bytes())?;
    }
    let mut ready = String::new();
    std::io::BufReader::new(&stream).take(64).read_line(&mut ready)?;
    if ready != "durable-owner-ready\n" {
        return Err(std::io::Error::other("no startup ACK"));
    }
    std::thread::spawn(move || {
        let mut byte = [0];
        let _read = stream.read(&mut byte);
        // SAFETY: terminate only this isolated fixture process on EOF.
        unsafe { libc::_exit(125) }
    });
    Ok(())
}

fn write_guardian_script(root: &Path) {
    let executable = std::env::current_exe().unwrap();
    let escaped = executable.to_string_lossy().replace('\'', "'\\''");
    let test = "crash_ownership::lifecycle_tests::native_guardian_fixture_entry";
    let script = format!("#!/bin/sh\nexec '{escaped}' --exact {test} --nocapture\n");
    let path = root.join("guardian.sh");
    fs::write(&path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn native_worker_command(root: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::native_worker_fixture_entry",
            "--nocapture",
        ])
        .env("LEAN_NATIVE_WORKER_FIXTURE", root)
        .env("LEAN_NATIVE_GUARDIAN_FIXTURE", "1");
    command
}

#[test]
fn native_guardian_fixture_entry() {
    if std::env::var_os("LEAN_NATIVE_GUARDIAN_FIXTURE").is_none() {
        return;
    }
    if let Some(root) = std::env::var_os("LEAN_NATIVE_GUARDIAN_FAILURE_ROOT") {
        std::thread::spawn(move || {
            let root = PathBuf::from(root);
            while !root.join("kill-guardian-now").exists() {
                std::thread::sleep(Duration::from_millis(5));
            }
            // SAFETY: this isolated guardian fixture signals only its own live
            // process, simulating SIGKILL without targeting any stored PID.
            unsafe {
                libc::raise(libc::SIGKILL);
            }
        });
    }
    startup_faults::fixture_watchdog("guardian");
    let result = lean_toolchain::native_guardian::run_guardian_cli();
    startup_faults::pause_drained_guardian(result).unwrap();
}

#[test]
fn native_worker_fixture_entry() {
    let Some(root) = std::env::var_os("LEAN_NATIVE_WORKER_FIXTURE") else {
        return;
    };
    startup_faults::fixture_watchdog("worker");
    // SAFETY: this isolated fixture is the actual worker before any body work.
    assert!(unsafe { libc::setsid() } > 0);
    let socket = std::env::var_os(SOCKET_ENV).unwrap();
    let nonce = std::env::var(NONCE_ENV).unwrap();
    startup_faults::collide_bootstrap_publication();
    let mut guardian = lean_toolchain::native_guardian::GuardianHandle::spawn(
        &Path::new(&root).join("guardian.sh"),
        Path::new(&socket),
        &nonce,
        Duration::from_secs(3),
    )
    .unwrap();
    if std::env::var_os("LEAN_NATIVE_READ_DEADLINE_FIXTURE").is_some() {
        guarded_release::run_reader_after_go(Path::new(&root), &mut guardian);
        return;
    }
    #[cfg(target_os = "macos")]
    startup_traces::run_lean_body(Path::new(&root), &mut guardian);
    if std::env::var_os("LEAN_NATIVE_GUARDIAN_FAILURE_ROOT").is_some() {
        use std::os::fd::AsFd as _;
        use std::os::unix::fs::MetadataExt as _;
        let project = Path::new(&root).join("read-project");
        let lock = lean_toolchain::shared_writer::project_lock_path(&project).unwrap();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
            .unwrap();
        file.try_lock_shared().unwrap();
        let metadata = file.metadata().unwrap();
        guardian
            .retain_reads(
                &[lean_toolchain::native_guardian::GuardianReadLoan {
                    domain: lean_toolchain::native_guardian::NativeReadDomain {
                        project_root: project,
                        lock_path: lock,
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        epoch: "fixture-epoch".to_owned(),
                    },
                    descriptor: file.as_fd(),
                }],
                Duration::from_secs(2),
            )
            .unwrap();
        drop(file);
        let mut leaf = Command::new("python3")
            .args([
                "-c",
                r"
import os,pathlib,signal,sys,time
signal.signal(signal.SIGTERM,signal.SIG_IGN)
signal.alarm(4)
for fd in range(3,4096):
    try: os.close(fd)
    except OSError: pass
root=pathlib.Path(sys.argv[1])
with (root/'native-heartbeat').open('ab',buffering=0) as output:
    while True: output.write(b'x');time.sleep(.005)
",
            ])
            .arg(&root)
            .spawn()
            .unwrap();
        // The fixture's worker can die before this waiter; the independent
        // parent fallback owns and kills that inherited group in the test.
        std::thread::spawn(move || {
            let _status = leaf.wait();
        });
    }
    let body = Path::new(&root).join("native-body");
    let pending = body.with_extension("pending");
    fs::write(&pending, std::process::id().to_string()).unwrap();
    fs::rename(pending, body).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[path = "tests/guarded_release.rs"]
mod guarded_release;
#[path = "tests/guardian_failure.rs"]
mod guardian_failure;
#[path = "tests/input_custody.rs"]
mod input_custody;
#[path = "tests/lifeline.rs"]
mod lifeline;

#[path = "tests/startup_faults.rs"]
pub(super) mod startup_faults;

#[cfg(target_os = "macos")]
#[path = "tests/startup_traces.rs"]
mod startup_traces;

#[test]
fn explicit_relative_socket_root_fails_before_owner_creation() {
    let owners = Path::new("unused-relative-owner-root");
    let error = LeanWorkerNativeScope::prepare_with_socket_directory(
        owners,
        Duration::from_secs(1),
        None,
        Path::new("relative"),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
#[ignore = "requires explicit private canonical short LEAN_RS_TEST_SOCKET_ROOT outside temporary storage"]
fn explicit_socket_root_validation_and_cleanup() {
    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn create(path: PathBuf) -> Self {
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            // This test starts no children and owns only the directory it created.
            let _removed = fs::remove_dir_all(&self.0);
        }
    }
    let root = PathBuf::from(std::env::var_os("LEAN_RS_TEST_SOCKET_ROOT").expect("select private socket root"));
    let socket = root.join("check.sock");
    validate_socket_root(&root, &socket).unwrap();
    let too_long = root.join("x".repeat(256));
    assert!(validate_socket_root(&root, &too_long).is_err());
    for guarded in [false, true] {
        let owner_root = root.join(format!("owners-{}-{guarded}", std::process::id()));
        let owned = TestDirectory::create(owner_root.clone());
        let lifetime = if guarded {
            LeanWorkerNativeScope::prepare_with_socket_directory(&owner_root, Duration::from_secs(1), None, &root)
                .unwrap()
                .0
        } else {
            ParentLifetime::prepare_with_socket_directory(&owner_root, Some(&root), false, Duration::from_secs(1), None)
                .unwrap()
        };
        let directory = lifetime.socket_directory.clone();
        assert_eq!(directory.parent(), Some(root.as_path()));
        assert!(directory.join("parent.sock").exists());
        drop(lifetime);
        assert!(!directory.exists());
        drop(owned);
        assert!(!owner_root.exists());
    }
    let public = root.join(format!("public-{}", std::process::id()));
    let owned = TestDirectory::create(public.clone());
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        validate_socket_root(&public, &public.join("s")).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    drop(owned);
    assert!(!public.exists());
}
