//! Real process and descriptor regressions for compatible writer demand.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "isolated real-process fixtures assert precise demand and close-only custody"
)]
use super::*;
use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};

const WAIT: Duration = Duration::from_secs(5);
const CHILD: &str = "writer_demand::tests::demand_test_child";
const MODE_ENV: &str = "LEAN_TOOLCHAIN_DEMAND_TEST_MODE";
const PATHS_ENV: &str = "LEAN_TOOLCHAIN_DEMAND_TEST_PATHS";
const ROOT_ENV: &str = "LEAN_TOOLCHAIN_DEMAND_TEST_ROOT";
struct Fixture {
    directory: PathBuf,
    roots: Vec<PathBuf>,
    paths: Vec<PathBuf>,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "lean-writer-demand-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("private fixture");
        let directory = path.canonicalize().expect("canonical fixture");
        let roots = ["A", "B"]
            .into_iter()
            .map(|name| {
                let root = directory.join(name);
                fs::create_dir(&root).expect("project root");
                root
            })
            .collect::<Vec<_>>();
        let paths = roots
            .iter()
            .map(|root| crate::shared_writer::project_lock_path(root).expect("project lock"))
            .collect();
        Self {
            directory,
            roots,
            paths,
        }
    }
    fn project_graph(&self) {
        fs::write(self.roots[1].join("lake-manifest.json"), br#"{"packages":[]}"#).expect("dependency metadata");
        fs::write(self.roots[0].join("lake-manifest.json"),serde_json::to_vec(&serde_json::json!({
            "version":"1.3.0","packages":[{"name":"B","type":"path","dir":self.roots[1],"manifestFile":"lake-manifest.json"}]
        })).expect("graph JSON")).expect("project graph");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.directory);
    }
}
#[test]
fn local_artifact_reader_distinguishes_artifact_and_private_demand_modes() {
    let fixture = Fixture::new();
    for path in &fixture.paths {
        fs::write(path, []).expect("empty artifact lock");
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("standard artifact mode");
        let _gate = open(&demand_path(path).expect("stable demand path")).expect("private demand lock");
    }
    let reader = crate::shared_writer::LocalArtifactRead::try_acquire(&fixture.roots)
        .expect("standard artifact mode is readable under private demand gates");
    reader.validate().expect("retained artifact identities");
    drop(reader);

    let demand = demand_path(&fixture.paths[0]).expect("first demand path");
    fs::set_permissions(&demand, fs::Permissions::from_mode(0o644)).expect("nonprivate disposable demand gate");
    let error = crate::shared_writer::LocalArtifactRead::try_acquire(&fixture.roots)
        .expect_err("nonprivate demand gate must still refuse");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn immediate_writer_refuses_readers_then_admits_after_release() {
    let fixture = Fixture::new();
    fixture.project_graph();
    let (writer, roots) =
        crate::writer_lock::WriterLock::acquire_projects(&fixture.roots[0], &[], Duration::ZERO, &|| false)
            .expect("create disposable artifact and demand locks");
    drop(writer);
    let reader = crate::shared_writer::LocalArtifactRead::try_acquire(&roots).expect("hold shared artifact union");
    let refusal = crate::writer_lock::WriterLock::acquire_projects(&fixture.roots[0], &[], Duration::ZERO, &|| false);
    assert!(matches!(refusal, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    reader.validate().expect("contending writer changed no reader custody");
    drop(reader);
    let (writer, fresh_roots) =
        crate::writer_lock::WriterLock::acquire_projects(&fixture.roots[0], &[], Duration::ZERO, &|| false)
            .expect("fresh immediate writer now owns the union");
    assert_eq!(fresh_roots, roots);
    assert_eq!(writer.len(), roots.len());
}

struct Process {
    child: Child,
    lines: Receiver<String>,
}
impl Process {
    fn start(fixture: &Fixture, mode: &str, paths: &[PathBuf], loans: &[BorrowedFd<'_>]) -> Self {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", CHILD, "--nocapture", "--quiet", "--test-threads=1"])
            .env(MODE_ENV, mode)
            .env(PATHS_ENV, serde_json::to_string(paths).expect("paths JSON"))
            .env(ROOT_ENV, &fixture.roots[0])
            .env(
                crate::native_guardian::OWNERS_DIRECTORY_ENV,
                fixture.directory.join("owners"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = crate::process_group::spawn_inheriting(command, loans).expect("spawn fixture child");
        let stderr = child.stderr.take().expect("observations");
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }
    fn observed(&self, expected: &str) {
        let started = Instant::now();
        let mut diagnostics = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT.saturating_sub(started.elapsed()))
                .unwrap_or_else(|error| panic!("missing {expected}: {error}; {diagnostics:?}"));
            if let Some(value) = line.strip_prefix("demand-fixture:") {
                assert_eq!(value, expected);
                return;
            }
            diagnostics.push(line);
        }
    }
    fn finish(&mut self) {
        writeln!(self.child.stdin.as_mut().expect("control"), "release").expect("release");
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                assert!(status.success(), "{status}");
                return;
            }
            assert!(started.elapsed() < WAIT, "fixture did not close");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _kill = self.child.kill();
        }
        let _wait = self.child.wait();
    }
}
fn report(message: &str) {
    writeln!(io::stderr(), "demand-fixture:{message}").expect("report");
}
fn await_release() {
    let mut line = String::new();
    assert!(io::stdin().read_line(&mut line).expect("instruction") > 0);
    assert_eq!(line.trim(), "release");
}
#[test]
fn demand_test_child() {
    let Some(mode) = std::env::var_os(MODE_ENV) else {
        return;
    };
    let paths: Vec<PathBuf> = serde_json::from_str(&std::env::var(PATHS_ENV).expect("paths")).expect("paths JSON");
    match mode.to_str().expect("mode") {
        "writer" => {
            let _demand = WriterDemand::announce(&paths, WAIT, &|| false).expect("compatible writer");
            report("holding");
            await_release();
        }
        "gate" => {
            let _gate = ReadAdmission::try_acquire(&paths).expect("gate").expect("idle domain");
            report("holding");
            await_release();
        }
        "inherited" => {
            report("holding");
            await_release();
        }
        "project" => {
            report("waiting");
            let _guard =
                crate::writer_lock::WriterLock::acquire_project(&paths[0], WAIT, &|| false).expect("single project EX");
            report("acquired");
            await_release();
        }
        "projects" => {
            report("waiting");
            let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("project"));
            let _guards =
                crate::writer_lock::WriterLock::acquire_projects(&root, &[], WAIT, &|| false).expect("project EX");
            report("acquired");
            await_release();
        }
        other => panic!("unexpected fixture mode {other}"),
    }
}
#[test]
fn compatible_real_writers_hold_every_domain_until_last_close() {
    let fixture = Fixture::new();
    let mut first = Process::start(&fixture, "writer", &fixture.paths, &[]);
    first.observed("holding");
    let mut reversed = fixture.paths.clone();
    reversed.reverse();
    let mut second = Process::start(&fixture, "writer", &reversed, &[]);
    second.observed("holding");
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("reader probe")
            .is_none()
    );
    first.finish();
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("remaining writer")
            .is_none()
    );
    assert!(second.child.try_wait().expect("second stays live").is_none());
    second.finish();
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("last close")
            .is_some()
    );
}
#[test]
fn duplicated_demand_description_survives_controller_guard_and_actual_child_exit() {
    let fixture = Fixture::new();
    let demand = WriterDemand::announce(&fixture.paths, WAIT, &|| false).expect("demand");
    let loans = demand.files.iter().map(|file| file.as_fd()).collect::<Vec<_>>();
    let mut child = Process::start(&fixture, "inherited", &fixture.paths, &loans);
    child.observed("holding");
    drop(loans);
    drop(demand);
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("inherited demand remains")
            .is_none()
    );
    child.finish();
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("actual exit closes aliases")
            .is_some()
    );
}
#[test]
fn transient_reader_gate_bounds_writer_wait_and_cancellation() {
    let fixture = Fixture::new();
    let mut gate = Process::start(&fixture, "gate", &fixture.paths, &[]);
    gate.observed("holding");
    let started = Instant::now();
    let error = WriterDemand::announce(&fixture.paths, Duration::from_millis(30), &|| false)
        .expect_err("gate excludes SH demand");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < WAIT);
    let checks = AtomicUsize::new(0);
    let error = WriterDemand::announce(&fixture.paths, WAIT, &|| checks.fetch_add(1, Ordering::Relaxed) >= 3)
        .expect_err("cancel blocked writer");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    for pre_cancelled in [false, true] {
        for multiple in [false, true] {
            let result = if multiple {
                crate::writer_lock::WriterLock::acquire_projects(&fixture.roots[0], &[], Duration::ZERO, &|| {
                    pre_cancelled
                })
                .map(|_guards| ())
            } else {
                crate::writer_lock::WriterLock::acquire_project(&fixture.paths[0], Duration::ZERO, &|| pre_cancelled)
                    .map(|_guard| ())
            };
            let error = result.expect_err("pre-EX demand cannot dispatch native work");
            assert_eq!(
                error.kind(),
                if pre_cancelled {
                    io::ErrorKind::Interrupted
                } else {
                    io::ErrorKind::TimedOut
                }
            );
            assert_eq!(
                crate::shared_writer::WriterFailure::from_io(&error).map(|failure| failure.disposition),
                Some(crate::shared_writer::WriterDisposition::NotStarted)
            );
        }
    }
    assert!(gate.child.try_wait().expect("gate still owned").is_none());
    gate.finish();
    drop(WriterDemand::announce(&fixture.paths, Duration::ZERO, &|| false).expect("gate closed"));
}
#[test]
fn failed_multi_domain_gate_and_writer_release_every_partial_guard() {
    let fixture = Fixture::new();
    let mut paths = fixture.paths.clone();
    paths.sort();
    let writer = WriterDemand::announce(&paths[1..], WAIT, &|| false).expect("writer on last domain");
    assert!(
        ReadAdmission::try_acquire(&paths)
            .expect("partial gate must unwind")
            .is_none()
    );
    drop(WriterDemand::announce(&paths[..1], Duration::ZERO, &|| false).expect("first partial gate closed"));
    drop(writer);
    let gate = ReadAdmission::try_acquire(&paths[1..])
        .expect("last domain")
        .expect("idle last domain");
    assert_eq!(
        WriterDemand::announce(&paths, Duration::from_millis(20), &|| false)
            .expect_err("last gate busy")
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(
        ReadAdmission::try_acquire(&paths[..1])
            .expect("partial demand released")
            .is_some()
    );
    drop(gate);
    assert!(ReadAdmission::try_acquire(&paths).expect("all idle").is_some());
}
#[test]
fn waiting_artifact_writer_announces_complete_union_before_any_ex() {
    let fixture = Fixture::new();
    fixture.project_graph();
    let shared = fixture
        .paths
        .iter()
        .map(|path| {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)
                .expect("artifact lock");
            fs4::FileExt::try_lock_shared(&file).expect("live reader SH");
            file
        })
        .collect::<Vec<_>>();
    let mut writer = Process::start(&fixture, "projects", &fixture.paths, &[]);
    writer.observed("waiting");
    let started = Instant::now();
    loop {
        if fixture.paths.iter().all(|path| {
            ReadAdmission::try_acquire(std::slice::from_ref(path))
                .expect("demand observation")
                .is_none()
        }) {
            break;
        }
        assert!(started.elapsed() < WAIT, "complete writer demand absent");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(writer.child.try_wait().expect("waiting actual writer").is_none());
    drop(shared);
    writer.observed("acquired");
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("returned guards retain demand")
            .is_none()
    );
    writer.finish();
    assert!(
        ReadAdmission::try_acquire(&fixture.paths)
            .expect("writer guard drop closes demand")
            .is_some()
    );
}
#[test]
fn pre_cancel_and_invalid_paths_never_create_demand_files() {
    let fixture = Fixture::new();
    let path = demand_path(&fixture.paths[0]).expect("stable demand path");
    assert_eq!(
        WriterDemand::announce(&fixture.paths, WAIT, &|| true)
            .expect_err("pre-cancel")
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(!path.exists());
    assert_eq!(
        ReadAdmission::try_acquire(&[]).expect_err("empty domain set").kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        ReadAdmission::try_acquire(&[fixture.directory.join("arbitrary.lock")])
            .expect_err("not project lock")
            .kind(),
        io::ErrorKind::InvalidData
    );
    let alias = fixture.directory.join("alias");
    std::os::unix::fs::symlink(&fixture.directory, &alias).expect("parent alias");
    let alias_path = alias.join(fixture.paths[0].file_name().expect("basename"));
    assert_eq!(
        ReadAdmission::try_acquire(&[alias_path])
            .expect_err("noncanonical parent")
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(!path.exists());
}
#[test]
fn aliases_nonregular_hardlinked_and_replaced_inodes_fail_closed() {
    let fixture = Fixture::new();
    let path = demand_path(&fixture.paths[0]).expect("stable demand path");
    let sentinel = fixture.directory.join("sentinel");
    fs::write(&sentinel, b"unchanged").expect("sentinel");
    std::os::unix::fs::symlink(&sentinel, &path).expect("demand alias");
    assert_eq!(
        ReadAdmission::try_acquire(std::slice::from_ref(&fixture.paths[0]))
            .expect_err("symlink rejected")
            .raw_os_error(),
        Some(libc::ELOOP)
    );
    assert_eq!(fs::read(&sentinel).expect("sentinel unchanged"), b"unchanged");
    fs::remove_file(&path).expect("fixture alias cleanup");
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).expect("malformed FIFO");
    let started = Instant::now();
    assert_eq!(
        ReadAdmission::try_acquire(std::slice::from_ref(&fixture.paths[0]))
            .expect_err("FIFO rejected")
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(started.elapsed() < WAIT);
    fs::remove_file(&path).expect("fixture FIFO cleanup");
    let demand =
        WriterDemand::announce(std::slice::from_ref(&fixture.paths[0]), WAIT, &|| false).expect("regular demand");
    fs::hard_link(&path, fixture.directory.join("hardlink")).expect("fixture hardlink");
    assert_eq!(
        ReadAdmission::try_acquire(std::slice::from_ref(&fixture.paths[0]))
            .expect_err("hardlink rejected")
            .kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_file(fixture.directory.join("hardlink")).expect("fixture hardlink cleanup");
    fs::remove_file(&path).expect("fixture only unlinks for inode substitution");
    fs::write(&path, b"replacement").expect("replacement");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private replacement");
    assert_eq!(
        validate(&demand.files[0], &path)
            .expect_err("opened/named inode must match")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn single_project_publication_announces_before_ex_without_entry_lock_demand() {
    let fixture = Fixture::new();
    let path = &fixture.paths[0];
    let shared = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .expect("artifact lock");
    fs4::FileExt::try_lock_shared(&shared).expect("current artifact reader");
    let mut writer = Process::start(&fixture, "project", std::slice::from_ref(path), &[]);
    writer.observed("waiting");
    let started = Instant::now();
    while ReadAdmission::try_acquire(std::slice::from_ref(path))
        .expect("demand observation")
        .is_some()
    {
        assert!(started.elapsed() < WAIT, "single project writer did not announce");
        thread::sleep(Duration::from_millis(5));
    }
    drop(shared);
    writer.observed("acquired");
    assert!(
        ReadAdmission::try_acquire(std::slice::from_ref(path))
            .expect("active demand")
            .is_none()
    );
    writer.finish();
    assert!(
        ReadAdmission::try_acquire(std::slice::from_ref(path))
            .expect("single project closed")
            .is_some()
    );
    let entry = fixture.directory.join("source-entry.lock");
    drop(crate::writer_lock::WriterLock::acquire(&entry, WAIT, &|| false).expect("entry lock has no project demand"));
}

#[test]
fn other_reader_gate_is_retryable_but_writer_demand_is_not() {
    let fixture = Fixture::new();
    let mut paths = fixture.paths.clone();
    paths.sort();
    let mut reader = Process::start(&fixture, "gate", &paths[1..], &[]);
    reader.observed("holding");
    let error = ReadAdmission::try_acquire(&paths).expect_err("another EX gate is transient");
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    drop(
        WriterDemand::announce(&paths[..1], Duration::ZERO, &|| false)
            .expect("every earlier partial EX gate closed before retryable return"),
    );
    reader.finish();
    let writer = WriterDemand::announce(&paths[1..], WAIT, &|| false).expect("SH writer demand");
    assert!(
        ReadAdmission::try_acquire(&paths)
            .expect("writer demand is a known refusal")
            .is_none()
    );
    drop(
        WriterDemand::announce(&paths[..1], Duration::ZERO, &|| false)
            .expect("earlier partial EX gate also closed on writer refusal"),
    );
    drop(writer);
    assert!(ReadAdmission::try_acquire(&paths).expect("all demand closed").is_some());
}
