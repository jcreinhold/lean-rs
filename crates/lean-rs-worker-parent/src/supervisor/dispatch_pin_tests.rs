//! Private frame-gating tests. Injected native IDs are fixture identities only;
//! none of these tests qualifies native admission, custody or stage closure.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::wildcard_imports,
    reason = "private contract tests inspect the owning supervisor and fail with fixture context"
)]

use super::*;
use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::AtomicU64;

const NATIVE_ID: &str = "fixture-admitted";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

const CHILD: &str = r"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
import time

root = Path(os.environ['LEAN_RS_PIN_FIXTURE'])
sequence = 0

def record(operation):
    global sequence
    with (root / 'frames.jsonl').open('a') as log:
        log.write(json.dumps({'pid': os.getpid(), 'op': operation,
                              'sequence': sequence}) + '\n')
        log.flush()
    sequence += 1

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
        record('configure_frame_limit')
        continue
    operation = message['body']['op']
    record(operation)
    if operation == 'terminate':
        emit('terminating.frame')
        break
    if operation == 'release_host_read':
        emit('released.frame')
        continue
    mode = (root / 'mode').read_text().strip()
    if mode == 'exit_17':
        raise SystemExit(17)
    if mode == 'hang':
        time.sleep(60)
        break
    if mode == 'session_missing':
        emit('missing.frame')
    elif mode == 'header_recycle':
        emit('header.frame')
    elif mode == 'epoch_changed':
        emit('epoch.frame')
    elif mode == 'wrong_response':
        emit('json.frame' if operation == 'health' else 'health.frame')
    elif operation == 'open_host_session':
        emit('session.frame')
    elif operation == 'health':
        emit('health.frame')
    else:
        emit('json.frame')
";

