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
    fs::write(Path::new(&root).join("native-body"), std::process::id().to_string()).unwrap();
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
