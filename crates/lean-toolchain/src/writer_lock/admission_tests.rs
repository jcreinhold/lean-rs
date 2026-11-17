//! Actual owner-check refusals, fresh SH contenders and complete-union retries.
#![allow(
    clippy::indexing_slicing,
    reason = "the private fixture has exactly two canonical project domains"
)]
use super::*;
use crate::native_guardian::NativeProcessIdentity;
use crate::writer_demand::ReadAdmission;
use std::os::unix::fs::PermissionsExt as _;

const TEST: &str = "writer_lock::tests::admission_tests::admission_child";
const MODE: &str = "LEAN_WRITER_RETRY_TEST_MODE";
const ROOT: &str = "LEAN_WRITER_RETRY_TEST_ROOT";
const PATHS: &str = "LEAN_WRITER_RETRY_TEST_PATHS";

struct Fixture {
    workspace: Workspace,
    paths: Vec<PathBuf>,
}
impl Fixture {
    fn new() -> Self {
        let workspace = Workspace::new();
        let roots = ["A", "B"].map(|name| workspace.0.join(name)).to_vec();
        for root in &roots {
            fs::create_dir(root).expect("private project");
        }
        fs::write(roots[1].join("lake-manifest.json"), br#"{"packages":[]}"#).expect("dependency graph");
        fs::write(roots[0].join("lake-manifest.json"), serde_json::to_vec(&serde_json::json!({
            "version":"1.3.0","packages":[{"name":"B","type":"path","dir":roots[1],"manifestFile":"lake-manifest.json"}]
        })).expect("graph JSON")).expect("primary graph");
        let mut paths = roots
            .iter()
            .map(|root| crate::shared_writer::project_lock_path(root).expect("lock path"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in &paths {
            drop(open(path));
        }
        let registry = workspace.0.join("owners");
        fs::create_dir(&registry).expect("private registry");
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700)).expect("private mode");
        Self { workspace, paths }
    }
    fn marker(&self) -> PathBuf {
        self.workspace.0.join("target-write")
    }
    fn record(&self, owner: &OwnedChild, native: bool) {
        let identity = NativeProcessIdentity::capture(owner.child.id())
            .expect("capture actual owner")
            .expect("live owner");
        assert_eq!(identity.pid, identity.session);
        assert_eq!(identity.pid, identity.process_group);
        if native {
            // Header-only legacy fixture for a REAL live dummy group. No
            // guardian, SH transfer or recovery authority is claimed.
            let directory = self.workspace.0.join("owners").join(format!("worker-{:048x}", 1));
            fs::create_dir(&directory).expect("owner directory");
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("owner mode");
            fs::write(directory.join("owner.json"), serde_json::to_vec(&serde_json::json!({
                "version":1,"scope":"inherited-process-group-v1","generation_id":format!("{:048x}",1),
                "parent":NativeProcessIdentity::capture(std::process::id()).expect("capture parent").expect("live parent"),"process":identity
            })).expect("native header")).expect("native owner");
        } else {
            // A valid previous-writer header with an actual living group. This
            // tests passive admission, not the helper's custody protocol.
            let mut process = serde_json::to_value(&identity).expect("identity JSON");
            process["start_new_session"] = serde_json::json!(false);
            let mut bytes = serde_json::to_vec(&serde_json::json!({"version":1,"owner":identity,"process":process}))
                .expect("writer header");
            bytes.push(b'\n');
            fs::write(&self.paths[1], bytes).expect("last-domain previous writer");
        }
    }
    fn spawn(&self, mode: &str, paths: &[PathBuf]) -> OwnedChild {
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST, "--nocapture", "--quiet", "--test-threads=1"])
            .env(MODE, mode)
            .env(ROOT, &self.workspace.0)
            .env(PATHS, serde_json::to_string(paths).expect("path JSON"))
            .env(
                crate::native_guardian::OWNERS_DIRECTORY_ENV,
                self.workspace.0.join("owners"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn actual dummy child");
        let stderr = child.stderr.take().expect("observations");
        let (send, observations) = mpsc::channel();
        thread::spawn(move || {
            for line in io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        OwnedChild { child, observations }
    }
    fn assert_no_write(&self, owner: &mut OwnedChild) {
        assert!(
            owner.child.try_wait().expect("owner status").is_none(),
            "owner must still live"
        );
        assert!(!self.marker().exists(), "writer ran before the recorded owner stopped");
    }
}
fn open(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("fresh lock description")
}
fn shared(paths: &[PathBuf]) -> Option<Vec<File>> {
    let mut files = Vec::new();
    for path in paths {
        let file = open(path);
        match fs4::FileExt::try_lock_shared(&file) {
            Ok(()) => files.push(file),
            Err(fs4::TryLockError::WouldBlock) => return None,
            Err(error) => panic!("SH error: {error}"),
        }
    }
    Some(files)
}
fn demand_present(paths: &[PathBuf]) {
    assert!(
        ReadAdmission::try_acquire(paths)
            .expect("ordinary reader gate")
            .is_none(),
        "writer demand must span every retry"
    );
}
fn all_ex_blocked(paths: &[PathBuf]) {
    for path in paths {
        assert!(
            shared(std::slice::from_ref(path)).is_none(),
            "initial actual EX must cover {}",
            path.display()
        );
    }
}

// Observation barrier only: production has already obtained real EX and the
// real owner check returned false. It never changes that result or closes FDs.
pub(super) fn admission_wait(path: &Path, reason: &str) {
    if !std::env::var(MODE).is_ok_and(|value| value.starts_with("writer")) {
        return;
    }
    let root = PathBuf::from(std::env::var_os(ROOT).expect("fixture root"));
    let marker = root.join("actual-refusal");
    if marker.exists() {
        return;
    }
    fs::write(marker, path.as_os_str().as_encoded_bytes()).expect("actual refusal marker");
    report(reason);
    assert_eq!(instruction(), "retry");
}

#[test]
fn admission_child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT).expect("root"));
    let paths: Vec<PathBuf> = serde_json::from_str(&std::env::var(PATHS).expect("paths")).expect("path JSON");
    match mode.as_str() {
        "owner" => {
            rustix::process::setsid().expect("owned dummy private session");
            report("owner-live");
            assert_eq!(instruction(), "release");
        }
        "shared" => {
            assert!(
                shared(&paths).is_none(),
                "contender must really block before writer retries"
            );
            report("shared-blocked");
            assert_eq!(instruction(), "retry");
            let started = Instant::now();
            let files = loop {
                if let Some(files) = shared(&paths) {
                    break files;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(1),
                    "fresh recovery SH blocked by ungranted writer EX"
                );
                thread::sleep(Duration::from_millis(2));
            };
            report("shared-acquired");
            assert_eq!(instruction(), "release");
            drop(files);
        }
        "writer-single" | "writer-union" | "writer-kernel" | "writer-timeout" | "writer-cancel" | "writer-zero"
        | "writer-malformed" => {
            let cancelled = || root.join("cancel").exists();
            let timeout = if mode == "writer-timeout" {
                Duration::from_millis(250)
            } else if mode == "writer-zero" {
                Duration::ZERO
            } else {
                WAIT
            };
            let result = if mode == "writer-single" {
                WriterLock::acquire_project(&paths[0], timeout, &cancelled).map(|guard| vec![guard])
            } else {
                WriterLock::acquire_projects(&root.join("A"), &[], timeout, &cancelled).map(|(guards, _)| guards)
            };
            if mode == "writer-timeout" || mode == "writer-cancel" || mode == "writer-malformed" {
                let error = match result {
                    Ok(_guards) => panic!("refused writer ran"),
                    Err(error) => error,
                };
                assert_eq!(
                    error.kind(),
                    if mode == "writer-timeout" {
                        io::ErrorKind::TimedOut
                    } else if mode == "writer-malformed" {
                        io::ErrorKind::Other
                    } else {
                        io::ErrorKind::Interrupted
                    }
                );
                assert_eq!(
                    crate::shared_writer::WriterFailure::from_io(&error).map(|value| value.disposition),
                    Some(crate::shared_writer::WriterDisposition::NotStarted)
                );
                assert!(
                    shared(&paths).is_some(),
                    "all partial EX descriptions must close on error"
                );
                assert!(ReadAdmission::try_acquire(&paths).expect("closed demand").is_some());
                report("writer-error");
            } else {
                let _guards = result.expect("admitted writer");
                fs::write(root.join("target-write"), b"actual post-admission write").expect("target write");
                report("writer-acquired");
                assert_eq!(instruction(), "release");
            }
        }
        "bypass-owner-control" => {
            let file = open(&paths[0]);
            fs4::FileExt::try_lock(&file).expect("raw EX free despite live recorded owner");
            fs::write(root.join("target-write"), b"deliberate unsafe-control write").expect("negative write");
            report("unsafe-write");
            assert_eq!(instruction(), "release");
        }
        other => panic!("unexpected mode {other}"),
    }
}

