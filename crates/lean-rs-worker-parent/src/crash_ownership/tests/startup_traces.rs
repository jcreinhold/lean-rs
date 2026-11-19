//! Project the SDK model's startup prefixes onto the actual Darwin guardian path.

use super::*;
use std::os::fd::AsFd as _;
use std::process::Stdio;

const BROKER: &str = "LEAN_STARTUP_TRACE_BROKER_ROOT";
const ROOT: &str = "LEAN_STARTUP_TRACE_ROOT";
const EXIT: i32 = 91;
const CUTS: [(&str, &str); 7] = [
    ("reserved", "preparing"),
    ("dispatch", "dispatching"),
    ("spawned", "dispatching"),
    ("bootstrap", "bootstrap"),
    ("running", "running"),
    ("partial-ack", "running"),
    ("acknowledged", "running"),
];
const LEAN: &str = r#"
example (n : Nat) : n + 0 = n := Nat.add_zero n

def main : IO Unit := do
  IO.FS.writeFile "lean-started" "checked\n"
  for _ in [0:200] do
    if ← ("writer-admitted" : System.FilePath).pathExists then
      IO.FS.writeFile "lean-late-write" "late\n"
      return
    IO.sleep 20
  IO.FS.writeFile "lean-expired" "watchdog\n"
"#;

#[derive(serde::Serialize, serde::Deserialize)]
struct Cut {
    record: PathBuf,
    socket: PathBuf,
    state: serde_json::Value,
    scope: Option<(LeanWorkerOwnedProcess, LeanWorkerOwnedProcess)>,
}

fn slot() -> native_guardian::NativeCapacitySlot {
    native_guardian::NativeCapacitySlot {
        key: "startup-trace".into(),
        scope: "main".into(),
        limit: 1,
    }
}