struct Fixture {
    root: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
struct FrameEvent {
    pid: u32,
    op: String,
    sequence: u64,
}

impl Fixture {
    fn new() -> Self {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lean-rs-dispatch-pin-{}-{timestamp}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create exclusive fixture directory");
        let fixture = Self { root };
        let script = fixture.root.join("worker");
        fs::write(&script, CHILD).expect("write framed fake worker");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("make fixture executable");
        fixture.mode("normal");
        fixture.message(
            "handshake.frame",
            Message::Handshake {
                worker_version: "dispatch-pin-fixture".to_owned(),
                protocol_version: lean_rs_worker_protocol::protocol::PROTOCOL_VERSION,
            },
        );
        for (name, response) in [
            ("health.frame", Response::HealthOk),
            ("terminating.frame", Response::Terminating),
            ("released.frame", Response::HostReadReleased),
            ("header.frame", Response::HeaderImportRecycle),
            ("epoch.frame", Response::HostArtifactEpochChanged),
            (
                "json.frame",
                Response::JsonCommand {
                    response_json: "fixture-ok".to_owned(),
                },
            ),
            (
                "missing.frame",
                Response::Error {
                    code: "lean_rs.worker.session_missing".to_owned(),
                    message: "fixture session absent".to_owned(),
                },
            ),
            (
                "session.frame",
                Response::HostSessionOpened {
                    import_stats: fixture_import_stats(),
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

    fn mode(&self, mode: &str) {
        fs::write(self.root.join("mode"), mode).expect("select fixture response");
    }

    fn worker(&self) -> LeanWorker {
        let config = LeanWorkerConfig::new(self.root.join("worker"))
            .env("LEAN_RS_PIN_FIXTURE", self.root.as_os_str())
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

    fn assert_operations(&self, pid: u32, operations: &[&str]) {
        let events = self.events();
        assert!(events.iter().all(|event| event.pid == pid), "{events:?}");
        assert_eq!(
            events.iter().filter(|event| event.op == "spawn").count(),
            1,
            "{events:?}"
        );
        for pair in events.windows(2) {
            let [previous, next] = pair else {
                panic!("windows(2) always produces a pair")
            };
            assert_eq!(previous.sequence.checked_add(1), Some(next.sequence), "{events:?}");
        }
        let actual: Vec<_> = events
            .iter()
            .filter(|event| !matches!(event.op.as_str(), "spawn" | "configure_frame_limit"))
            .map(|event| event.op.as_str())
            .collect();
        assert_eq!(actual, operations, "complete native frame ledger");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.root);
    }
}

fn fixture_import_stats() -> LeanWorkerImportStats {
    LeanWorkerImportStats {
        direct_import_names: Vec::new(),
        effective_module_count: 0,
        compacted_region_count: 0,
        memory_mapped_region_count: 0,
        compacted_region_bytes: 0,
        memory_mapped_region_bytes: 0,
        non_memory_mapped_region_bytes: 0,
        imported_bytes: 0,
        imported_constant_count: 0,
        extension_count: 0,
        total_imported_extension_entries: 0,
        import_level: "fixture".to_owned(),
        import_all: false,
        load_exts: false,
    }
}

fn pin(worker: &mut LeanWorker) -> GenerationDispatchBinding {
    // This injection tests frame restriction only, not actual guarded admission.
    worker.native_generation_id = Some(NATIVE_ID.to_owned());
    worker.arm_generation_dispatch(NATIVE_ID).expect("arm fixture binding")
}

fn command(worker: &mut LeanWorker) -> Result<String, LeanWorkerError> {
    worker.worker_json_command("fixture", "{}".to_owned(), None, None)
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

fn refused<T: fmt::Debug>(result: &Result<T, LeanWorkerError>) {
    assert!(
        matches!(result, Err(LeanWorkerError::ShutdownInProgress { .. })),
        "{result:?}"
    );
}

#[test]
fn arm_requires_idle_native_identity_and_cannot_rebind() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker();
    let child_pid = pid(&worker);
    assert_eq!(
        worker
            .arm_generation_dispatch(NATIVE_ID)
            .expect_err("missing admitted identity"),
        GenerationDispatchRejection::NativeGenerationUnavailable
    );
    worker.native_generation_id = Some(NATIVE_ID.to_owned());
    assert_eq!(
        worker
            .arm_generation_dispatch("other-generation")
            .expect_err("wrong native identity"),
        GenerationDispatchRejection::NativeGenerationMismatch
    );
    worker
        .begin_in_flight("fixture_busy")
        .expect("set private busy state without a frame");
    assert_eq!(
        worker
            .arm_generation_dispatch(NATIVE_ID)
            .expect_err("busy owner cannot arm"),
        GenerationDispatchRejection::NativeGenerationUnavailable
    );
    worker.finish_in_flight();
    let binding = worker.arm_generation_dispatch(NATIVE_ID).expect("idle exact identity");
    assert_eq!(
        worker
            .arm_generation_dispatch(NATIVE_ID)
            .expect_err("a stage cannot rearm"),
        GenerationDispatchRejection::AlreadyBound
    );
    worker
        .seal_generation_dispatch(&binding)
        .expect("seal accepted binding");
    assert_eq!(
        worker
            .arm_generation_dispatch(NATIVE_ID)
            .expect_err("seal is not discharge"),
        GenerationDispatchRejection::AlreadyBound
    );
    let stopped = reap(worker);
    fixture.assert_operations(child_pid, &["terminate"]);
    assert!(stopped.is_ok(), "{stopped:?}");
}

#[test]
fn original_generation_allows_deliberate_frames_without_reply_discharge() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker();
    let child_pid = pid(&worker);
    let _binding = pin(&mut worker);
    let first = command(&mut worker);
    let second = command(&mut worker);
    let still_bound = worker.generation_dispatch.is_some();
    let sealed = worker
        .generation_dispatch
        .as_ref()
        .expect("restriction retained")
        .sealed;
    let stopped = reap(worker);
    fixture.assert_operations(child_pid, &["json_command", "json_command", "terminate"]);
    assert_eq!(first.expect("first deliberate frame"), "fixture-ok");
    assert_eq!(second.expect("second deliberate frame"), "fixture-ok");
    assert!(still_bound);
    assert!(
        !sealed,
        "a reply is neither closure nor a reason to forbid an intended grouped frame"
    );
    assert!(stopped.is_ok(), "{stopped:?}");
}

#[test]
#[allow(
    clippy::mem_forget,
    reason = "adversarially forgetting the correlation token must not remove owner state"
)]
fn explicit_seal_foreign_binding_and_forget_reach_the_final_writer_gate() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker();
    let child_pid = pid(&worker);
    let binding = pin(&mut worker);
    let foreign = GenerationDispatchBinding { owner: Arc::new(()) };
    assert_eq!(
        worker
            .seal_generation_dispatch(&foreign)
            .expect_err("foreign seal cannot mutate this owner"),
        GenerationDispatchRejection::ForeignBinding
    );
    let allowed = command(&mut worker);
    worker
        .seal_generation_dispatch(&binding)
        .expect("seal original binding");
    worker.seal_generation_dispatch(&binding).expect("seal is idempotent");
    std::mem::forget(binding);
    // Intentionally bypass send_request_cancellable's precheck. A mutation of
    // the final writer gate must emit an extra actual frame caught below.
    let raw = worker.write_request_bounded(
        Request::JsonCommand {
            export: "forbidden".to_owned(),
            request_json: "{}".to_owned(),
        },
        "direct_final_writer_probe",
        None,
        Duration::from_secs(1),
    );
    let stopped = reap(worker);
    fixture.assert_operations(child_pid, &["json_command", "terminate"]);
    assert!(allowed.is_ok(), "foreign binding must not seal this owner");
    refused(&raw);
    assert!(stopped.is_ok(), "{stopped:?}");
}

#[test]
fn changed_nonce_epoch_or_owner_seals_even_after_identity_is_restored() {
    for changed in ["nonce", "epoch", "owner"] {
        let fixture = Fixture::new();
        let mut worker = fixture.worker();
        let child_pid = pid(&worker);
        let _binding = pin(&mut worker);
        let first = command(&mut worker);
        let original_owner = Arc::clone(&worker.request_scope);
        let original_generation = worker.generation;
        match changed {
            "nonce" => worker.native_generation_id = Some("replacement-native".to_owned()),
            "epoch" => worker.generation = WorkerGeneration(1),
            "owner" => worker.request_scope = Arc::new(()),
            _ => panic!("fixed mismatch cases"),
        }
        let request_id_before = worker.next_request_id;
        let mismatch = command(&mut worker);
        assert_eq!(
            worker.next_request_id, request_id_before,
            "clean refusal cannot allocate a request ID"
        );
        // Cleanup uses actual Child authority even while the pin mismatches.
        worker.host_read_active = true;
        let released = worker.release_host_read();
        worker.native_generation_id = Some(NATIVE_ID.to_owned());
        worker.generation = original_generation;
        worker.request_scope = original_owner;
        let restored = command(&mut worker);
        let stopped = reap(worker);
        fixture.assert_operations(child_pid, &["json_command", "release_host_read", "terminate"]);
        assert!(first.is_ok(), "{first:?}");
        refused(&mismatch);
        refused(&restored);
        assert!(released.is_ok(), "{released:?}");
        assert!(stopped.is_ok(), "{stopped:?}");
    }
}

#[test]
fn precancelled_binding_cannot_send_with_a_fresh_token() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker();
    let child_pid = pid(&worker);
    let _binding = pin(&mut worker);
    let cancelled = LeanWorkerCancellationToken::new();
    cancelled.cancel();
    let first = worker.worker_json_command("fixture", "{}".to_owned(), Some(&cancelled), None);
    let fresh = LeanWorkerCancellationToken::new();
    let retry = worker.worker_json_command("fixture", "{}".to_owned(), Some(&fresh), None);
    let stopped = reap(worker);
    fixture.assert_operations(child_pid, &["terminate"]);
    assert!(matches!(first, Err(LeanWorkerError::Cancelled { .. })), "{first:?}");
    refused(&retry);
    assert!(stopped.is_ok(), "{stopped:?}");
}

