//! Guarded worker startup and release through polling, drop, or pending cleanup.

use super::{
    BootstrapCustody, LeanWorkerNativePendingCleanup, LeanWorkerNativeScope, LeanWorkerNativeScopeLease,
    LeanWorkerNativeSpawnError, LeanWorkerOwnedGeneration, LeanWorkerOwnedProcess, ParentLifetime, directory,
    native_worker_command, wait_for, write_guardian_script,
};
use crate::process::{LeanWorkerExitObservation, LeanWorkerProcessTracker, OwnedChild};
use std::{fs, io, path::PathBuf, process::Command, time::Duration};

#[test]
fn external_guardian_ack_is_attached_to_the_actual_worker_before_body() {
    let (scope, mut worker) = StoppingScope::start_and_kill();
    wait_for(|| {
        worker.try_wait().unwrap();
        scope.tracker.active_processes() == 0
    });
    scope.check_repeat_wait_and_drop(worker);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn guarded_release_deferred_drop() {
    let (scope, worker) = StoppingScope::start_and_kill();
    drop(worker);
    wait_for(|| scope.tracker.active_processes() == 0);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn guarded_release_echild_sync() {
    let (scope, mut worker) = StoppingScope::start_and_kill();
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
    let (scope, worker) = StoppingScope::start_and_kill();
    reap_direct_child(&worker);
    drop(worker);
    wait_for(|| scope.tracker.active_processes() == 0);
    scope.assert_closed(ExitReceipt::ReapedOutsideTracker);
}

#[test]
fn native_pending_cleanup_retains_exact_closure() {
    let (scope, worker) = StoppingScope::start_and_kill();
    let error = LeanWorkerNativeSpawnError::AfterSpawn {
        source: io::Error::new(io::ErrorKind::TimedOut, "injected startup error"),
        pending: LeanWorkerNativePendingCleanup::new(worker, scope.custody.clone()),
    };
    let LeanWorkerNativeSpawnError::AfterSpawn { source, mut pending } = error else {
        unreachable!()
    };
    assert_eq!(source.kind(), io::ErrorKind::TimedOut);
    assert!(pending.cleanup_pending());
    wait_for(|| pending.try_wait().unwrap().is_some());
    assert!(pending.custody_closed().unwrap());
    assert!(!pending.cleanup_pending());
    assert_eq!(scope.tracker.active_processes(), 0);
    drop(pending);
    scope.assert_closed(ExitReceipt::Observed);
}

#[test]
fn native_spawn_error_before_target_preserves_source() {
    let root = directory().canonicalize().unwrap();
    let scope = LeanWorkerNativeScope::prepare(&root.join("owners"), Duration::from_secs(2), None).unwrap();
    let error = scope
        .spawn(
            &mut Command::new(root.join("absent-executable")),
            Duration::from_secs(1),
        )
        .unwrap_err();
    match error {
        LeanWorkerNativeSpawnError::BeforeSpawn(source) => assert_eq!(source.kind(), io::ErrorKind::NotFound),
        other => panic!("unexpected outcome: {other}"),
    }
    fs::remove_dir_all(root).unwrap();
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
    fn start_and_kill() -> (Self, OwnedChild) {
        let root = directory().canonicalize().unwrap();
        write_guardian_script(&root);
        let mut lifetime = ParentLifetime::prepare_guarded(&root.join("owners"), Duration::from_secs(2)).unwrap();
        let mut command = native_worker_command(&root);
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
        worker.acknowledge_lifetime().unwrap();
        wait_for(|| body.exists());
        assert_eq!(fs::read_to_string(&body).unwrap(), pid.to_string());

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
