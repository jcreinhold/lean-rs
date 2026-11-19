#![cfg(unix)]
#![allow(clippy::unwrap_used, reason = "tests own their fake child and temporary directory")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lean_rs_worker_parent::{LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerProcessTracker};
use lean_rs_worker_protocol::protocol::{Frame, Message, PROTOCOL_VERSION, Response};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn fixture(skip_authentication: bool) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "lean-worker-lifetime-integration-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let script = root.join("worker.py");
    let handshake = serde_json::to_string(&Frame::new(Message::Handshake {
        worker_version: "fake-owned-lifetime".to_owned(),
        protocol_version: PROTOCOL_VERSION,
    }))
    .unwrap();
    let health = serde_json::to_string(&Frame::new(Message::Response(Response::HealthOk))).unwrap();
    let terminating = serde_json::to_string(&Frame::new(Message::Response(Response::Terminating))).unwrap();
    let source = format!(
        r"#!/usr/bin/env python3
import json, os, pathlib, socket, struct, sys, threading, time
root = pathlib.Path(__file__).parent
if {skip_authentication}:
    time.sleep(60)
time.sleep(float(os.environ.get('LEAN_RS_TEST_LIFELINE_DELAY_SECS', '0')))
os.setsid()
connection = socket.socket(socket.AF_UNIX)
connection.connect(os.environ['LEAN_RS_WORKER_PARENT_SOCKET'])
connection.sendall((os.environ['LEAN_RS_WORKER_PARENT_NONCE'] + ' ' + str(os.getpid()) + '\n').encode())
lifetime_reader = connection.makefile('rb')
ack = lifetime_reader.readline()
if ack != b'durable-owner-ready\n':
    os._exit(126)
records = list((root / 'owners').glob('worker-*/owner.json'))
assert len(records) == 1
owner = json.loads(records[0].read_text())
assert owner['process']['pid'] == os.getpid()
(root / 'body').write_text(str(os.getpid()))
def eof():
    connection.recv(1)
    os._exit(125)
threading.Thread(target=eof, daemon=True).start()
def send(value):
    payload = value.encode()
    sys.stdout.buffer.write(struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()
def receive():
    header = sys.stdin.buffer.read(4)
    if len(header) != 4:
        os._exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack('>I', header)[0]))
time.sleep(float(os.environ.get('LEAN_RS_TEST_READY_DELAY_SECS', '0')))
send({handshake:?})
receive()
(root / 'configured').write_text('ready accepted')
while True:
    frame = receive()
    op = frame['message']['body']['op']
    if op == 'health':
        send({health:?})
    elif op == 'terminate':
        send({terminating:?})
        break
",
        skip_authentication = if skip_authentication { "True" } else { "False" }
    );
    fs::write(&script, source).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    (root, script)
}

#[test]
fn authenticated_ownership_is_attached_before_startup_is_released() {
    let (root, script) = fixture(false);
    let tracker = LeanWorkerProcessTracker::new();
    let mut worker = LeanWorker::spawn(
        &LeanWorkerConfig::new(script)
            .process_tracker(tracker.clone())
            .lifetime_directory(root.join("owners"))
            .startup_timeout(Duration::from_secs(2)),
    )
    .unwrap();
    assert!(root.join("body").exists());
    worker.health().unwrap();
    assert_eq!(tracker.active_processes(), 1);
    let report = worker.shutdown().unwrap();
    assert!(report.exit.success);
    assert_eq!(tracker.active_processes(), 0);
    assert_eq!(fs::read_dir(root.join("owners")).unwrap().count(), 1);
    assert!(
        !fs::read_dir(root.join("owners"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("owner.json")
            .exists()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_authentication_never_releases_body_and_reaps_owned_child() {
    let (root, script) = fixture(true);
    let tracker = LeanWorkerProcessTracker::new();
    let started = Instant::now();
    let error = LeanWorker::spawn(
        &LeanWorkerConfig::new(script)
            .process_tracker(tracker.clone())
            .lifetime_directory(root.join("owners"))
            .startup_timeout(Duration::from_millis(50)),
    )
    .unwrap_err();
    assert!(error.to_string().contains("worker parent lifeline startup failed"));
    let elapsed = started.elapsed();
    assert!(!root.join("body").exists());
    assert!(!root.join("configured").exists());
    assert_eq!(tracker.active_processes(), 0);
    fs::remove_dir_all(root).unwrap();
    assert!(elapsed < Duration::from_secs(2), "startup and reap took {elapsed:?}");
}

#[test]
fn startup_budget_covers_lifeline_and_ready() {
    let (root, script) = fixture(false);
    let tracker = LeanWorkerProcessTracker::new();
    let budget = Duration::from_secs(2);
    let started = Instant::now();
    let error = LeanWorker::spawn(
        &LeanWorkerConfig::new(script)
            .process_tracker(tracker.clone())
            .lifetime_directory(root.join("owners"))
            .startup_timeout(budget)
            .env("LEAN_RS_TEST_LIFELINE_DELAY_SECS", "1.2")
            .env("LEAN_RS_TEST_READY_DELAY_SECS", "1.2"),
    )
    .unwrap_err();
    assert!(
        matches!(&error, LeanWorkerError::Timeout { operation: "startup", duration, .. } if *duration == budget),
        "lifeline and READY must share the startup budget: {error:?}"
    );
    assert!(
        started.elapsed() < budget.saturating_add(Duration::from_secs(1)),
        "startup must not grant READY a second full budget"
    );
    assert!(
        root.join("body").exists(),
        "the child authenticated before its delayed READY"
    );
    assert!(
        !root.join("configured").exists(),
        "timed-out startup cannot release protocol work"
    );
    assert_eq!(
        tracker.active_processes(),
        0,
        "startup timeout must reap the owned child"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn protected_native_spawn_refuses_a_registry_outside_the_writer_binding() {
    let (root, script) = fixture(false);
    let tracker = LeanWorkerProcessTracker::new();
    let error = LeanWorker::spawn(
        &LeanWorkerConfig::new(script)
            .process_tracker(tracker.clone())
            .lifetime_directory(root.join("unbound-owners"))
            .native_guardian(root.join("guardian-not-spawned"))
            .shared_artifact_reads(true),
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("differs from the managed writer ownership registry")
    );
    assert_eq!(tracker.active_processes(), 0);
    assert!(!root.join("body").exists());
    assert!(!root.join("unbound-owners").exists());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn actor_capacity_without_guardian_refuses_before_child_spawn() {
    let (root, script) = fixture(false);
    let tracker = LeanWorkerProcessTracker::new();
    let error = LeanWorker::spawn(
        &LeanWorkerConfig::new(script)
            .process_tracker(tracker.clone())
            .native_capacity_slot("actor", "main", 1),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("capacity requires the independent guardian"));
    assert_eq!(tracker.active_processes(), 0);
    assert!(!root.join("body").exists());
    fs::remove_dir_all(root).unwrap();
}