fn run_refused_writer(native: bool, multiple: bool) {
    let fixture = Fixture::new();
    let mut owner = fixture.spawn("owner", &fixture.paths);
    owner.observe("owner-live");
    fixture.record(&owner, native);
    let paths = if multiple {
        fixture.paths.clone()
    } else {
        vec![fixture.paths[1].clone()]
    };
    let mut writer = fixture.spawn(if multiple { "writer-union" } else { "writer-single" }, &paths);
    writer.observe("owner-refused");
    all_ex_blocked(&paths);
    demand_present(&paths);
    fixture.assert_no_write(&mut owner);
    let mut recovery = fixture.spawn("shared", &paths);
    recovery.observe("shared-blocked");
    recovery.tell("retry");
    writer.tell("retry");
    recovery.observe("shared-acquired");
    demand_present(&paths);
    fixture.assert_no_write(&mut owner);
    owner.tell("release");
    owner.wait();
    // The owner is now gone, but the independently acquired SH still prevents
    // a post-admission write. A broken owner-fence bypass also fails above.
    assert!(!fixture.marker().exists());
    recovery.tell("release");
    recovery.wait();
    writer.observe("writer-acquired");
    assert!(fixture.marker().exists());
    demand_present(&paths);
    writer.tell("release");
    writer.wait();
    assert!(ReadAdmission::try_acquire(&paths).expect("demand released").is_some());
    assert!(shared(&paths).is_some());
}
#[test]
fn owner_refusal_releases_single_candidate_for_fresh_shared_reader() {
    for native in [false, true] {
        run_refused_writer(native, false);
    }
}
#[test]
fn owner_refusal_releases_every_partial_domain_before_retry() {
    run_refused_writer(false, true);
}
#[test]
fn refused_union_timeout_and_cancel_close_candidates_and_demand() {
    for mode in ["writer-timeout", "writer-cancel"] {
        let fixture = Fixture::new();
        let mut owner = fixture.spawn("owner", &fixture.paths);
        owner.observe("owner-live");
        fixture.record(&owner, false);
        let mut writer = fixture.spawn(mode, &fixture.paths);
        writer.observe("owner-refused");
        all_ex_blocked(&fixture.paths);
        if mode == "writer-cancel" {
            fs::write(fixture.workspace.0.join("cancel"), b"cancel").expect("cancel marker");
        }
        writer.tell("retry");
        writer.observe("writer-error");
        writer.wait();
        fixture.assert_no_write(&mut owner);
        assert!(shared(&fixture.paths).is_some());
        assert!(
            ReadAdmission::try_acquire(&fixture.paths)
                .expect("no leftover demand")
                .is_some()
        );
        owner.tell("release");
        owner.wait();
    }
}
#[test]
fn raw_ex_negative_control_exposes_a_write_despite_live_recorded_owner() {
    let fixture = Fixture::new();
    let mut owner = fixture.spawn("owner", &fixture.paths);
    owner.observe("owner-live");
    fixture.record(&owner, false);
    let file = open(&fixture.paths[1]);
    assert!(!crate::shared_writer::previous_writer_stopped(&file).expect("actual live group"));
    drop(file);
    let mut bypass = fixture.spawn("bypass-owner-control", &fixture.paths[1..]);
    bypass.observe("unsafe-write");
    assert!(owner.child.try_wait().expect("owner live").is_none());
    assert!(
        fixture.marker().exists(),
        "common no-write oracle must detect unsafe control"
    );
    bypass.tell("release");
    bypass.wait();
    owner.tell("release");
    owner.wait();
}

