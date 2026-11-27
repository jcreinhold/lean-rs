//! Guarded worker startup and release through polling, drop, or pending cleanup.

use super::{
    BootstrapCustody, LeanWorkerNativePendingCleanup, LeanWorkerNativeScope, LeanWorkerNativeScopeLease,
    LeanWorkerNativeSpawnError, LeanWorkerOwnedGeneration, LeanWorkerOwnedProcess, ParentLifetime, directory,
    native_worker_command, wait_for, write_guardian_script,
};
use crate::process::{LeanWorkerExitObservation, LeanWorkerProcessTracker, OwnedChild};
use std::os::fd::AsRawFd as _;
use std::os::unix::{fs::PermissionsExt as _, process::CommandExt as _};
use std::path::{Path, PathBuf};
use std::{fs, io, process::Command, time::Duration};

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn native_pending_cleanup_remains_send_sync() {
    assert_send_sync::<LeanWorkerNativePendingCleanup>();
}

#[test]
fn external_guardian_ack_is_attached_to_the_actual_worker_before_body() {
    let (scope, mut worker) = StoppingScope::start_and_kill(true);
    wait_for(|| {
        worker.try_wait().unwrap();
        scope.tracker.active_processes() == 0
    });
    scope.check_repeat_wait_and_drop(worker);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn cancelling_attached_guarded_worker_before_ack_keeps_body_unstarted() {
    let (scope, mut worker) = StoppingScope::start_and_kill(false);
    wait_for(|| {
        worker.try_wait().unwrap();
        scope.tracker.active_processes() == 0
    });
    assert!(!scope.root.join("native-body").exists());
    scope.check_repeat_wait_and_drop(worker);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn guarded_release_deferred_drop() {
    let (scope, worker) = StoppingScope::start_and_kill(true);
    drop(worker);
    wait_for(|| scope.tracker.active_processes() == 0);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn guarded_release_echild_sync() {
    let (scope, mut worker) = StoppingScope::start_and_kill(true);
    reap_direct_child(&worker);
    wait_for(|| {
        assert!(worker.try_wait().is_err());
        scope.tracker.active_processes() == 0
    });
    scope.check_repeat_wait_and_drop(worker);
    scope.assert_closed(ExitReceipt::ReapedOutsideTracker);
}

#[test]
fn guarded_release_echild_deferred() {
    let (scope, worker) = StoppingScope::start_and_kill(true);
    reap_direct_child(&worker);
    drop(worker);
    wait_for(|| scope.tracker.active_processes() == 0);
    scope.assert_closed(ExitReceipt::ReapedOutsideTracker);
}

#[test]
fn native_pending_cleanup_retains_exact_closure() {
    let (scope, worker) = StoppingScope::start_and_kill(true);
    let error = LeanWorkerNativeSpawnError::AfterSpawn {
        source: io::Error::new(io::ErrorKind::TimedOut, "injected startup error"),
        pending: LeanWorkerNativePendingCleanup::new(worker, scope.custody.clone()),
    };
    assert!(matches!(
        error,
        LeanWorkerNativeSpawnError::AfterSpawn { ref source, .. }
            if source.kind() == io::ErrorKind::TimedOut
    ));
    let LeanWorkerNativeSpawnError::AfterSpawn { mut pending, .. } = error else {
        return;
    };
    assert!(pending.cleanup_pending());
    wait_for(|| pending.try_wait().unwrap().is_some());
    assert!(pending.custody_closed().unwrap());
    assert!(!pending.cleanup_pending());
    assert_eq!(scope.tracker.active_processes(), 0);
    drop(pending);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn dropping_native_pending_cleanup_delegates_exact_closure() {
    let (scope, worker) = StoppingScope::start_and_kill(true);
    let pending = LeanWorkerNativePendingCleanup::new(worker, scope.custody.clone());
    assert!(pending.cleanup_pending());
    drop(pending);
    wait_for(|| scope.tracker.active_processes() == 0);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn native_exec_failure_reaps_child_and_releases_registry_slot() {
    let root = directory().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let registry = root.join("owners");
    let slot = lean_toolchain::native_guardian::NativeCapacitySlot {
        key: "exec-failure".into(),
        scope: "main".into(),
        limit: 1,
    };
    let scope = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(2), Some(&slot)).unwrap();
    let record = scope.0.directory.join("owner.json");
    let socket_directory = scope.0.socket_directory.clone();
    // The named file exists and is executable. Its unique interpreter does not exist.
    let program = root.join("worker");
    let interpreter = root.join("absent-interpreter");
    let body = root.join("native-body");
    assert!(!interpreter.exists());
    fs::write(
        &program,
        format!("#!{}\nprintf started > '{}'\n", interpreter.display(), body.display()),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let witness = root.join("pre-exec");
    let mut command = Command::new(program);
    record_pre_exec_pid(&mut command, &witness);

    let error = scope.spawn(&mut command, Duration::from_secs(1)).unwrap_err();
    assert!(matches!(
        error,
        LeanWorkerNativeSpawnError::BeforeSpawn(source) if source.raw_os_error() == Some(libc::ENOENT)
    ));
    let pid = libc::pid_t::from_ne_bytes(fs::read(witness).unwrap().try_into().unwrap());
    assert!(pid > 1);
    // SAFETY: the private pre-exec witness names this invocation's child.
    // WNOHANG never waits for its exit, and the test discards status.
    let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    assert_eq!(waited, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    assert!(!body.exists());
    assert!(!socket_directory.exists());
    assert_owner_phase(&record, "preparing_closed");
    let successor = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(2), Some(&slot)).unwrap();
    assert!(!record.exists());
    drop(successor);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_dispatch_refusal_does_not_create_child() {
    let root = directory().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let scope = LeanWorkerNativeScope::prepare(&root.join("owners"), Duration::from_secs(2), None).unwrap();
    let record = scope.0.directory.join("owner.json");
    let socket_directory = scope.0.socket_directory.clone();
    let staging = record.with_extension("guardian-write");
    fs::write(&staging, b"dispatch write collision").unwrap();
    let witness = root.join("pre-exec");
    let mut command = Command::new("/usr/bin/true");
    record_pre_exec_pid(&mut command, &witness);

    let error = scope.spawn(&mut command, Duration::from_secs(1)).unwrap_err();
    assert!(matches!(
        error,
        LeanWorkerNativeSpawnError::BeforeSpawn(source) if source.kind() == io::ErrorKind::AlreadyExists
    ));
    assert!(fs::read(witness).unwrap().is_empty());
    assert_eq!(fs::read(staging).unwrap(), b"dispatch write collision");
    assert_owner_phase(&record, "preparing");
    assert!(!socket_directory.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn guarded_child_that_exits_before_hello_closes_its_record_and_slot() {
    let root = directory().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let registry = root.join("owners");
    let slot = lean_toolchain::native_guardian::NativeCapacitySlot {
        key: "early-exit".into(),
        scope: "main".into(),
        limit: 1,
    };
    let scope = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(2), Some(&slot)).unwrap();
    let record = scope.0.directory.join("owner.json");
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 3"]);
    let error = scope.spawn(&mut command, Duration::from_millis(200)).unwrap_err();
    assert!(matches!(
        error,
        LeanWorkerNativeSpawnError::AfterSpawn { ref source, .. } if source.kind() == io::ErrorKind::TimedOut
    ));
    let LeanWorkerNativeSpawnError::AfterSpawn { mut pending, .. } = error else {
        return;
    };
    assert_owner_phase(&record, "dispatching");
    // A build waits while any record is dispatching.
    let lock = root.join("project.lock");
    let reads_stopped =
        || lean_toolchain::native_guardian::protected_native_reads_stopped(&registry, &lock, (0, 0)).unwrap();
    assert!(!reads_stopped());
    wait_for(|| pending.try_wait().unwrap().is_some());
    assert_owner_phase(&record, "preparing_closed");
    assert!(pending.custody_closed().unwrap());
    assert!(reads_stopped());
    let successor = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(2), Some(&slot)).unwrap();
    assert!(!record.exists());
    drop(successor);
    drop(pending);
    fs::remove_dir_all(root).unwrap();
}

fn assert_owner_phase(record: &Path, expected: &str) {
    let owner: serde_json::Value = serde_json::from_slice(&fs::read(record).unwrap()).unwrap();
    assert_eq!(owner.get("phase").and_then(serde_json::Value::as_str), Some(expected));
}

fn record_pre_exec_pid(command: &mut Command, witness: &Path) {
    let witness = fs::File::create_new(witness).unwrap();
    // SAFETY: the closure owns the file, uses only async-signal-safe syscalls
    // and stack data after fork, and the buffer is valid for the whole write.
    unsafe {
        command.pre_exec(move || {
            let bytes = libc::getpid().to_ne_bytes();
            if libc::write(witness.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) == 4 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
        });
    }
}

#[derive(Clone, Copy)]
enum ExitReceipt {
    Observed,
    ReapedOutsideTracker,
}

struct StoppingScope {
    root: PathBuf,
    tracker: LeanWorkerProcessTracker,
    custody: BootstrapCustody,
    guardian: LeanWorkerOwnedProcess,
    pid: u32,
    observation: LeanWorkerExitObservation,
}

impl StoppingScope {
    fn start_and_kill(acknowledge: bool) -> (Self, OwnedChild) {
        let root = directory().canonicalize().unwrap();
        write_guardian_script(&root);
        let mut lifetime = ParentLifetime::prepare_guarded(&root.join("owners"), Duration::from_secs(2)).unwrap();
        let mut command = native_worker_command(&root);
        command.env("LEAN_STARTUP_FAULT_ROOT", &root);
        lifetime.configure(&mut command);
        let tracker = LeanWorkerProcessTracker::new();
        let mut worker = tracker.spawn_guarded(&mut command, &mut lifetime).unwrap();
        let custody = lifetime.custody.clone();
        let pid = worker.id();
        let lease = lifetime.accept(pid, Duration::from_secs(3)).unwrap();
        let owner: LeanWorkerOwnedGeneration = serde_json::from_slice(&fs::read(&lease.record).unwrap()).unwrap();
        assert_eq!(owner.version, 2);
        assert_eq!(owner.process.pid, pid);
        let guardian = owner.guardian.unwrap();
        assert_eq!(guardian.session, pid);
        assert_eq!(guardian.process_group, guardian.pid);
        assert_ne!(guardian.pid, pid);

        let body = root.join("native-body");
        assert!(!body.exists());
        worker.attach_lifetime(lease).unwrap();
        if acknowledge {
            worker.acknowledge_lifetime().unwrap();
            wait_for(|| body.exists());
            assert_eq!(fs::read_to_string(&body).unwrap(), pid.to_string());
        }

        let observation = worker.exit_observation();
        assert_eq!(tracker.active_processes(), 1);
        assert!(!custody.closed().unwrap());
        let mut public_lease = LeanWorkerNativeScopeLease(worker);
        assert!(!public_lease.cleanup_pending());
        assert!(!public_lease.custody_closed().unwrap());
        assert_eq!(tracker.active_processes(), 1);
        public_lease.kill().unwrap();
        assert!(public_lease.cleanup_pending());
        // Reading pending is nonpolling and cannot return the custody reservation.
        assert_eq!(tracker.active_processes(), 1);

        (
            Self {
                root,
                tracker,
                custody,
                guardian,
                pid,
                observation,
            },
            public_lease.0,
        )
    }

    fn check_repeat_wait_and_drop(&self, mut worker: OwnedChild) {
        // Repeated observation and Drop must not return the same slot twice.
        let _repeat = worker.try_wait();
        assert_eq!(self.tracker.active_processes(), 0);
        drop(worker);
    }

    fn assert_closed(self, receipt: ExitReceipt) {
        assert!(self.custody.closed().unwrap());
        let status_missing = matches!(receipt, ExitReceipt::ReapedOutsideTracker);
        assert_eq!(self.observation.scope_closed_without_status(), status_missing);
        assert_eq!(self.observation.confirmed_exit().is_some(), !status_missing);
        assert!(!lean_toolchain::process_group::group_present(self.pid).unwrap());
        assert!(!self.guardian.is_live().unwrap());
        assert!(!self.root.join("worker-rescue").exists());
        assert!(!self.root.join("guardian-rescue").exists());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(self.tracker.active_processes(), 0);
        fs::remove_dir_all(self.root).unwrap();
    }
}

fn reap_direct_child(worker: &OwnedChild) {
    let pid = i32::try_from(worker.id()).unwrap();
    let mut status = 0;
    // SAFETY: this test owns this exact direct child; steal only its wait
    // receipt, never signal an identity read from disk.
    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
}
