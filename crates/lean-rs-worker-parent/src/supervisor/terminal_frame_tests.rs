//! Tests of terminal frames that the parent dequeues after the request deadline
//! or that carry an earlier request's tag. A Python fake worker speaks the real
//! frame protocol, and `observe_fixture_terminal` alters each terminal event
//! before the supervisor reads it. These tests check frame handling only, not
//! native admission or custody.
#![allow(
    clippy::expect_used,
    clippy::wildcard_imports,
    reason = "private tests inspect the owning supervisor and fail with fixture context"
)]

use super::*;
use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::AtomicU64;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

const CHILD: &str = r"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['LEAN_RS_FRAME_FIXTURE'])

def record(operation):
    with (root / 'frames.jsonl').open('a') as log:
        log.write(json.dumps({'pid': os.getpid(), 'op': operation}) + '\n')
        log.flush()

def emit(name):
    sys.stdout.buffer.write((root / name).read_bytes())
    sys.stdout.buffer.flush()

def exact(count):
    result = bytearray()
    while len(result) < count:
        part = sys.stdin.buffer.read(count - len(result))
        if not part:
            return None
        result.extend(part)
    return bytes(result)

record('spawn')
emit('handshake.frame')
while True:
    header = exact(4)
    if header is None:
        break
    payload = exact(int.from_bytes(header, 'big'))
    if payload is None:
        break
    message = json.loads(payload)['message']
    if message['type'] == 'configure_frame_limit':
        continue
    operation = message['body']['op']
    record(operation)
    if operation == 'terminate':
        emit('terminating.frame')
        break
    emit('health.frame' if operation == 'health' else 'metadata.frame')
";

struct Fixture {
    root: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
struct FrameEvent {
    pid: u32,
    op: String,
}

impl Fixture {
    fn new() -> Self {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lean-rs-terminal-frame-{}-{timestamp}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create exclusive fixture directory");
        let fixture = Self { root };
        let script = fixture.root.join("worker");
        fs::write(&script, CHILD).expect("write framed fake worker");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("make fixture executable");
        fixture.message(
            "handshake.frame",
            Message::Handshake {
                worker_version: "terminal-frame-fixture".to_owned(),
                protocol_version: lean_rs_worker_protocol::protocol::PROTOCOL_VERSION,
            },
        );
        for (name, response) in [
            ("health.frame", Response::HealthOk),
            ("terminating.frame", Response::Terminating),
            (
                "metadata.frame",
                Response::CapabilityMetadata {
                    metadata: fixture_metadata(),
                },
            ),
        ] {
            fixture.message(name, Message::Response(response));
        }
        fixture
    }

    fn message(&self, name: &str, message: Message) {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, message, MAX_FRAME_BYTES).expect("encode real protocol frame");
        fs::write(self.root.join(name), bytes).expect("write fixture response frame");
    }

    fn worker(&self) -> LeanWorker {
        let config = LeanWorkerConfig::new(self.root.join("worker"))
            .env("LEAN_RS_FRAME_FIXTURE", self.root.as_os_str())
            .startup_timeout(Duration::from_secs(3))
            .request_timeout(Duration::from_secs(3))
            .shutdown_timeout(Duration::from_millis(500));
        LeanWorker::spawn(&config).expect("spawn framed fake worker")
    }

    fn events(&self) -> Vec<FrameEvent> {
        fs::read_to_string(self.root.join("frames.jsonl"))
            .expect("read complete, reaped-child frame ledger")
            .lines()
            .map(|line| serde_json::from_str(line).expect("parse fixture frame event"))
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.root);
    }
}

fn fixture_metadata() -> LeanWorkerCapabilityMetadata {
    LeanWorkerCapabilityMetadata {
        commands: Vec::new(),
        capabilities: Vec::new(),
        lean_version: Some("fixture-ok".to_owned()),
        extra: None,
    }
}

fn command(worker: &mut LeanWorker) -> Result<LeanWorkerCapabilityMetadata, LeanWorkerError> {
    worker.worker_capability_metadata("fixture", &serde_json::json!({}), None, None)
}

fn pid(worker: &LeanWorker) -> u32 {
    worker.child.as_ref().expect("actual owned fixture child").id()
}

fn reap(mut worker: LeanWorker) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
    let tracker = worker.config.process_tracker.clone();
    let stopped = worker.try_shutdown();
    drop(worker);
    let until = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("cleanup deadline");
    while tracker.active_processes() != 0 {
        assert!(Instant::now() < until, "fixture owned process did not close");
        thread::sleep(Duration::from_millis(2));
    }
    // Ledger absence is checked only after actual owned wait/reap, never after
    // a sleep that merely assumes the child had time to receive a frame.
    stopped
}

// Reader identity is local metadata assigned by the parent, outside the wire frame.
#[derive(Clone, Copy)]
enum EarlierTag {
    Generation(WorkerGeneration),
    Request(WorkerRequestId),
}