#[test]
fn kernel_contention_releases_earlier_partial_domain_before_retry() {
    let fixture = Fixture::new();
    let last = open(&fixture.paths[1]);
    fs4::FileExt::try_lock_shared(&last).expect("actual reader on last domain");
    let mut writer = fixture.spawn("writer-kernel", &fixture.paths);
    writer.observe("kernel-busy");
    assert!(
        shared(&fixture.paths[..1]).is_none(),
        "first actual partial EX held at observation barrier"
    );
    demand_present(&fixture.paths);
    let mut recovery = fixture.spawn("shared", &fixture.paths[..1]);
    recovery.observe("shared-blocked");
    recovery.tell("retry");
    writer.tell("retry");
    recovery.observe("shared-acquired");
    demand_present(&fixture.paths);
    assert!(!fixture.marker().exists());
    drop(last);
    assert!(!fixture.marker().exists(), "recovery SH still blocks full writer");
    recovery.tell("release");
    recovery.wait();
    writer.observe("writer-acquired");
    writer.tell("release");
    writer.wait();
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("closed demand")
            .is_some()
    );
}

#[test]
fn retry_anchors_hold_no_flock_and_reject_replaced_or_missing_names() {
    for replace in [false, true] {
        let fixture = Fixture::new();
        let path = &fixture.paths[0];
        let mut target = LockTarget::new(path);
        let candidate = target.candidate().expect("initial stable candidate");
        fs4::FileExt::try_lock(&candidate).expect("initial actual EX");
        drop(candidate);
        assert!(
            shared(std::slice::from_ref(path)).is_some(),
            "unlocked anchor must not block SH"
        );
        fs::rename(path, fixture.workspace.0.join("original-inode")).expect("test-only inode substitution");
        if replace {
            drop(open(path));
        }
        let error = target
            .candidate()
            .expect_err("never follow a replacement or recreate a missing retry target");
        assert_eq!(
            error.kind(),
            if replace {
                io::ErrorKind::InvalidData
            } else {
                io::ErrorKind::NotFound
            }
        );
        assert_eq!(path.exists(), replace, "retry must not create a missing target");
    }
}
#[test]
fn zero_budget_allows_one_free_union_and_malformed_owner_is_a_hard_error() {
    for malformed in [false, true] {
        let fixture = Fixture::new();
        if malformed {
            fs::write(&fixture.paths[1], b"{malformed}\n").expect("malformed committed owner");
        }
        let mut writer = fixture.spawn(
            if malformed { "writer-malformed" } else { "writer-zero" },
            &fixture.paths,
        );
        if malformed {
            writer.observe("writer-error");
        } else {
            writer.observe("writer-acquired");
            writer.tell("release");
        }
        writer.wait();
        assert_eq!(fixture.marker().exists(), !malformed);
        assert!(shared(&fixture.paths).is_some());
        assert!(
            ReadAdmission::try_acquire(&fixture.paths)
                .expect("released demand")
                .is_some()
        );
    }
}