fn value(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[expect(
    clippy::exit,
    reason = "abrupt exit of the fixture without dropping its active lease"
)]
fn report_cut(
    root: &Path,
    record: PathBuf,
    socket: PathBuf,
    scope: Option<(LeanWorkerOwnedProcess, LeanWorkerOwnedProcess)>,
) -> ! {
    let observed = Cut {
        state: value(&record),
        record,
        socket,
        scope,
    };
    let staged = root.join("cut.write");
    fs::write(&staged, serde_json::to_vec(&observed).unwrap()).unwrap();
    fs::rename(&staged, root.join("cut.json")).unwrap();
    wait_for(|| root.join("crash-now").exists());
    std::process::exit(EXIT);
}

#[test]
fn broker() {
    let Some(root) = std::env::var_os(BROKER).map(PathBuf::from) else {
        return;
    };
    let prefix: usize = std::env::var("LEAN_STARTUP_TRACE_PREFIX").unwrap().parse().unwrap();
    let mut lifetime =
        ParentLifetime::prepare_guarded_with_slot(&root.join("owners"), Duration::from_secs(2), Some(&slot())).unwrap();
    let record = lifetime.directory.join("owner.json");
    let socket = lifetime.socket_directory.clone();
    if prefix < 2 {
        if prefix == 1 {
            lifetime.dispatch().unwrap();
        }
        report_cut(&root, record, socket, None);
    }
    let mut command = native_worker_command(&root);
    command.env("LEAN_STARTUP_FAULT_ROOT", &root);
    let tracker = process::LeanWorkerProcessTracker::new();
    lifetime.configure(&mut command);
    let mut worker = tracker.spawn_guarded(&mut command, &mut lifetime).unwrap();
    if prefix == 2 {
        // Hold the real guardian's connection open before the parent's
        // authentication and publication. The observer can see both births;
        // the registry still has only dispatching until the guardian drains.
        let mut connection = None;
        wait_for(|| {
            let accepted = lifetime.listener.accept();
            if accepted
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
            {
                return false;
            }
            connection = Some(accepted.unwrap().0);
            true
        });
        let _connection = connection.unwrap();
        let process = LeanWorkerOwnedProcess::capture(worker.id()).unwrap().unwrap();
        let guardian: LeanWorkerOwnedProcess =
            serde_json::from_slice(&fs::read(root.join("guardian-identity.json")).unwrap()).unwrap();
        assert_eq!(process.session, process.pid);
        assert_eq!(process.process_group, process.pid);
        assert_eq!(guardian.session, process.pid);
        assert_eq!(guardian.process_group, guardian.pid);
        assert_eq!(
            LeanWorkerOwnedProcess::capture(guardian.pid).unwrap(),
            Some(guardian.clone())
        );
        report_cut(&root, record, socket, Some((process, guardian)));
    }
    let lease = lifetime.accept(worker.id(), Duration::from_secs(3)).unwrap();
    let scope = Some((lease.owner.clone(), lease.guardian.clone().unwrap()));
    if prefix == 3 {
        report_cut(&root, record, socket, scope);
    }
    worker.attach_lifetime(lease).unwrap();
    if prefix == 5 {
        let path = root.clone();
        let record = record.clone();
        let socket = socket.clone();
        let scope = scope.clone();
        let _hook = startup_faults::on_first_ack_write(move |written| {
            assert_eq!(written, 1);
            fs::write(path.join("partial-wire"), written.to_string()).unwrap();
            report_cut(&path, record, socket, scope);
        });
        worker.acknowledge_lifetime().unwrap();
    }
    if prefix == 6 {
        worker.acknowledge_lifetime().unwrap();
        wait_for(|| {
            fs::read(root.join("lean-started"))
                .as_deref()
                .is_ok_and(|bytes| bytes == b"checked\n")
        });
    }
    report_cut(&root, record, socket, scope);
}

pub(super) fn run_lean_body(root: &Path, guardian: &mut native_guardian::GuardianHandle) {
    let program = root.join("Probe.lean");
    if !program.is_file() {
        return;
    }
    let project = root.join("read-project");
    let lock = lean_toolchain::shared_writer::project_lock_path(&project).unwrap();
    let file = OpenOptions::new().read(true).write(true).open(&lock).unwrap();
    file.try_lock_shared().unwrap();
    let metadata = file.metadata().unwrap();
    guardian
        .retain_reads(
            &[native_guardian::GuardianReadLoan {
                domain: native_guardian::NativeReadDomain {
                    project_root: project,
                    lock_path: lock,
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    epoch: "startup-trace".to_owned(),
                },
                descriptor: file.as_fd(),
            }],
            Duration::from_secs(2),
        )
        .unwrap();
    drop(file);
    let lean = PathBuf::from(std::env::var_os("LEAN_SYSROOT").unwrap()).join("bin/lean");
    assert!(
        Command::new(lean)
            .arg("--run")
            .arg(program)
            .current_dir(root)
            .env_remove("LEAN_PATH")
            .env_remove("LEAN_SRC_PATH")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
#[ignore = "requires the installed Lean binary and fresh disposable Darwin inputs"]
fn supervisor_loss_at_generated_startup_prefixes() {
    let root = PathBuf::from(std::env::var_os(ROOT).expect("fresh output path required"));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let root = root.canonicalize().unwrap();
    // Check the projection shortest first, so a failure names the shortest
    // counterexample in the covered prefix family.
    for (prefix, (label, phase)) in CUTS.into_iter().enumerate() {
        eprintln!("startup prefix {prefix}: {label}");
        run_prefix(&root.join(label), prefix, phase);
    }
}

fn run_prefix(root: &Path, prefix: usize, expected_phase: &str) {
    fs::DirBuilder::new().mode(0o700).create(root).unwrap();
    let project = root.join("read-project");
    fs::create_dir(&project).unwrap();
    fs::write(root.join("Probe.lean"), LEAN).unwrap();
    write_guardian_script(root);
    let lock = lean_toolchain::shared_writer::project_lock_path(&project).unwrap();
    let exclusive = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    let mut broker = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::startup_traces::broker",
            "--nocapture",
        ])
        .env(BROKER, root)
        .env("LEAN_STARTUP_TRACE_PREFIX", prefix.to_string())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let cut = root.join("cut.json");
    wait_for(|| cut.exists());
    let cut: Cut = serde_json::from_slice(&fs::read(cut).unwrap()).unwrap();
    assert_eq!(
        cut.state.get("phase").and_then(serde_json::Value::as_str),
        Some(expected_phase)
    );
    if let Some((process, guardian)) = &cut.scope {
        assert!(process.is_live().unwrap() && guardian.is_live().unwrap());
        assert!(lean_toolchain::process_group::group_present(process.process_group).unwrap());
    }
    let started = root.join("lean-started");
    if prefix == 5 {
        assert_eq!(fs::read(root.join("partial-wire")).unwrap(), b"1");
    }
    if prefix == 6 {
        assert_eq!(fs::read(&started).unwrap(), b"checked\n");
        assert!(matches!(exclusive.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    } else {
        assert!(!started.exists());
    }
    let registry = root.join("owners");
    assert_eq!(
        LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(2), Some(&slot()))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    fs::write(root.join("crash-now"), b"crash this owned fixture").unwrap();
    let mut status = None;
    wait_for(|| {
        status = broker.try_wait().unwrap();
        status.is_some()
    });
    assert_eq!(status.unwrap().code(), Some(EXIT));
    if let Some((process, guardian)) = cut.scope {
        wait_for(|| {
            !process.is_live().unwrap()
                && !lean_toolchain::process_group::group_present(process.process_group).unwrap()
                && !guardian.is_live().unwrap()
                && !lean_toolchain::process_group::group_present(guardian.process_group).unwrap()
        });
        if matches!(prefix, 2 | 3) {
            assert_eq!(
                value(&cut.record).get("phase"),
                Some(&serde_json::json!("bootstrap_closed"))
            );
        }
    }
    if prefix == 1 {
        // A new observer cannot know this dispatch did not create a child.
        assert_eq!(
            LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(2), Some(&slot()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(value(&cut.record).get("phase"), Some(&serde_json::json!("dispatching")));
    } else {
        if prefix == 3 {
            compete_for_closed_slot(root, &cut.record);
        } else {
            drop(LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(2), Some(&slot())).unwrap());
        }
        assert!(!cut.record.exists());
        exclusive.try_lock().unwrap();
        let metadata = exclusive.metadata().unwrap();
        assert!(
            native_guardian::protected_native_reads_stopped(&registry, &lock, (metadata.dev(), metadata.ino()))
                .unwrap()
        );
        fs::write(root.join("writer-admitted"), b"independent same-inode EX").unwrap();
    }
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(started.exists(), prefix == 6);
    for marker in ["lean-late-write", "lean-expired", "worker-rescue", "guardian-rescue"] {
        assert!(!root.join(marker).exists(), "unexpected marker {marker}");
    }
    if cut.socket.exists() {
        // These cuts skip ParentLifetime::accept, which removes the private socket directory.
        assert!(prefix <= 2);
        fs::remove_file(cut.socket.join("parent.sock")).unwrap();
        fs::remove_dir(&cut.socket).unwrap();
    }
    fs::write(root.join("closed"), b"observed exact native scope and cleanup").unwrap();
}

#[test]
fn collector() {
    let Some(root) = std::env::var_os("LEAN_STARTUP_COLLECT_ROOT").map(PathBuf::from) else {
        return;
    };
    let name = std::env::var("LEAN_STARTUP_COLLECT_NAME").unwrap();
    fs::write(root.join(format!("{name}-ready")), b"ready").unwrap();
    wait_for(|| root.join("collect-go").exists());
    let result = LeanWorkerNativeScope::prepare(&root.join("owners"), Duration::from_secs(2), Some(&slot()));
    let outcome = if let Err(error) = &result {
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            error.to_string(),
            "native actor key belongs to another parent incarnation"
        );
        "refused"
    } else {
        "admitted"
    };
    let pending = root.join(format!("{name}.write"));
    fs::write(&pending, outcome).unwrap();
    fs::rename(&pending, root.join(format!("{name}.result"))).unwrap();
    wait_for(|| root.join("collect-release").exists());
    drop(result);
}

fn compete_for_closed_slot(root: &Path, predecessor: &Path) {
    let names = ["collector-a", "collector-b"];
    let children = names.map(|name| {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_ownership::lifecycle_tests::startup_traces::collector"])
            .env("LEAN_STARTUP_COLLECT_ROOT", root)
            .env("LEAN_STARTUP_COLLECT_NAME", name)
            .spawn()
            .unwrap()
    });
    wait_for(|| names.iter().all(|name| root.join(format!("{name}-ready")).exists()));
    fs::write(root.join("collect-go"), b"start both collectors").unwrap();
    wait_for(|| names.iter().all(|name| root.join(format!("{name}.result")).exists()));
    let mut outcomes = names.map(|name| fs::read_to_string(root.join(format!("{name}.result"))).unwrap());
    outcomes.sort();
    assert_eq!(outcomes, ["admitted", "refused"]);
    assert!(!predecessor.exists());
    fs::write(root.join("collect-release"), b"exit both collectors").unwrap();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
}
