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
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let started = Instant::now();
    while !predicate() {
        assert!(started.elapsed() < Duration::from_secs(4), "condition did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn child(root: &Path) -> (crash_ownership::ParentLifetime, Child) {
    let lifetime = crash_ownership::ParentLifetime::prepare(root).unwrap();
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
    let mut descendant = Command::new("/bin/sleep").arg("0.8").spawn().unwrap();
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
    owned.attach_lifetime(lease).unwrap();
    owned.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("body").exists());
    fs::write(root.join("broker-ready"), owned.id().to_string()).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
#[test]
fn durable_ack_precedes_body_and_eof_keeps_descendant_capacity() {
    let root = directory();
    let (lifetime, child) = child(&root);
    let pid = child.id();
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let lease = lifetime.accept(pid, Duration::from_secs(2)).unwrap();
    let generation: crash_ownership::LeanWorkerOwnedGeneration =
        serde_json::from_slice(&fs::read(&lease.record).unwrap()).unwrap();
    assert_eq!(generation.process.pid, pid);
    assert!(!root.join("body").exists());
    assert_eq!(generation.scope, "inherited-process-group-v1");
    owned.attach_lifetime(lease).unwrap();
    owned.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("body").exists());
    owned.kill().unwrap();
    std::thread::sleep(Duration::from_millis(80));
    assert!(owned.try_wait().unwrap().is_none(), "descendant still owns the session");
    assert_eq!(tracker.active_processes(), 1);
    wait_for(|| owned.try_wait().unwrap().is_some());
    assert_eq!(tracker.active_processes(), 0);
    assert_eq!(
        generation.process.observe().unwrap(),
        crash_ownership::LeanWorkerOwnerObservation::Absent
    );
    drop(owned);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn parent_eof_before_ack_never_starts_body() {
    let root = directory();
    let (lifetime, child) = child(&root);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let lease = lifetime.accept(owned.id(), Duration::from_secs(2)).unwrap();
    owned.attach_lifetime(lease).unwrap();
    owned.kill().unwrap();
    wait_for(|| owned.try_wait().unwrap().is_some());
    assert!(!root.join("body").exists());
    assert_eq!(tracker.active_processes(), 0);
    drop(owned);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn actual_broker_sigkill_exits_worker_without_replaying_body() {
    let root = directory();
    let mut broker = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::fixture_broker_entry",
            "--nocapture",
        ])
        .env("PROOFS_LIFELINE_BROKER", &root)
        .env_remove("PROOFS_LIFELINE_CHILD")
        .spawn()
        .unwrap();
    wait_for(|| root.join("broker-ready").exists());
    let worker: u32 = fs::read_to_string(root.join("broker-ready")).unwrap().parse().unwrap();
    let identity = crash_ownership::LeanWorkerOwnedProcess::capture(worker)
        .unwrap()
        .unwrap();
    broker.kill().unwrap();
    broker.wait().unwrap();
    wait_for(|| identity.observe().unwrap() == crash_ownership::LeanWorkerOwnerObservation::Absent);
    assert_eq!(
        fs::read_to_string(root.join("body"))
            .unwrap()
            .split_whitespace()
            .count(),
        2
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unrelated_pid_churn_does_not_prevent_owned_session_release() {
    let root = directory();
    let (lifetime, child) = child(&root);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let lease = lifetime.accept(owned.id(), Duration::from_secs(2)).unwrap();
    owned.attach_lifetime(lease).unwrap();
    owned.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("body").exists());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let churn = std::sync::Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        while !churn.load(Ordering::Relaxed) {
            Command::new("/usr/bin/true").status().unwrap();
        }
    });
    owned.kill().unwrap();
    let started = Instant::now();
    let mut closed = false;
    while started.elapsed() < Duration::from_secs(3) {
        if owned.try_wait().unwrap().is_some() {
            closed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::Relaxed);
    thread.join().unwrap();
    assert!(closed, "unrelated process churn stalled actual stopped work");
    assert_eq!(tracker.active_processes(), 0);
    drop(owned);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fork_after_observation_and_parent_reap_cannot_hide_inherited_child() {
    let root = directory();
    let lifetime = crash_ownership::ParentLifetime::prepare(&root).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::fixture_child_entry",
            "--nocapture",
        ])
        .env("PROOFS_LIFELINE_CHILD", &root)
        .env("PROOFS_LIFELINE_LATE_FORK", "1");
    lifetime.configure(&mut command);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(command.spawn().unwrap());
    let lease = lifetime.accept(owned.id(), Duration::from_secs(2)).unwrap();
    let identity = lease.owner.clone();
    owned.attach_lifetime(lease).unwrap();
    owned.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("before-fork").exists());
    assert_eq!(
        identity.observe().unwrap(),
        crash_ownership::LeanWorkerOwnerObservation::Present
    );
    fs::write(root.join("fork-now"), "go").unwrap();
    wait_for(|| root.join("forked").exists());
    std::thread::sleep(Duration::from_millis(40));
    assert!(
        owned.try_wait().unwrap().is_none(),
        "kernel group still contains the late fork"
    );
    assert!(owned.cleanup_pending(), "reaped leader cannot accept another request");
    assert_eq!(tracker.active_processes(), 1);
    wait_for(|| owned.try_wait().unwrap().is_some());
    assert_eq!(tracker.active_processes(), 0);
    drop(owned);
    fs::remove_dir_all(root).unwrap();
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