thread_local! {
    static NEXT_TERMINAL_TAG: Cell<Option<EarlierTag>> = const { Cell::new(None) };
    static EXPIRE_ON_NEXT_TERMINAL: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn observe_fixture_terminal(
    mut event: RequestReaderEvent,
    deadline: &mut Option<Instant>,
) -> RequestReaderEvent {
    if let RequestReaderEvent::Terminal {
        generation, request_id, ..
    } = &mut event
    {
        match NEXT_TERMINAL_TAG.take() {
            Some(EarlierTag::Generation(old)) => *generation = old,
            Some(EarlierTag::Request(old)) => *request_id = Some(old),
            None => {}
        }
        if EXPIRE_ON_NEXT_TERMINAL.replace(false) {
            // Model the parent resuming at expiry after a successful receive.
            assert!(deadline.is_some(), "the fixture needs a configured deadline");
            *deadline = Some(Instant::now());
        }
    }
    event
}

#[test]
fn configured_deadline_rejects_dequeued_success_and_restarts() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker();
    let original_pid = pid(&worker);
    let original_generation = worker.generation;
    let request_id = worker.next_request_id;
    let checkpoint = worker.request_checkpoint();
    let configured_timeout = worker.config.request_timeout;

    EXPIRE_ON_NEXT_TERMINAL.set(true);
    let result = command(&mut worker);
    let terminal_observed = !EXPIRE_ON_NEXT_TERMINAL.replace(false);
    let receipt = worker.confirmed_request_stop_since(&checkpoint).cloned();
    let successor_pid = pid(&worker);
    let successor_generation = worker.generation;
    let restarts = worker.stats.restarts;
    let next = command(&mut worker);
    let stopped = reap(worker);

    assert!(terminal_observed, "the real terminal must reach the receive boundary");
    assert!(
        matches!(&result, Err(LeanWorkerError::Timeout { operation: "worker_capability_metadata", duration, resource })
            if *duration == configured_timeout && resource.work_entered_child),
        "{result:?}"
    );
    let receipt = receipt.expect("the original request's child was reaped");
    assert_eq!(receipt.operation(), "worker_capability_metadata");
    assert_eq!(receipt.request_id(), request_id.0);
    assert_eq!(receipt.worker_generation(), original_generation.0);
    assert_eq!(receipt.process_id(), original_pid);
    assert_eq!(restarts, 1);
    assert_ne!(successor_generation, original_generation);
    assert_ne!(successor_pid, original_pid);
    assert_eq!(
        next.expect("a new request succeeds on the successor"),
        fixture_metadata()
    );
    assert!(stopped.is_ok(), "{stopped:?}");

    let events = fixture.events();
    let command_pids: Vec<_> = events
        .iter()
        .filter(|event| event.op == "capability_metadata")
        .map(|event| event.pid)
        .collect();
    assert_eq!(command_pids, [original_pid, successor_pid], "{events:?}");
}

#[test]
fn stale_terminal_cannot_complete_or_redispatch_the_current_command() {
    for previous_generation in [false, true] {
        let fixture = Fixture::new();
        let mut worker = fixture.worker();
        worker.health().expect("establish prior completed request");
        let prior = worker.last_request.clone().expect("prior request identity");
        let (tag, expected_error) = if previous_generation {
            worker.cycle().expect("establish successor generation");
            assert_ne!(prior.generation, worker.generation);
            (
                EarlierTag::Generation(prior.generation),
                format!(
                    "from generation {}, current generation is {}",
                    prior.generation.0, worker.generation.0
                ),
            )
        } else {
            (
                EarlierTag::Request(prior.id),
                format!(
                    "from request {}, current request is {}",
                    prior.id.0, worker.next_request_id.0
                ),
            )
        };
        let current_pid = pid(&worker);
        NEXT_TERMINAL_TAG.set(Some(tag));
        let result = command(&mut worker);
        let unused_tag = NEXT_TERMINAL_TAG.take();
        let pending = worker.cleanup_pending();
        let next = command(&mut worker);
        let stopped = reap(worker);
        let events = fixture.events();
        eprintln!("{expected_error}; current pid {current_pid}; ledger {events:?}; cleanup {stopped:?}");
        assert!(
            !events
                .iter()
                .any(|event| event.pid == current_pid && event.op == "terminate"),
            "{events:?}"
        );
        assert!(unused_tag.is_none(), "reader must deliver a terminal frame");
        assert!(
            matches!(result, Err(LeanWorkerError::Protocol { ref message })
            if message == &format!("worker sent stale worker_capability_metadata frame {expected_error}")),
            "{result:?}"
        );
        assert!(pending, "stale reader must fence the live owner");
        assert!(
            matches!(next, Err(LeanWorkerError::ShutdownInProgress { .. })),
            "{next:?}"
        );
        assert!(
            matches!(stopped, Ok(ref report) if report.outcome == LeanWorkerShutdownOutcome::KillOnly),
            "{stopped:?}; {events:?}"
        );
        let calls: Vec<_> = events
            .iter()
            .filter(|event| event.op == "capability_metadata")
            .collect();
        assert_eq!(calls.len(), 1, "all generations: {events:?}");
        assert_eq!(
            calls.first().expect("one command reached the live worker").pid,
            current_pid
        );
        assert_eq!(
            events.iter().filter(|event| event.op == "spawn").count(),
            if previous_generation { 2 } else { 1 }
        );
    }
}
