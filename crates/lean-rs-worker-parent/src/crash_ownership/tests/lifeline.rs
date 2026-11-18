//! Startup ACK, parent death, and reaping for the inherited lifeline.

use super::{Ordering, child, crash_ownership, directory, process, wait_for};
use std::fs;
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

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
