#![allow(
    clippy::expect_used,
    reason = "fixture failures identify the failed setup, I/O, or assertion"
)]

use super::{ManagedInspectionOutcome, ManagedRequestDeadline};
use crate::crash_ownership::{NONCE_ENV, ParentLifetime, SOCKET_ENV};
use crate::supervisor::{
    BufReader, BufWriter, Command, Duration, Instant, LeanWorker, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerError, LeanWorkerProcessTracker, LeanWorkerSessionConfig,
    MAX_FRAME_BYTES, Message, Path, PathBuf, Pipe, Request, Response, ShutdownIntent, Stdio, WorkerSupervisorState,
    Write, io, read_frame, write_frame,
};
use std::io::BufRead as _;
use std::os::unix::fs::PermissionsExt as _;

const ROOT: &str = "LEAN_MANAGED_ONCE_FIXTURE";
const THIS: &str = "supervisor::managed_once::tests::";

#[test]
fn guardian_fixture() {
    let Some(root) = std::env::var_os(ROOT).map(PathBuf::from) else {
        return;
    };
    lean_toolchain::native_guardian::run_guardian_cli().expect("run managed fixture guardian");
    if root.join("hold-guardian").exists() {
        let until = Instant::now() + Duration::from_secs(8);
        while Instant::now() < until && !root.join("release-guardian").exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn worker_fixture() {
    let Some(root) = std::env::var_os(ROOT).map(PathBuf::from) else {
        return;
    };
    rustix::process::setsid().expect("start managed worker session");
    let _guardian = lean_toolchain::native_guardian::GuardianHandle::spawn(
        &root.join("guardian"),
        Path::new(&std::env::var_os(SOCKET_ENV).expect("fixture guardian socket")),
        &std::env::var(NONCE_ENV).expect("fixture guardian nonce"),
        Duration::from_secs(3),
    )
    .expect("start fixture guardian");
    println!("managed-worker-ready");
    std::io::stdout().flush().expect("flush worker readiness");
    let mut output = std::io::stdout().lock();
    let mut ledger = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("dispatch"))
        .expect("open dispatch ledger");
    loop {
        let message = read_frame(&mut std::io::stdin().lock(), MAX_FRAME_BYTES)
            .expect("read managed inspection frame")
            .message;
        writeln!(&mut ledger, "request").expect("record managed request dispatch");
        assert!(matches!(message, Message::Request(Request::InspectDeclaration { .. })));
        if root.join("hold-inspection-reply").exists() {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until && !root.join("release-inspection-reply").exists() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        write_frame(
            &mut output,
            Message::Response(Response::DeclarationInspection {
                result: LeanWorkerDeclarationInspectionResult::Unsupported,
            }),
            MAX_FRAME_BYTES,
        )
        .expect("write managed inspection response");
    }
}

fn write_guardian_script(root: &Path, executable: &Path) {
    let binary = executable.to_string_lossy().replace('\'', "'\\''");
    let fixture = root.to_string_lossy().replace('\'', "'\\''");
    std::fs::write(
        root.join("guardian"),
        format!("#!/bin/sh\nexport {ROOT}='{fixture}'\nexec '{binary}' --exact {THIS}guardian_fixture --nocapture\n"),
    )
    .expect("write fixture guardian script");
    std::fs::set_permissions(root.join("guardian"), std::fs::Permissions::from_mode(0o700))
        .expect("make fixture guardian executable");
}

fn worker(root: &Path) -> (LeanWorker, LeanWorkerProcessTracker, u32) {
    let executable = std::env::current_exe().expect("fixture test executable");
    write_guardian_script(root, &executable);
    let mut lifetime = ParentLifetime::prepare_guarded(&root.join("owners"), Duration::from_secs(3))
        .expect("prepare managed fixture lifetime");
    let mut command = Command::new(executable);
    command
        .args(["--exact", &format!("{THIS}worker_fixture"), "--nocapture"])
        .env(ROOT, root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    lifetime.configure(&mut command);
    let tracker = LeanWorkerProcessTracker::new();
    let mut child = tracker
        .spawn_guarded(&mut command, &mut lifetime)
        .expect("spawn guarded fixture worker");
    let pid = child.id();
    let lease = lifetime
        .accept(pid, Duration::from_secs(3))
        .expect("accept fixture guardian lease");
    let generation = lease
        .guarded_generation_id()
        .expect("guarded fixture generation")
        .to_owned();
    child.attach_lifetime(lease).expect("attach fixture lifetime");
    child.acknowledge_lifetime().expect("acknowledge fixture lifetime");
    let mut output = Pipe::new(child.stdout.take().expect("fixture worker stdout")).expect("open worker output pipe");
    output.deadline = Instant::now().checked_add(Duration::from_secs(3));
    let mut output = BufReader::new(output);
    loop {
        let mut line = String::new();
        assert_ne!(output.read_line(&mut line).expect("read fixture worker readiness"), 0);
        if line.ends_with("managed-worker-ready\n") {
            break;
        }
    }
    output.get_mut().deadline = None;
    let mut worker = super::super::tests::sleeping_worker();
    worker
        .shutdown_child(ShutdownIntent::KillOnly)
        .expect("retire placeholder worker");
    worker.state = WorkerSupervisorState::Idle {
        generation: worker.generation,
    };
    worker.child = Some(child);
    worker.last_exit = None;
    worker.stdout = Some(output);
    worker.stdin = Some(BufWriter::new(
        Pipe::new(
            worker
                .child
                .as_mut()
                .expect("installed fixture worker")
                .stdin
                .take()
                .expect("fixture worker stdin"),
        )
        .expect("open worker input pipe"),
    ));
    worker.native_generation_id = Some(generation);
    worker
        .open_session_keys
        .push(LeanWorkerSessionConfig::new(root, "fixture", "Fixture", ["Init"]));
    (worker, tracker, pid)
}

fn scope(worker: &LeanWorker, registry: &Path) -> lean_toolchain::native_guardian::NativeGuardianDrainBinding {
    use std::os::unix::fs::MetadataExt as _;
    #[derive(serde::Deserialize)]
    struct Record {
        generation_id: String,
        original_worker_boot: String,
        parent: Identity,
        process: Identity,
        guardian: Identity,
    }
    let generation = worker.current_native_generation_id().expect("exact owned generation");
    let path = registry.join(format!("worker-{generation}/owner.json"));
    let record: Record = serde_json::from_slice(&std::fs::read(path).expect("read exact owner record"))
        .expect("decode guarded owner identities");
    let metadata = registry.metadata().expect("read private registry identity");
    lean_toolchain::native_guardian::NativeGuardianDrainBinding {
        registry_device: metadata.dev(),
        registry_inode: metadata.ino(),
        generation: record.generation_id,
        boot: record.original_worker_boot,
        parent: record.parent,
        worker: record.process,
        guardian: record.guardian,
    }
}

fn inspect(
    worker: &mut LeanWorker,
    registry: &Path,
    name: &str,
    arm: &ManagedRequestDeadline,
) -> Result<ManagedInspectionOutcome, LeanWorkerError> {
    let expected = scope(worker, registry);
    worker.inspect_admitted_declaration_once(
        LeanWorkerDeclarationInspectionRequest::new(name),
        &expected,
        arm.attempt,
        arm.stage,
        arm.expires_ns,
    )
}

fn arm() -> ManagedRequestDeadline {
    ManagedRequestDeadline {
        attempt: [7; 16],
        stage: 1,
        expires_ns: u64::try_from(
            lean_toolchain::native_guardian::guardian_deadline_ticks()
                .expect("read guardian deadline clock")
                .saturating_add(Duration::from_secs(3))
                .as_nanos(),
        )
        .expect("fixture deadline fits in u64 nanoseconds"),
    }
}

#[test]
fn real_guardian_gates_managed_inspection_and_retires_once() {
    let fixture = std::env::temp_dir().join(format!("lean-managed-once-{}", std::process::id()));
    std::fs::create_dir(&fixture).expect("create managed fixture directory");
    let root = fixture.canonicalize().expect("resolve managed fixture directory");
    for case in ["success", "duplicate", "retirement_pending"] {
        let duplicate_arm = case == "duplicate";
        let retirement_pending = case == "retirement_pending";
        let root = root.join(case);
        std::fs::create_dir(&root).expect("create managed fixture case directory");
        if retirement_pending {
            std::fs::write(root.join("hold-guardian"), b"").expect("hold fixture guardian after draining");
        }
        let (mut worker, tracker, pid) = worker(&root);
        if duplicate_arm {
            worker
                .child
                .as_mut()
                .expect("fixture worker for duplicate arm")
                .arm_managed_request(&arm())
                .expect("arm the first managed request");
        }
        let registry = root.join("owners");
        let expected = scope(&worker, &registry);
        let mut stale = expected.clone();
        stale.boot = "00000000-0000-0000-0000-000000000000".to_owned();
        let original = arm();
        let next = worker.next_request_id;
        assert!(
            worker
                .inspect_admitted_declaration_once(
                    LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero"),
                    &stale,
                    original.attempt,
                    original.stage,
                    original.expires_ns,
                )
                .is_err()
        );
        assert_eq!(worker.next_request_id, next, "stale scope must send no bytes");
        let ManagedInspectionOutcome { result, retirement } = if case == "success" {
            let request = LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero");
            worker
                .validate_managed_receiver(&expected)
                .expect("first grant has its exact live receiver");
            worker.inspect_declaration_managed_once_with(request.clone(), &original, None, None, |receiver| {
                receiver
                    .validate_managed_receiver(&expected)
                    .expect("original worker still live before replay");
                assert_eq!(
                    receiver.current_native_generation_id(),
                    Some(expected.generation.as_str())
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("dispatch")).expect("first frame arrived"),
                    "request\n"
                );
                let next = receiver.next_request_id;
                let duplicate = receiver.inspect_admitted_declaration_once(
                    request,
                    &expected,
                    original.attempt,
                    original.stage,
                    original.expires_ns,
                );
                assert!(matches!(
                    duplicate,
                    Err(LeanWorkerError::ShutdownInProgress {
                        operation: "worker_inspect_declaration_managed_once"
                    })
                ));
                assert_eq!(receiver.next_request_id, next, "refused replay must send no frame");
                receiver
                    .validate_managed_receiver(&expected)
                    .expect("refusal still owns the same live worker");
            })
        } else {
            inspect(&mut worker, &registry, "Nat.add_zero", &original)
        }
        .expect("attempt managed declaration inspection");
        if retirement_pending {
            assert!(matches!(retirement, Err(LeanWorkerError::WaitTimeout { .. })));
            assert_eq!(tracker.active_processes(), 1);
            assert!(worker.cleanup_pending());
            std::fs::write(root.join("release-guardian"), b"").expect("release held fixture guardian");
            worker
                .shutdown_child(ShutdownIntent::KillOnly)
                .expect("finish deferred worker retirement");
        } else {
            retirement.expect("retire managed worker");
        }
        if duplicate_arm {
            assert!(matches!(result, Err(LeanWorkerError::Setup { .. })));
        } else {
            assert_eq!(
                result.expect("receive managed declaration result"),
                LeanWorkerDeclarationInspectionResult::Unsupported
            );
        }
        assert!(
            worker
                .inspect_admitted_declaration_once(
                    LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero"),
                    &expected,
                    original.attempt,
                    original.stage,
                    original.expires_ns,
                )
                .is_err()
        );
        assert_eq!(tracker.active_processes(), 0);
        assert!(!lean_toolchain::process_group::group_present(pid).expect("check retired worker group absence"));
        let dispatch = std::fs::read_to_string(root.join("dispatch"));
        if duplicate_arm {
            assert!(dispatch.map_or_else(|error| error.kind() == io::ErrorKind::NotFound, |data| data.is_empty()));
        } else {
            assert_eq!(dispatch.expect("read successful dispatch ledger"), "request\n");
        }
        if case == "success" {
            let (mut replacement, replacement_tracker, replacement_pid) = self::worker(&root);
            let replacement_binding = scope(&replacement, &registry);
            assert_eq!(replacement_binding.registry_inode, expected.registry_inode);
            assert_ne!(replacement_binding.generation, expected.generation);
            assert_ne!(replacement_binding.worker, expected.worker);
            assert_eq!(replacement_tracker.active_processes(), 1);
            original
                .remaining()
                .expect("original grant still unexpired at replacement");
            let next = replacement.next_request_id;
            assert!(matches!(
                replacement.inspect_admitted_declaration_once(
                    LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero"),
                    &expected,
                    original.attempt,
                    original.stage,
                    original.expires_ns,
                ),
                Err(LeanWorkerError::Setup { message }) if message.contains("managed grant names another receiver")
            ));
            assert_eq!(replacement.next_request_id, next, "replacement refusal sends no frame");
            assert_eq!(scope(&replacement, &registry), replacement_binding);
            replacement
                .shutdown_child(ShutdownIntent::KillOnly)
                .expect("retire the actual replacement");
            assert_eq!(replacement_tracker.active_processes(), 0);
            assert!(
                !lean_toolchain::process_group::group_present(replacement_pid)
                    .expect("check replacement group closure")
            );
            assert_eq!(
                std::fs::read_to_string(root.join("dispatch")).expect("read original frame ledger"),
                "request\n"
            );
        }
    }
    std::fs::remove_dir_all(fixture).expect("remove managed fixture directory");
}

const REAL_INPUT: &str = "LEAN_RS_TEST_MANAGED_REAL_INPUT";
const REAL_FREEZE: &str = "LEAN_RS_TEST_MANAGED_FREEZE_ROOT";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RealInput {
    worker: PathBuf,
    worker_sha256: String,
    helper: PathBuf,
    helper_sha256: String,
    sysroot: PathBuf,
    fixture: PathBuf,
    scratch: PathBuf,
    prepared: lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts,
}

impl RealInput {
    fn selected() -> Self {
        let raw = std::env::var(REAL_INPUT).expect("supply pinned real worker and prepared SDK fixture");
        assert!(raw.len() <= 64 * 1024);
        let selected: Self = serde_json::from_str(&raw).expect("parse pinned managed fixture");
        for (path, expected) in [
            (&selected.worker, &selected.worker_sha256),
            (&selected.helper, &selected.helper_sha256),
        ] {
            assert_eq!(path.canonicalize().expect("canonical selected executable"), *path);
            let output = Command::new("/usr/bin/shasum")
                .args(["-a", "256"])
                .arg(path)
                .output()
                .expect("hash selected executable");
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).split_whitespace().next(),
                Some(expected.as_str())
            );
        }
        assert!(std::env::var_os("LEAN_PATH").is_none());
        selected
    }

    fn worker(&self, root: &Path) -> LeanWorker {
        let owners = self.scratch.join("owners");
        assert_eq!(
            lean_toolchain::native_guardian::registry_directory().expect("isolated registry"),
            owners
        );
        assert!(root.starts_with(&self.scratch));
        write_guardian_script(
            root,
            &std::env::current_exe().expect("portable guardian fixture executable"),
        );
        let config = crate::supervisor::LeanWorkerConfig::new(&self.worker)
            .env(ROOT, root)
            .checked_native_loader_environment()
            .env("LEAN_SYSROOT", &self.sysroot)
            .env(
                "LEAN_RS_PREPARED_HOST_ARTIFACTS",
                serde_json::to_string(&self.prepared).expect("prepared descriptor"),
            )
            .env("LEAN_HOST_MCP_WRITER_EXECUTABLE", &self.helper)
            .native_guardian(root.join("guardian"))
            .lifetime_directory(owners)
            .lifetime_socket_directory(&self.scratch);
        let mut worker = LeanWorker::spawn(&config).expect("spawn normal pinned Lean worker");
        let session = LeanWorkerSessionConfig::new(
            &self.fixture,
            "lean_rs_fixture",
            "LeanRsFixture",
            ["LeanRsFixture.Handles", "LeanRsFixture.SourceRanges"],
        );
        let session_open = worker.open_session(&session, None, None).map(|_| ());
        session_open
            .map_err(|error| {
                let retirement = worker.shutdown_child(ShutdownIntent::KillOnly);
                (error, retirement)
            })
            .expect("open prepared real Lean session or report its retirement");
        worker
    }
}

