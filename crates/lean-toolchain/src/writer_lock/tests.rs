//! Real descriptor and owned-process tests for close-only writer-lock custody.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::{BufRead as _, Seek as _, SeekFrom, Write as _};
#[cfg(unix)]
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

const CHILD_MODE: &str = "LEAN_TOOLCHAIN_WRITER_LOCK_TEST_CHILD";
const CHILD_PATH: &str = "LEAN_TOOLCHAIN_WRITER_LOCK_TEST_PATH";
const CHILD_TEST: &str = "writer_lock::tests::lock_test_child";
const READY: &str = "writer-lock-test:";
const WAIT: Duration = Duration::from_secs(5);

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock")
            .as_nanos();
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lean-toolchain-writer-lock-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create private lock fixture");
        Self(path.canonicalize().expect("canonical fixture directory"))
    }

    fn lock_path(&self) -> PathBuf {
        self.0.join("writer.lock")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild {
    child: Child,
    observations: Receiver<String>,
}

impl OwnedChild {
    fn spawn(mode: &str, path: &Path, loan: Option<&File>) -> Self {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--quiet", "--test-threads=1"])
            .env(CHILD_MODE, mode)
            .env(CHILD_PATH, path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        let mut child = if let Some(file) = loan {
            let metadata = file.metadata().expect("loan identity");
            command.env(
                "LEAN_RS_WRITER_LOCK_LOANS",
                serde_json::json!([{
                    "fd":file.as_raw_fd(), "path":path, "device":metadata.dev(), "inode":metadata.ino()
                }])
                .to_string(),
            );
            crate::process_group::spawn_inheriting(command, &[file.as_fd()]).expect("spawn inherited loan child")
        } else {
            command.spawn().expect("spawn owned fixture child")
        };
        #[cfg(not(unix))]
        let mut child = command.spawn().expect("spawn owned fixture child");
        let stderr = child.stderr.take().expect("child observation pipe");
        let (send, observations) = mpsc::channel();
        thread::spawn(move || {
            for line in io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, observations }
    }

    fn observe(&self, expected: &str) {
        let started = Instant::now();
        let mut diagnostics = Vec::new();
        loop {
            let remaining = WAIT.saturating_sub(started.elapsed());
            let line = self
                .observations
                .recv_timeout(remaining)
                .unwrap_or_else(|error| panic!("child did not report {expected}: {error}; stderr={diagnostics:?}"));
            if let Some(observation) = line.strip_prefix(READY) {
                assert_eq!(observation, expected);
                return;
            }
            diagnostics.push(line);
        }
    }

    fn tell(&mut self, instruction: &str) {
        let stdin = self.child.stdin.as_mut().expect("child control pipe");
        writeln!(stdin, "{instruction}").expect("send child instruction");
        stdin.flush().expect("flush child instruction");
    }

    fn wait(&mut self) -> ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for owned child") {
                assert!(status.success(), "fixture child exited with {status}");
                return status;
            }
            assert!(started.elapsed() < WAIT, "owned child did not exit after release");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Only this test's actual Child handle may receive cleanup signals.
        if self.child.try_wait().ok().flatten().is_none() {
            let _kill = self.child.kill();
        }
        let _reap = self.child.wait();
    }
}

fn acquired(path: &Path) -> WriterLock {
    WriterLock::acquire(path, WAIT, &|| false).expect("acquire fixture lock")
}

fn lock_error(result: io::Result<WriterLock>) -> io::Error {
    match result {
        Ok(_guard) => panic!("contending acquisition unexpectedly succeeded"),
        Err(error) => error,
    }
}

fn report(observation: &str) {
    writeln!(io::stderr(), "{READY}{observation}").expect("report child state");
}

fn instruction() -> String {
    let mut line = String::new();
    assert!(io::stdin().read_line(&mut line).expect("read child instruction") > 0);
    line.trim_end().to_owned()
}

/// Re-enter only this test in a real process; an ordinary test run does no work.
#[test]
fn lock_test_child() {
    let Some(mode) = std::env::var_os(CHILD_MODE) else {
        return;
    };
    let path = PathBuf::from(std::env::var_os(CHILD_PATH).expect("private fixture path"));
    match mode.to_str().expect("child mode") {
        "hold" => {
            let guard = acquired(&path);
            report("holding");
            assert_eq!(instruction(), "release");
            drop(guard);
        }
        "contend" => {
            let error = lock_error(WriterLock::acquire(&path, Duration::ZERO, &|| false));
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            report("blocked");
        }
        #[cfg(unix)]
        "inherited" => {
            let locks = crate::shared_writer::inherited_writer_locks()
                .expect("validate inherited loans")
                .expect("loan mode");
            let mut loan = locks.into_iter().next().expect("one inherited loan").1;
            fs4::FileExt::try_lock(&loan).expect("same-description loan already owns flock");
            report("inherited");
            assert_eq!(instruction(), "probe");
            assert_eq!(loan.stream_position().expect("read shared offset"), 13);
            loan.seek(SeekFrom::Start(37)).expect("move shared offset");
            report("shared-offset");
            assert_eq!(instruction(), "release");
            drop(loan);
            // The original inherited descriptor closes on actual process exit.
            // Parent assertions use an actual wait, not this message.
        }
        other => panic!("unexpected child mode {other}"),
    }
}

#[test]
fn closing_controller_preserves_a_duplicated_open_description_lock() {
    let workspace = Workspace::new();
    let path = workspace.lock_path();
    let guard = acquired(&path);
    assert_eq!(guard.path(), path);
    let loan = guard.file().try_clone().expect("duplicate the same open description");
    drop(guard);
    assert_eq!(
        lock_error(WriterLock::acquire(&path, Duration::ZERO, &|| false)).kind(),
        io::ErrorKind::TimedOut
    );
    drop(loan);
    drop(acquired(&path));
}

#[cfg(unix)]
#[test]
fn inherited_same_description_loan_retains_exclusion_until_owned_child_exit() {
    let workspace = Workspace::new();
    let path = workspace.lock_path();
    let guard = acquired(&path);
    let mut child = OwnedChild::spawn("inherited", &path, Some(guard.file()));
    child.observe("inherited");
    let mut original = guard.file();
    original.seek(SeekFrom::Start(13)).expect("set shared offset");
    child.tell("probe");
    child.observe("shared-offset");
    assert_eq!(original.stream_position().expect("observe child's shared offset"), 37);
    drop(guard);

    let mut contender = OwnedChild::spawn("contend", &path, None);
    contender.observe("blocked");
    contender.wait();
    assert!(child.child.try_wait().expect("original child remains alive").is_none());
    child.tell("release");
    child.wait();
    drop(acquired(&path));
}

#[test]
fn cancellation_before_open_creates_no_lock_file() {
    let workspace = Workspace::new();
    let path = workspace.lock_path();
    let error = lock_error(WriterLock::acquire(&path, WAIT, &|| true));
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(!path.exists());
}

#[test]
fn cancellation_after_acquisition_releases_the_new_lock() {
    let workspace = Workspace::new();
    let path = workspace.lock_path();
    let checks = AtomicUsize::new(0);
    let error = lock_error(WriterLock::acquire(&path, WAIT, &|| {
        checks.fetch_add(1, Ordering::SeqCst) >= 2
    }));
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    drop(acquired(&path));
}

#[test]
fn owned_child_contention_timeout_and_cancellation_preserve_its_lock() {
    let workspace = Workspace::new();
    let path = workspace.lock_path();
    let mut holder = OwnedChild::spawn("hold", &path, None);
    holder.observe("holding");
    let started = Instant::now();
    let error = lock_error(WriterLock::acquire(&path, Duration::from_millis(30), &|| false));
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < WAIT, "acquisition exceeded its bounded wait");

    let checks = AtomicUsize::new(0);
    let error = lock_error(WriterLock::acquire(&path, WAIT, &|| {
        checks.fetch_add(1, Ordering::SeqCst) >= 2
    }));
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(holder.child.try_wait().expect("holder stays alive").is_none());
    let mut contender = OwnedChild::spawn("contend", &path, None);
    contender.observe("blocked");
    contender.wait();
    holder.tell("release");
    holder.wait();
    drop(acquired(&path));
}

#[cfg(unix)]
#[path = "admission_tests.rs"]
mod admission_tests;

#[cfg(unix)]
pub(super) fn admission_wait(path: &Path, reason: &str) {
    admission_tests::admission_wait(path, reason);
}