#[test]
fn session_missing_error_blocks_owner_reopen_and_same_generation_retry() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker();
    let child_pid = pid(&worker);
    let _binding = pin(&mut worker);
    fixture.mode("session_missing");
    let first = command(&mut worker);
    let original_generation = worker.generation;
    fixture.mode("normal");
    // The sibling test module cannot construct capability-private fields. This
    // exercises the owner sequence that the unchanged convenience retry reaches.
    let config = crate::session::LeanWorkerSessionConfig::shims_only(&fixture.root, ["Init"]);
    let reopen = worker.open_worker_session(&config, None, None);
    let retry = command(&mut worker);
    assert_eq!(worker.generation, original_generation);
    assert_eq!(worker.native_generation_id.as_deref(), Some(NATIVE_ID));
    let stopped = reap(worker);
    fixture.assert_operations(child_pid, &["json_command", "terminate"]);
    assert!(
        matches!(&first, Err(LeanWorkerError::Worker { code, .. }) if code == "lean_rs.worker.session_missing"),
        "{first:?}"
    );
    refused(&reopen);
    refused(&retry);
    assert!(stopped.is_ok(), "{stopped:?}");
}

#[test]
fn framed_recycle_refusals_never_reopen_replay_or_spawn() {
    for (mode, session) in [
        ("header_recycle", false),
        ("header_recycle", true),
        ("epoch_changed", true),
    ] {
        let fixture = Fixture::new();
        let mut worker = fixture.worker();
        let child_pid = pid(&worker);
        let _binding = pin(&mut worker);
        fixture.mode(mode);
        let config = crate::session::LeanWorkerSessionConfig::shims_only(&fixture.root, ["Init"]);
        let first = if session {
            worker.open_worker_session(&config, None, None)
        } else {
            command(&mut worker).map(|_| ())
        };
        fixture.mode("normal");
        let retry = command(&mut worker);
        let restarts = worker.stats.restarts;
        let stopped = reap(worker);
        let operation = if session { "open_host_session" } else { "json_command" };
        fixture.assert_operations(child_pid, &[operation, "terminate"]);
        refused(&first);
        refused(&retry);
        assert_eq!(restarts, 0);
        assert!(stopped.is_ok(), "{stopped:?}");
    }
}