fn check_prepared_handle(selected: &RealInput, root: &Path) {
    let executable = std::env::current_exe().expect("portable guardian fixture executable");
    write_guardian_script(root, &executable);
    let child = crate::LeanWorkerChild::for_toolchain(&selected.worker, &selected.sysroot)
        .checked_native_loader_environment()
        .shared_writer_executable(&selected.helper)
        .native_guardian(root.join("guardian"))
        .lifetime_directory(selected.scratch.join("owners"))
        .lifetime_socket_directory(&selected.scratch)
        .prepared_host_artifacts(selected.prepared.clone());
    let tracker = LeanWorkerProcessTracker::new();
    let mut handle = crate::LeanWorkerHostHandleBuilder::shims_only(
        &selected.fixture,
        ["LeanRsFixture.Handles", "LeanRsFixture.SourceRanges"],
    )
    .worker_child(child)
    .process_tracker(tracker.clone())
    .open()
    .expect("open the normal prepared Lean host handle");
    let expected = handle
        .managed_guardian_binding()
        .expect("observe exact guarded receiver");
    let mut wrong = expected.clone();
    wrong.generation = "0".repeat(48);
    let original = arm();
    let request = || LeanWorkerDeclarationInspectionRequest::new("LeanRsFixture.SourceRanges.knownTheorem");
    assert!(handle.inspect_admitted_declaration_once(
        request(), &wrong, original.attempt, original.stage, original.expires_ns,
    ).is_err());
    assert_eq!(
        handle
            .managed_guardian_binding()
            .expect("wrong binding did not replace the receiver"),
        expected
    );
    let ManagedInspectionOutcome { result, retirement } = handle
        .inspect_admitted_declaration_once(
            request(),
            &expected,
            original.attempt,
            original.stage,
            original.expires_ns,
        )
        .expect("authenticated managed inspection on the prepared handle");
    assert!(matches!(
        result.expect("typed Lean reply preserved"),
        LeanWorkerDeclarationInspectionResult::Found { declaration }
            if declaration.name == "LeanRsFixture.SourceRanges.knownTheorem"
    ));
    retirement.expect("prepared handle custody has retired");
    assert_eq!(tracker.active_processes(), 0);
    assert!(handle.managed_guardian_binding().is_err());
    assert!(
        handle
            .inspect_admitted_declaration_once(
                request(),
                &expected,
                original.attempt,
                original.stage,
                original.expires_ns,
            )
            .is_err()
    );
}