#[test]
fn failed_ack_after_persist_never_starts_body_or_loses_attached_guard() {
    let root = directory();
    let (lifetime, child) = child(&root);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let lease = lifetime.accept(owned.id(), Duration::from_secs(2)).unwrap();
    assert!(lease.record.exists());
    owned.attach_lifetime(lease).unwrap();
    owned.kill().unwrap();
    assert!(owned.acknowledge_lifetime().is_err());
    wait_for(|| owned.try_wait().unwrap().is_some());
    assert!(!root.join("body").exists());
    assert_eq!(tracker.active_processes(), 0);
    drop(owned);
    fs::remove_dir_all(root).unwrap();
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
    lean_toolchain::native_guardian::run_guardian_cli().unwrap();
}

#[test]
fn native_worker_fixture_entry() {
    let Some(root) = std::env::var_os("LEAN_NATIVE_WORKER_FIXTURE") else {
        return;
    };
    // SAFETY: this isolated fixture is the actual worker before any body work.
    assert!(unsafe { libc::setsid() } > 0);
    let socket = std::env::var_os(SOCKET_ENV).unwrap();
    let nonce = std::env::var(NONCE_ENV).unwrap();
    let mut guardian = lean_toolchain::native_guardian::GuardianHandle::spawn(
        &Path::new(&root).join("guardian.sh"),
        Path::new(&socket),
        &nonce,
        Duration::from_secs(3),
    )
    .unwrap();
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
    fs::write(Path::new(&root).join("native-body"), std::process::id().to_string()).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn external_guardian_ack_is_attached_to_the_actual_worker_before_body() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = directory().canonicalize().unwrap();
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .replace('\'', "'\\''");
    fs::write(root.join("guardian.sh"),format!("#!/bin/sh\nexec '{executable}' --exact crash_ownership::lifecycle_tests::native_guardian_fixture_entry --nocapture\n")).unwrap();
    fs::set_permissions(root.join("guardian.sh"), fs::Permissions::from_mode(0o700)).unwrap();
    let lifetime = ParentLifetime::prepare_guarded(&root.join("owners"), Duration::from_secs(2)).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::native_worker_fixture_entry",
            "--nocapture",
        ])
        .env("LEAN_NATIVE_WORKER_FIXTURE", &root)
        .env("LEAN_NATIVE_GUARDIAN_FIXTURE", "1");
    lifetime.configure(&mut command);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut worker = tracker.own(command.spawn().unwrap());
    let actual_pid = worker.id();
    let lease = lifetime.accept(actual_pid, Duration::from_secs(3)).unwrap();
    let owner: LeanWorkerOwnedGeneration = serde_json::from_slice(&fs::read(&lease.record).unwrap()).unwrap();
    assert_eq!(owner.version, 2);
    assert_eq!(owner.process.pid, actual_pid);
    let guardian = owner.guardian.unwrap();
    assert_eq!(guardian.session, actual_pid);
    assert_eq!(guardian.process_group, guardian.pid);
    assert_ne!(guardian.pid, actual_pid);
    assert!(!root.join("native-body").exists());
    worker.attach_lifetime(lease).unwrap();
    worker.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("native-body").exists());
    assert_eq!(
        fs::read_to_string(root.join("native-body")).unwrap(),
        actual_pid.to_string()
    );
    worker.kill().unwrap();
    wait_for(|| worker.try_wait().unwrap().is_some());
    assert_eq!(tracker.active_processes(), 0);
    assert!(!lean_toolchain::process_group::group_present(actual_pid).unwrap());
    drop(worker);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_wait_can_release_closed_scope_capacity_without_an_exit_receipt() {
    failed_wait_scope_recovery(true);
}