#[test]
fn well_framed_unexpected_projection_and_health_replies_seal_before_retry() {
    for health in [false, true] {
        let fixture = Fixture::new();
        let mut worker = fixture.worker();
        let child_pid = pid(&worker);
        let _binding = pin(&mut worker);
        fixture.mode("wrong_response");
        let first = if health {
            worker.health()
        } else {
            command(&mut worker).map(|_| ())
        };
        fixture.mode("normal");
        let retry = if health {
            worker.health()
        } else {
            command(&mut worker).map(|_| ())
        };
        let stopped = reap(worker);
        let operation = if health { "health" } else { "json_command" };
        fixture.assert_operations(child_pid, &[operation, "terminate"]);
        assert!(matches!(first, Err(LeanWorkerError::Protocol { .. })), "{first:?}");
        refused(&retry);
        assert!(stopped.is_ok(), "{stopped:?}");
    }
}

#[test]
fn central_policy_restart_runs_cleanup_callback_without_spawning() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker();
    let child_pid = pid(&worker);
    let _binding = pin(&mut worker);
    let callbacks = Cell::new(0_u32);
    let spawns = Cell::new(0_u32);
    let result = worker.restart_with_reason_before_spawn_using(
        LeanWorkerRestartReason::Explicit,
        || callbacks.set(callbacks.get().saturating_add(1)),
        |config| {
            spawns.set(spawns.get().saturating_add(1));
            LeanWorker::spawn_tracked(config)
        },
    );
    let stopped = reap(worker);
    // Removing the central bound branch produces a real second fixture PID,
    // caught by the complete ledger before checking the returned error.
    fixture.assert_operations(child_pid, &["terminate"]);
    assert_eq!(spawns.get(), 0);
    assert_eq!(callbacks.get(), 1);
    refused(&result);
    assert!(stopped.is_ok(), "{stopped:?}");
}

#[test]
fn typed_verification_child_exit_preserves_error_instead_of_degraded_success() {
    use lean_rs_worker_protocol::types::{
        LeanWorkerDeclarationVerificationBatchItem, LeanWorkerDeclarationVerificationTarget, LeanWorkerOutputBudgets,
        LeanWorkerSorryPolicy,
    };

    for batch in [false, true] {
        let fixture = Fixture::new();
        let mut worker = fixture.worker();
        let child_pid = pid(&worker);
        let _binding = pin(&mut worker);
        fixture.mode("exit_17");
        let target = LeanWorkerDeclarationVerificationTarget::Name {
            name: "fixture".to_owned(),
        };
        // Only the frame shape is real. This fake child never invokes Lean or
        // performs verification, and intentionally exits after recording it.
        let result = if batch {
            worker
                .worker_verify_declaration_batch(
                    &LeanWorkerDeclarationVerificationBatchRequest {
                        source: String::new(),
                        targets: vec![LeanWorkerDeclarationVerificationBatchItem {
                            id: "fixture-target".to_owned(),
                            target,
                        }],
                        sorry_policy: LeanWorkerSorryPolicy::Deny,
                        report_axioms: false,
                        budgets: LeanWorkerOutputBudgets::default(),
                    },
                    &LeanWorkerElabOptions::default(),
                    None,
                    None,
                )
                .map(|_| ())
        } else {
            worker
                .worker_verify_declaration(
                    &LeanWorkerDeclarationVerificationRequest {
                        source: String::new(),
                        target,
                        sorry_policy: LeanWorkerSorryPolicy::Deny,
                        report_axioms: false,
                        budgets: LeanWorkerOutputBudgets::default(),
                    },
                    &LeanWorkerElabOptions::default(),
                    None,
                    None,
                )
                .map(|_| ())
        };
        let retry = command(&mut worker);
        let restarts = worker.stats.restarts;
        let stopped = reap(worker);
        let operation = if batch {
            "verify_declaration_batch"
        } else {
            "verify_declaration"
        };
        fixture.assert_operations(child_pid, &[operation]);
        assert!(
            matches!(&result, Err(LeanWorkerError::ChildPanicOrAbort { exit }) if exit.code == Some(17)),
            "bound verification must preserve the original child exit: {result:?}"
        );
        refused(&retry);
        assert_eq!(restarts, 0);
        assert!(stopped.is_ok(), "{stopped:?}");
    }
}

