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
    std::fs::write(
        root.join("guardian"),
        format!("#!/bin/sh\nexec '{binary}' --exact {THIS}guardian_fixture --nocapture\n"),
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
        let ManagedInspectionOutcome { result, retirement } = worker
            .inspect_declaration_managed_once(LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero"), &arm())
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
                .inspect_declaration_managed_once(LeanWorkerDeclarationInspectionRequest::new("Nat.add_zero"), &arm())
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

type Identity = lean_toolchain::native_guardian::NativeProcessIdentity;

#[derive(serde::Deserialize, serde::Serialize)]
struct RealDispatch {
    worker: Identity,
    expires_ns: u64,
}

pub(super) fn after_real_dispatch(worker: &LeanWorker, arm: &ManagedRequestDeadline) {
    let Some(root) = std::env::var_os(REAL_FREEZE).map(PathBuf::from) else {
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
fn real_lean_requester_fixture() {
    let Some(root) = std::env::var_os(REAL_FREEZE).map(PathBuf::from) else {
        return;
    };
    let mut worker = RealInput::selected().worker(&root);
    let outcome = worker
        .inspect_declaration_managed_once(
            LeanWorkerDeclarationInspectionRequest::new("LeanRsFixture.SourceRanges.knownTheorem"),
            &arm(),
        )
        .expect("run one disposable real Lean request");
    outcome.retirement.expect("requester reaps after resume");
}

#[test]
#[ignore = "requires a pinned prepared Lean 4.36 SDK fixture through LEAN_RS_TEST_MANAGED_REAL_INPUT"]
fn real_lean_managed_deadline_and_expired_dispatch() {
    let selected = RealInput::selected();
    for case in ["success", "expiry", "frozen"] {
        let root = selected.scratch.join(case);
        std::fs::create_dir(&root).expect("create clean disposable case");
        if case == "frozen" {
            check_frozen_real_requester(&root);
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
                worker
                    .shutdown_child(ShutdownIntent::KillOnly)
                    .expect("close initial generation");
                worker = selected.worker(&root);
            }
            let next = worker.next_request_id;
            let outcome = worker
                .inspect_declaration_managed_once(
                    LeanWorkerDeclarationInspectionRequest::new("LeanRsFixture.SourceRanges.knownTheorem"),
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

fn check_frozen_real_requester(root: &Path) {
    struct Requester(std::process::Child);
    impl Drop for Requester {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _kill = self.0.kill();
                let _wait = self.0.wait();
            }
        }
    }
    let mut requester = Requester(
        Command::new(std::env::current_exe().expect("parent test binary"))
            .args(["--exact", &format!("{THIS}real_lean_requester_fixture"), "--nocapture"])
            .env(REAL_FREEZE, root)
            .spawn()
            .expect("spawn isolated requester"),
    );
    let pid = rustix::process::Pid::from_raw(i32::try_from(requester.0.id()).expect("requester PID fits"))
        .expect("positive requester PID");
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
    rustix::process::kill_process(pid, rustix::process::Signal::CONT).expect("resume requester to reap");
    assert!(requester.0.wait().expect("reaped requester").success());
    assert!(!lean_toolchain::process_group::group_present(worker.process_group).expect("exact group absence"));
}