type Identity = lean_toolchain::native_guardian::NativeProcessIdentity;

#[derive(serde::Deserialize, serde::Serialize)]
struct RealDispatch {
    worker: Identity,
    expires_ns: u64,
}

const PREPARED_FREEZE: &str = "LEAN_RS_TEST_MANAGED_PREPARED_FREEZE_ROOT";

#[derive(serde::Deserialize, serde::Serialize)]
struct PreparedBarrier {
    worker: Identity,
    guardian: Identity,
    lock: PathBuf,
    expires_ns: u64,
}

fn signal_pid(pid: u32) -> rustix::process::Pid {
    rustix::process::Pid::from_raw(i32::try_from(pid).expect("fixture PID fits")).expect("positive fixture PID")
}

/// Called only after the exact guardian ACK and successful prepared open and
/// custody query. The worker is stopped before any inspection byte is written.
pub(super) fn before_prepared_inspection_dispatch(worker: &LeanWorker, arm: &ManagedRequestDeadline) {
    use rustix::process::{Signal, WaitId, WaitIdOptions, kill_process, waitid};
    let Some(root) = std::env::var_os(PREPARED_FREEZE).map(PathBuf::from) else {
        return;
    };
    let barrier: PreparedBarrier =
        serde_json::from_slice(&std::fs::read(root.join("prepared")).expect("read acknowledged custody"))
            .expect("decode prepared custody");
    let owned = worker.child.as_ref().expect("exact active child");
    assert_eq!(owned.id(), barrier.worker.pid);
    assert_eq!(arm.expires_ns, barrier.expires_ns);
    assert!(worker.host_runtime_metadata().is_some());
    assert_eq!(
        Identity::capture(owned.id()).expect("read exact worker birth"),
        Some(barrier.worker),
    );
    let pid = signal_pid(owned.id());
    kill_process(pid, Signal::STOP).expect("stop exact disposable worker before inspection");
    let until = Instant::now()
        .checked_add(Duration::from_secs(1))
        .expect("stopped worker observation limit");
    loop {
        if waitid(
            WaitId::Pid(pid),
            WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .expect("observe child stop without consuming its wait")
        .is_some_and(|status| status.stopped())
        {
            break;
        }
        assert!(Instant::now() < until, "worker did not reach the pre-result barrier");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::write(
        root.join("before-inspection"),
        b"guardian armed; worker stopped before request bytes",
    )
    .expect("publish pre-result barrier");
}

pub(super) fn after_real_dispatch(worker: &LeanWorker, arm: &ManagedRequestDeadline) {
    let Some(root) = std::env::var_os(PREPARED_FREEZE)
        .or_else(|| std::env::var_os(REAL_FREEZE))
        .map(PathBuf::from)
    else {
        return;
    };
    let exact = Identity::capture(worker.child.as_ref().expect("active real Lean child").id())
        .expect("capture real Lean process")
        .expect("real Lean process is present");
    let record = RealDispatch {
        worker: exact,
        expires_ns: arm.expires_ns,
    };
    let pending = root.join("dispatched.pending");
    std::fs::write(&pending, serde_json::to_vec(&record).expect("dispatch evidence")).expect("stage dispatch evidence");
    std::fs::rename(pending, root.join("dispatched")).expect("publish accepted pipe dispatch");
    let own = rustix::process::getpid();
    rustix::process::kill_process(own, rustix::process::Signal::STOP).expect("suspend disposable requester");
}

#[test]
fn managed_cancellation_stops_only_the_original_live_receiver_without_replay() {
    use crate::LeanWorkerCancellationToken;
    let fixture = std::env::temp_dir().join(format!("lean-managed-cancel-{}", std::process::id()));
    std::fs::create_dir(&fixture).expect("create cancellation fixture directory");
    let root = fixture.canonicalize().expect("resolve cancellation fixture directory");
    std::fs::write(root.join("hold-inspection-reply"), b"hold the first reply").expect("hold reply");
    let (mut worker, tracker, pid) = worker(&root);
    let expected = scope(&worker, &root.join("owners"));
    assert_eq!(expected.worker.pid, pid);
    let request = LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero");
    let original = arm();
    let token = LeanWorkerCancellationToken::new();
    let waiter_token = token.clone();
    let ledger = root.join("dispatch");
    let child = expected.worker.clone();
    let canceller = std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(2);
        while std::fs::read_to_string(&ledger).ok().as_deref() != Some("request\n") {
            assert!(Instant::now() < until, "the actual worker must receive one inspection");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(child.is_live().expect("exact worker observation"));
        waiter_token.cancel();
    });
    worker
        .validate_managed_receiver(&expected)
        .expect("validate exact managed receiver");
    let outcome = worker
        .inspect_declaration_managed_once_with(request.clone(), &original, None, Some(&token), |_| {})
        .expect("consumed managed request");
    canceller.join().expect("cancellation followed actual dispatch");
    assert!(matches!(outcome.result, Err(LeanWorkerError::Cancelled { .. })));
    outcome.retirement.expect("retirement must independently close");
    assert_eq!(tracker.active_processes(), 0);
    assert!(!expected.worker.is_live().expect("old worker no longer live"));
    assert!(!expected.guardian.is_live().expect("old guardian no longer live"));
    assert!(
        worker
            .inspect_admitted_declaration_once(
                request,
                &expected,
                original.attempt,
                original.stage,
                original.expires_ns
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(root.join("dispatch")).expect("read actual inspection frames"),
        "request\n"
    );
    let replacement_root = root.join("r");
    std::fs::create_dir(&replacement_root).expect("create independent receiver fixture");
    let (mut replacement, replacement_tracker, _) = self::worker(&replacement_root);
    let replacement_scope = scope(&replacement, &replacement_root.join("owners"));
    assert_ne!(expected.worker, replacement_scope.worker);
    assert!(token.is_cancelled());
    let next = replacement.next_request_id;
    assert!(
        replacement
            .inspect_admitted_declaration_once(
                LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero"),
                &expected,
                original.attempt,
                original.stage,
                original.expires_ns,
            )
            .is_err()
    );
    assert_eq!(replacement.next_request_id, next);
    replacement
        .validate_managed_receiver(&replacement_scope)
        .expect("old cancellation left replacement live");
    let fresh = inspect(
        &mut replacement,
        &replacement_root.join("owners"),
        "Nat.add_zero",
        &arm(),
    )
    .expect("independent grant is unaffected");
    assert_eq!(
        fresh.result.expect("independent response"),
        LeanWorkerDeclarationInspectionResult::Unsupported
    );
    fresh.retirement.expect("close independent receiver");
    assert_eq!(replacement_tracker.active_processes(), 0);
    assert_eq!(
        std::fs::read_to_string(replacement_root.join("dispatch")).expect("fresh frames"),
        "request\n"
    );
    std::fs::remove_dir_all(fixture).expect("remove closed cancellation fixture");
}

#[test]
fn real_lean_requester_fixture() {
    let Some(root) = std::env::var_os(REAL_FREEZE).map(PathBuf::from) else {
        return;
    };
    let selected = RealInput::selected();
    let mut worker = selected.worker(&root);
    let outcome = inspect(
        &mut worker,
        &selected.scratch.join("owners"),
        "LeanRsFixture.SourceRanges.knownTheorem",
        &arm(),
    )
    .expect("run one disposable real Lean request");
    outcome.retirement.expect("requester reaps after resume");
}

#[test]
#[ignore = "requires a pinned prepared Lean 4.36 SDK fixture through LEAN_RS_TEST_MANAGED_REAL_INPUT"]
fn real_lean_managed_deadline_and_expired_dispatch() {
    let selected = RealInput::selected();
    for case in ["prepared_handle", "success", "expiry", "frozen"] {
        let root = selected.scratch.join(case);
        std::fs::create_dir(&root).expect("create clean disposable case");
        if case == "frozen" {
            check_frozen_real_requester(&root, false);
        } else if case == "prepared_handle" {
            check_prepared_handle(&selected, &root);
        } else {
            let mut worker = selected.worker(&root);
            let mut original = arm();
            if case == "expiry" {
                // The original binding predates the replacement, and is never
                // renewed when the new exact generation is ready.
                original.expires_ns = u64::try_from(
                    lean_toolchain::native_guardian::guardian_deadline_ticks()
                        .expect("clock")
                        .as_nanos(),
                )
                .expect("clock fits in u64 nanos");
                let from_previous = scope(&worker, &selected.scratch.join("owners"));
                worker
                    .shutdown_child(ShutdownIntent::KillOnly)
                    .expect("close initial generation");
                worker = selected.worker(&root);
                let next = worker.next_request_id;
                assert!(
                    worker
                        .inspect_admitted_declaration_once(
                            LeanWorkerDeclarationInspectionRequest::new("LeanRsFixture.SourceRanges.knownTheorem"),
                            &from_previous,
                            original.attempt,
                            original.stage,
                            original.expires_ns,
                        )
                        .is_err()
                );
                assert_eq!(
                    worker.next_request_id, next,
                    "old grant cannot dispatch on a replacement"
                );
            }
            let next = worker.next_request_id;
            let outcome = inspect(
                &mut worker,
                &selected.scratch.join("owners"),
                "LeanRsFixture.SourceRanges.knownTheorem",
                &original,
            )
            .expect("enter managed inspection boundary");
            outcome.retirement.expect("prove actual custody closure");
            if case == "expiry" {
                assert!(matches!(outcome.result, Err(LeanWorkerError::Setup { .. })));
                assert_eq!(
                    worker.next_request_id, next,
                    "expiry must precede any accepted request byte"
                );
            } else {
                assert!(
                    matches!(outcome.result, Ok(LeanWorkerDeclarationInspectionResult::Found { .. })),
                    "prepared Lean 4.36 must actually inspect the known fixture theorem: {:?}",
                    outcome.result
                );
                assert_eq!(
                    Some(worker.next_request_id),
                    next.next(),
                    "one accepted real Lean request"
                );
            }
        }
        std::fs::remove_dir_all(root).expect("remove case only after closure");
    }
    std::fs::remove_dir_all(selected.scratch.join("owners")).expect("remove registry after exact closure");
}

fn check_frozen_real_requester(root: &Path, prepared: bool) {
    struct Requester(std::process::Child);
    impl Drop for Requester {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _kill = self.0.kill();
                let _wait = self.0.wait();
            }
        }
    }
    let (entry, control) = if prepared {
        ("cold_prepared_requester_fixture", PREPARED_FREEZE)
    } else {
        ("real_lean_requester_fixture", REAL_FREEZE)
    };
    let mut requester = Requester(
        Command::new(std::env::current_exe().expect("parent test binary"))
            .args(["--exact", &format!("{THIS}{entry}"), "--nocapture"])
            .env(control, root)
            .spawn()
            .expect("spawn isolated requester"),
    );
    let pid = signal_pid(requester.0.id());
    let until = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("bounded requester startup deadline");
    loop {
        if let Some((_, status)) = rustix::process::waitpid(
            Some(pid),
            rustix::process::WaitOptions::UNTRACED | rustix::process::WaitOptions::NOHANG,
        )
        .expect("observe stopped requester")
        {
            assert!(status.stopped(), "requester must stop after dispatch: {status:?}");
            break;
        }
        assert!(Instant::now() < until, "requester must reach the exact dispatch hook");
        std::thread::sleep(Duration::from_millis(5));
    }
    let RealDispatch { worker, expires_ns } =
        serde_json::from_slice(&std::fs::read(root.join("dispatched")).expect("real dispatch evidence"))
            .expect("parse exact real dispatch evidence");
    let custody = prepared.then(|| {
        assert!(
            root.join("before-inspection").is_file(),
            "guardian ACK and worker stop must precede dispatch"
        );
        let barrier: PreparedBarrier =
            serde_json::from_slice(&std::fs::read(root.join("prepared")).expect("read retained custody"))
                .expect("decode retained custody");
        assert_eq!(barrier.worker, worker);
        assert_eq!(barrier.expires_ns, expires_ns);
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&barrier.lock)
            .expect("open exact lock for competing writer");
        assert!(matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        (barrier, contender)
    });
    let hard = Duration::from_nanos(expires_ns);
    let tick = || lean_toolchain::native_guardian::guardian_deadline_ticks().expect("guardian clock");
    assert!(tick() < hard && worker.is_live().expect("live worker before expiry"));
    while worker.is_live().expect("observe exact worker execution stop") {
        assert!(
            tick() < hard.saturating_add(Duration::from_secs(1)),
            "guardian missed original deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(tick() >= hard, "worker stopped before the original deadline");
    assert!(
        requester
            .0
            .try_wait()
            .expect("requester still owns opened lifeline")
            .is_none()
    );
    if let Some((barrier, contender)) = &custody {
        assert!(
            barrier
                .guardian
                .is_live()
                .expect("exact guardian remains live while requester is suspended")
        );
        assert!(
            !root.join("retired").exists(),
            "stopped execution cannot announce custody closure"
        );
        assert!(matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    }
    rustix::process::kill_process(pid, rustix::process::Signal::CONT).expect("resume requester to reap");
    assert!(requester.0.wait().expect("reaped requester").success());
    assert!(!lean_toolchain::process_group::group_present(worker.process_group).expect("exact group absence"));
    if let Some((barrier, contender)) = custody {
        assert!(root.join("retired").is_file());
        assert!(
            !barrier
                .guardian
                .is_live()
                .expect("guardian retired after requester reaped")
        );
        contender
            .try_lock()
            .expect("overlapping writer can lock only after exact retirement");
    }
}

#[test]
#[ignore = "requires the O3 owner's pinned prepared-project worker and exact launch input"]
fn cold_managed_inspection_uses_prepared_project_loans() {
    let selected = RealInput::selected();
    let root = selected.scratch.join("cold-managed");
    std::fs::create_dir(&root).expect("create private cold test case");
    run_cold_prepared_inspection(selected, &root, false);
    std::fs::remove_dir_all(root).expect("remove closed test fixture");
}

#[test]
fn cold_prepared_requester_fixture() {
    if let Some(root) = std::env::var_os(PREPARED_FREEZE).map(PathBuf::from) {
        run_cold_prepared_inspection(RealInput::selected(), &root, true);
    }
}

#[test]
#[ignore = "requires the O3 owner's pinned prepared-project worker and exact launch input"]
fn cold_prepared_guardian_stops_frozen_request_before_result() {
    let selected = RealInput::selected();
    let root = selected.scratch.join("cold-frozen");
    std::fs::create_dir(&root).expect("create private cold frozen test case");
    check_frozen_real_requester(&root, true);
    std::fs::remove_dir_all(root).expect("remove test fixture after verified closure");
}

fn run_cold_prepared_inspection(selected: RealInput, root: &Path, freeze: bool) {
    let application = std::env::var("LEAN_RS_TEST_MANAGED_ARTIFACT_APPLICATION")
        .expect("O3's exact prepared generation launch input");
    let imports: Vec<String> = serde_json::from_str(
        &std::env::var("LEAN_RS_TEST_MANAGED_ARTIFACT_IMPORTS").expect("prepared project imports"),
    )
    .expect("typed prepared imports");
    let declaration =
        std::env::var("LEAN_RS_TEST_MANAGED_ARTIFACT_DECLARATION").expect("known theorem in the prepared project");
    write_guardian_script(
        root,
        &std::env::current_exe().expect("portable guardian fixture executable"),
    );
    let tracker = LeanWorkerProcessTracker::new();
    let registry = selected.scratch.join("owners");
    let child = crate::LeanWorkerChild::for_toolchain(&selected.worker, &selected.sysroot)
        .checked_native_loader_environment()
        .shared_writer_executable(&selected.helper)
        .native_guardian(root.join("guardian"))
        .lifetime_directory(&registry)
        .lifetime_socket_directory(&selected.scratch)
        .prepared_host_artifacts(selected.prepared)
        .shared_artifact_reads(true)
        .application_launch_input(&application)
        .expect("bind exact prepared project input");
    let mut handle = crate::LeanWorkerHostHandleBuilder::shims_only(&selected.fixture, std::iter::empty::<String>())
        .worker_child(child)
        .process_tracker(tracker.clone())
        .open()
        .expect("open cold prepared handle without importing");
    assert!(handle.host_runtime_metadata().is_none());
    let original = arm();
    let binding = handle
        .prepare_managed_artifact_read(imports.clone(), original.expires_ns)
        .expect("retain actual artifact union before any session open");
    assert!(handle.host_runtime_metadata().is_none());
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(registry.join(format!("worker-{}/owner.json", binding.generation)))
            .expect("read original acknowledged guardian record"),
    )
    .expect("parse retained domains");
    assert!(
        record
            .get("protected_domains")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|domains| domains.iter().any(|domain| {
                domain.get("project_root").and_then(serde_json::Value::as_str) == selected.fixture.to_str()
            })),
        "the original guardian must retain the actual project lock before arm"
    );
    if freeze {
        let barrier = PreparedBarrier {
            worker: binding.worker.clone(),
            guardian: binding.guardian.clone(),
            lock: lean_toolchain::shared_writer::project_lock_path(&selected.fixture).expect("exact project lock"),
            expires_ns: original.expires_ns,
        };
        std::fs::write(
            root.join("prepared"),
            serde_json::to_vec(&barrier).expect("encode original custody"),
        )
        .expect("retain original owner and deadline evidence for the test");
    }
    let outcome = handle
        .inspect_admitted_declaration_with_imports_once(
            &imports,
            LeanWorkerDeclarationInspectionRequest::new(&declaration),
            &binding,
            original.attempt,
            original.stage,
            original.expires_ns,
        )
        .expect("dispatch through the original guarded prepared handle");
    if freeze {
        assert!(
            outcome.result.is_err(),
            "stopped worker did not emit an inspection result"
        );
    } else {
        assert!(matches!(
            outcome.result.expect("real Lean theorem lookup"),
            LeanWorkerDeclarationInspectionResult::Found { declaration: found }
                if found.name == declaration
        ));
    }
    outcome.retirement.expect("close worker and guardian custody");
    assert_eq!(tracker.active_processes(), 0);
    if freeze {
        std::fs::write(root.join("retired"), b"exact owner confirmed closure").expect("publish confirmed retirement");
    }
}