#[test]
fn accounting_request_import_residue_and_idle_policies_refuse_before_session_or_body() {
    for policy in ["accounting", "requests", "imports", "residue", "idle"] {
        for session in [false, true] {
            let fixture = Fixture::new();
            let mut worker = fixture.worker();
            let child_pid = pid(&worker);
            let _binding = pin(&mut worker);
            match policy {
                "accounting" => worker.budget.accounting_unknown = true,
                "requests" => {
                    worker.config.restart_policy = LeanWorkerRestartPolicy::disabled().max_requests(1);
                    worker.budget.requests = 1;
                }
                "imports" => {
                    worker.config.restart_policy = LeanWorkerRestartPolicy::disabled().max_imports(1);
                    worker.budget.imports = 1;
                }
                "residue" => {
                    worker.config.restart_policy = LeanWorkerRestartPolicy::disabled().max_import_residue_bytes(1);
                    worker.budget.import_residue_bytes = 1;
                }
                "idle" => {
                    worker.config.restart_policy =
                        LeanWorkerRestartPolicy::disabled().idle_restart_after(Duration::from_secs(1));
                    worker.last_activity = Instant::now()
                        .checked_sub(Duration::from_secs(2))
                        .expect("set idle fixture without sleeping");
                }
                _ => panic!("fixed policy cases"),
            }
            let result = if session {
                let config = crate::session::LeanWorkerSessionConfig::shims_only(&fixture.root, ["Init"]);
                worker.open_worker_session(&config, None, None)
            } else {
                // Import-count/residue checks apply only to import-like work.
                // Exercise that actual round-trip admission path without
                // adding any host policy or invoking an import implementation.
                worker
                    .round_trip(
                        "bound_policy_body_probe",
                        Request::JsonCommand {
                            export: "fixture".to_owned(),
                            request_json: "{}".to_owned(),
                        },
                        true,
                        None,
                        None,
                        |response, operation| {
                            if let Response::JsonCommand { response_json } = response {
                                Ok(response_json)
                            } else {
                                Err(unexpected_response(operation, &response))
                            }
                        },
                    )
                    .map(|_| ())
            };
            worker.config.restart_policy = LeanWorkerRestartPolicy::disabled();
            worker.budget = GenerationBudget::default();
            worker.last_activity = Instant::now();
            let retry = command(&mut worker);
            let stopped = reap(worker);
            fixture.assert_operations(child_pid, &["terminate"]);
            refused(&result);
            refused(&retry);
            assert!(stopped.is_ok(), "{stopped:?}");
        }
    }
}