#[test]
fn live_owner_reports_terminal_scope_closure_without_a_lost_wait_status() {
    failed_wait_scope_recovery(false);
}

fn failed_wait_scope_recovery(defer: bool) {
    let root = directory();
    let (lifetime, child) = child(&root);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let lease = lifetime.accept(owned.id(), Duration::from_secs(2)).unwrap();
    owned.attach_lifetime(lease).unwrap();
    owned.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("body").exists());
    let observation = owned.exit_observation();
    owned.kill().unwrap();
    let pid = i32::try_from(owned.id()).unwrap();
    let mut status = 0;
    // SAFETY: this fixture owns this exact unreaped direct child. Deliberately
    // steal its wait status to exercise ECHILD without signaling a recorded PID.
    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
    assert!(owned.try_wait().is_err());
    assert_eq!(tracker.active_processes(), 1, "the descendant still occupies the group");
    assert!(observation.confirmed_exit().is_none());
    assert!(
        !observation.scope_closed_without_status(),
        "a live descendant prevents terminal observation"
    );
    if defer {
        drop(owned);
        wait_for(|| tracker.active_processes() == 0);
    } else {
        wait_for(|| {
            let _unavailable_status = owned.try_wait();
            tracker.active_processes() == 0
        });
        drop(owned);
    }
    assert!(
        observation.confirmed_exit().is_none(),
        "physical closure cannot fabricate a lost exit status"
    );
    assert!(
        observation.scope_closed_without_status(),
        "retained lifecycle watching can finish without inventing a status"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn surviving_parent_stops_native_group_after_guardian_sigkill() {
    guardian_failure_parent_recovery(false);
}
#[test]
fn deferred_parent_reaper_stops_native_group_after_guardian_sigkill() {
    guardian_failure_parent_recovery(true);
}

fn guardian_failure_parent_recovery(defer: bool) {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let root = directory().canonicalize().unwrap();
    fs::create_dir(root.join("read-project")).unwrap();
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .replace('\'', "'\\''");
    fs::write(root.join("guardian.sh"), format!("#!/bin/sh\nexec '{executable}' --exact crash_ownership::lifecycle_tests::native_guardian_fixture_entry --nocapture\n")).unwrap();
    fs::set_permissions(root.join("guardian.sh"), fs::Permissions::from_mode(0o700)).unwrap();
    let lifetime = ParentLifetime::prepare_guarded(&root.join("owners"), Duration::from_secs(2)).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::native_worker_fixture_entry",
            "--nocapture",
        ])
        .env("LEAN_NATIVE_WORKER_FIXTURE", &root)
        .env("LEAN_NATIVE_GUARDIAN_FIXTURE", "1")
        .env("LEAN_NATIVE_GUARDIAN_FAILURE_ROOT", &root);
    lifetime.configure(&mut command);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut worker = tracker.own(command.spawn().unwrap());
    let lease = lifetime.accept(worker.id(), Duration::from_secs(3)).unwrap();
    let record: LeanWorkerOwnedGeneration = serde_json::from_slice(&fs::read(&lease.record).unwrap()).unwrap();
    let guardian = record.guardian.unwrap();
    let observation = worker.exit_observation();
    worker.attach_lifetime(lease).unwrap();
    worker.acknowledge_lifetime().unwrap();
    wait_for(|| fs::metadata(root.join("native-heartbeat")).is_ok_and(|metadata| metadata.len() > 0));
    let lock_path = lean_toolchain::shared_writer::project_lock_path(&root.join("read-project")).unwrap();
    let lock = fs::OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
    assert!(matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    fs::write(root.join("kill-guardian-now"), b"self SIGKILL").unwrap();
    wait_for(|| !guardian.is_live().unwrap());
    wait_for(|| lock.try_lock().is_ok());
    let metadata = lock.metadata().unwrap();
    assert!(
        !lean_toolchain::native_guardian::protected_native_reads_stopped(
            &root.join("owners"),
            &lock_path,
            (metadata.dev(), metadata.ino())
        )
        .unwrap()
    );
    let started = Instant::now();
    if defer {
        drop(worker);
        wait_for(|| tracker.active_processes() == 0);
    } else {
        wait_for(|| worker.try_wait().unwrap().is_some());
        drop(worker);
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "parent fallback must precede the leaf's 4 second self-expiry"
    );
    assert_eq!(tracker.active_processes(), 0);
    assert!(observation.confirmed_exit().is_some_and(|status| !status.success()));
    assert!(!observation.scope_closed_without_status());
    assert!(
        lean_toolchain::native_guardian::protected_native_reads_stopped(
            &root.join("owners"),
            &lock_path,
            (metadata.dev(), metadata.ino())
        )
        .unwrap()
    );
    let before = fs::metadata(root.join("native-heartbeat")).unwrap().len();
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(before, fs::metadata(root.join("native-heartbeat")).unwrap().len());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn preparing_scope_owns_one_private_input_directory_and_cleans_it_on_cancel() {
    use std::os::unix::fs::MetadataExt as _;
    let root = directory().canonicalize().unwrap();
    let scope = LeanWorkerNativeScope::prepare(&root.join("owners"), Duration::from_secs(1), None).unwrap();
    let inputs = scope.input_directory().unwrap();
    assert_eq!(inputs, scope.input_directory().unwrap());
    assert_eq!(fs::metadata(&inputs).unwrap().mode() & 0o777, 0o700);
    fs::create_dir(inputs.join("lake-cache")).unwrap();
    fs::write(inputs.join("lake-cache").join("source.lean"), b"owned source").unwrap();
    let generation = inputs.parent().unwrap().to_path_buf();
    assert!(generation.join("owner.json").exists());
    drop(scope);
    assert!(!generation.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelled_preparation_budget_failure_can_recover_while_parent_stays_alive() {
    let root = directory().canonicalize().unwrap();
    let registry = root.join("owners");
    let slot = lean_toolchain::native_guardian::NativeCapacitySlot {
        key: "utility".to_owned(),
        scope: "preparation".to_owned(),
        limit: 1,
    };
    let scope = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(1), Some(&slot)).unwrap();
    let inputs = scope.input_directory().unwrap();
    let record = inputs.parent().unwrap().join("owner.json");
    for index in 0..4097 {
        fs::write(inputs.join(format!("{index:04}")), []).unwrap();
    }
    drop(scope);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(
        value.get("phase").and_then(serde_json::Value::as_str),
        Some("preparing_closed")
    );
    assert!(value.get("process").is_some_and(serde_json::Value::is_null));
    assert!(
        LeanWorkerOwnedProcess::capture(std::process::id())
            .unwrap()
            .unwrap()
            .is_live()
            .unwrap()
    );
    let next = lean_toolchain::native_guardian::NativeCapacitySlot {
        key: "next".to_owned(),
        ..slot
    };
    let replacement = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(1), Some(&next)).unwrap();
    assert!(!record.exists());
    assert!(!inputs.exists());
    drop(replacement);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fragmented_late_hello_waits_without_inherited_nonblocking_failure() {
    let root = directory();
    fs::write(root.join("fragment-hello"), []).unwrap();
    let (lifetime, child) = child(&root);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let started = Instant::now();
    let lease = lifetime.accept(owned.id(), Duration::from_secs(1)).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert!(!root.join("body").exists());
    owned.attach_lifetime(lease).unwrap();
    owned.acknowledge_lifetime().unwrap();
    wait_for(|| root.join("body").exists());
    owned.kill().unwrap();
    wait_for(|| owned.try_wait().unwrap().is_some());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn trickled_hello_cannot_restart_the_absolute_startup_budget() {
    let root = directory();
    fs::write(root.join("trickle-hello"), []).unwrap();
    let (lifetime, child) = child(&root);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut owned = tracker.own(child);
    let started = Instant::now();
    let error = lifetime.accept(owned.id(), Duration::from_millis(90)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_millis(350));
    assert!(!root.join("body").exists());
    owned.kill().unwrap();
    wait_for(|| owned.try_wait().unwrap().is_some());
    fs::remove_dir_all(root).unwrap();
}
