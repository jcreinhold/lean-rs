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

fn worker(root: &Path) -> (LeanWorker, LeanWorkerProcessTracker, u32) {
    let executable = std::env::current_exe().expect("fixture test executable");
    let binary = executable.to_string_lossy().replace('\'', "'\\''");
    std::fs::write(
        root.join("guardian"),
        format!("#!/bin/sh\nexec '{binary}' --exact {THIS}guardian_fixture --nocapture\n"),
    )
    .expect("write fixture guardian script");
    std::fs::set_permissions(root.join("guardian"), std::fs::Permissions::from_mode(0o700))
        .expect("make fixture guardian executable");
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
