//! Parent recovery when the native guardian dies with payload still running.

use super::{
    LeanWorkerOwnedGeneration, ParentLifetime, directory, native_worker_command, process, wait_for,
    write_guardian_script,
};
use std::fs;
use std::time::{Duration, Instant};

#[test]
fn surviving_parent_stops_native_group_after_guardian_sigkill() {
    guardian_failure_parent_recovery(false);
}

#[test]
fn deferred_parent_reaper_stops_native_group_after_guardian_sigkill() {
    guardian_failure_parent_recovery(true);
}

fn guardian_failure_parent_recovery(defer: bool) {
    use std::os::unix::fs::MetadataExt as _;
    let root = directory().canonicalize().unwrap();
    fs::create_dir(root.join("read-project")).unwrap();
    write_guardian_script(&root);
    let mut lifetime = ParentLifetime::prepare_guarded(&root.join("owners"), Duration::from_secs(2)).unwrap();
    let mut command = native_worker_command(&root);
    command.env("LEAN_NATIVE_GUARDIAN_FAILURE_ROOT", &root);
    lifetime.configure(&mut command);
    let tracker = process::LeanWorkerProcessTracker::new();
    let mut worker = tracker.spawn_guarded(&mut command, &mut lifetime).unwrap();
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