#[test]
fn active_timeout_and_cancel_stop_original_without_replacement() {
    for cancel in [false, true] {
        let fixture = Fixture::new();
        let mut worker = fixture.worker();
        let child_pid = pid(&worker);
        let _binding = pin(&mut worker);
        fixture.mode("hang");
        worker.config.request_timeout = if cancel {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(100)
        };
        let token = LeanWorkerCancellationToken::new();
        let cancellation = if cancel {
            let log = fixture.root.join("frames.jsonl");
            let token = token.clone();
            Some(thread::spawn(move || {
                let until = Instant::now()
                    .checked_add(Duration::from_secs(3))
                    .expect("fixture observation deadline");
                loop {
                    let observed = fs::read_to_string(&log).is_ok_and(|text| text.contains("json_command"));
                    if observed || Instant::now() >= until {
                        token.cancel();
                        return observed;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            }))
        } else {
            None
        };
        let result = worker.worker_json_command("fixture", "{}".to_owned(), Some(&token), None);
        let observed = cancellation.map(|thread| thread.join().expect("cancellation observer finished"));
        let child_gone = worker.child.is_none();
        let restarts = worker.stats.restarts;
        let stopped = reap(worker);
        fixture.assert_operations(child_pid, &["json_command"]);
        assert!(child_gone);
        assert_eq!(restarts, 0);
        if cancel {
            assert_eq!(observed, Some(true), "cancel after actual frame observation");
            assert!(matches!(result, Err(LeanWorkerError::Cancelled { .. })), "{result:?}");
        } else {
            assert!(matches!(result, Err(LeanWorkerError::Timeout { .. })), "{result:?}");
        }
        assert!(stopped.is_ok(), "{stopped:?}");
    }
}

#[test]
fn partial_write_timeout_preserves_original_failure_and_stop_receipt() {
    let mut worker = super::tests::blocked_stdin_worker();
    let child_pid = pid(&worker);
    let _binding = pin(&mut worker);
    worker.config.request_timeout = Duration::from_millis(100);
    let checkpoint = worker.request_checkpoint();
    let result = worker.send_request_cancellable(
        Request::JsonCommand {
            export: "unused".to_owned(),
            request_json: "x".repeat(256 * 1024),
        },
        "bound_partial_write",
        None,
    );
    let receipt = worker.confirmed_request_stop_since(&checkpoint).cloned();
    let child_gone = worker.child.is_none();
    let restarts = worker.stats.restarts;
    let stopped = reap(worker);
    assert!(matches!(result, Err(LeanWorkerError::Timeout { .. })), "{result:?}");
    let receipt = receipt.expect("actual original child wait receipt");
    assert_eq!(receipt.process_id(), child_pid);
    assert_eq!(receipt.operation(), "bound_partial_write");
    assert!(child_gone);
    assert_eq!(restarts, 0);
    assert!(stopped.is_ok(), "{stopped:?}");
}

#[test]
fn cleanup_failure_remains_visible_and_keeps_binding_sealed() {
    let mut worker = super::tests::sleeping_worker();
    let _binding = pin(&mut worker);
    let actual = worker.child.take().expect("move exact child into pending startup");
    worker.pending_startup = Some(Box::new(StartupChild(Some(actual))));
    let result = worker.shutdown_child_with_cleanup(ShutdownIntent::KillOnly, |_| {
        Err(LeanWorkerError::Wait {
            source: std::io::Error::other("injected bound cleanup failure"),
        })
    });
    let retained = worker.pending_startup.is_some();
    let retry = command(&mut worker);
    let stopped = reap(worker);
    assert!(matches!(result, Err(LeanWorkerError::Wait { .. })), "{result:?}");
    assert!(retained, "failed cleanup retains actual owned child");
    refused(&retry);
    assert!(stopped.is_ok(), "{stopped:?}");
}

// Reader identity is local metadata assigned by the parent, outside the wire frame.
#[derive(Clone, Copy)]
enum EarlierTag {
    Generation(WorkerGeneration),
    Request(WorkerRequestId),
}

thread_local! {
    static NEXT_TERMINAL_TAG: Cell<Option<EarlierTag>> = const { Cell::new(None) };
}

pub(super) fn tag_fixture_terminal(mut event: RequestReaderEvent) -> RequestReaderEvent {
    if let RequestReaderEvent::Terminal {
        generation, request_id, ..
    } = &mut event
    {
        match NEXT_TERMINAL_TAG.take() {
            Some(EarlierTag::Generation(old)) => *generation = old,
            Some(EarlierTag::Request(old)) => *request_id = Some(old),
            None => {}
        }
    }
    event
}

#[test]
fn stale_terminal_cannot_complete_or_redispatch_the_current_command() {
    for previous_generation in [false, true] {
        let fixture = Fixture::new();
        let mut worker = fixture.worker();
        worker.health().expect("establish prior completed request");
        let prior = worker.last_request.clone().expect("prior request identity");
        let (tag, expected_error) = if previous_generation {
            worker.restart().expect("establish successor generation");
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
            if message == &format!("worker sent stale worker_json_command frame {expected_error}")),
            "{result:?}"
        );
        assert!(pending, "stale reader must fence the live owner");
        refused(&next);
        assert!(
            matches!(stopped, Ok(ref report) if report.outcome == LeanWorkerShutdownOutcome::KillOnly),
            "{stopped:?}; {events:?}"
        );
        let calls: Vec<_> = events.iter().filter(|event| event.op == "json_command").collect();
        assert_eq!(calls.len(), 1, "all generations: {events:?}");
        assert_eq!(calls[0].pid, current_pid);
        assert_eq!(
            events.iter().filter(|event| event.op == "spawn").count(),
            if previous_generation { 2 } else { 1 }
        );
    }
}
