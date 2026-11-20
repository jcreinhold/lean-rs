//! Real blocked-launcher children exercise the writer's production cleanup loop.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "isolated process fixtures assert lifecycle failures and durable handoff ordering"
)]

use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::mpsc::Sender;

const LIMIT: Duration = Duration::from_secs(5);
const LAUNCHER: &str = r"#!/usr/bin/env python3
import json, os, pathlib, signal, sys, time
root = pathlib.Path(__file__).parent
mode = (root / 'mode').read_text()
signal.signal(signal.SIGTERM, signal.SIG_IGN)
with (root / 'launches').open('a') as log:
    log.write(str(os.getpid()) + '\n')
if mode == 'blocked':
    time.sleep(15)
    sys.exit(119)
line = sys.stdin.buffer.readline()
if not line.endswith(b'\n'):
    sys.exit(120)
message = json.loads(line)
for name in ['primary.lock', 'loan.lock']:
    record = json.loads((root / name).read_text())
    if record['process']['pid'] != os.getpid():
        sys.exit(121)
with (root / 'body').open('a') as log:
    log.write('once\n')
print('actual stdout', flush=True)
print('actual stderr', file=sys.stderr, flush=True)
if mode == 'clean':
    sys.exit(0)
if mode == 'flood':
    stream = 2 if os.fork() == 0 else 1
    os.write(stream, b'\0' * (1024 * 1024))
    os._exit(0)
if mode in ['descendant', 'short-descendant']:
    child = os.fork()
    if child != 0:
        while not (root / 'heartbeat').exists():
            time.sleep(0.001)
        os._exit(0)
    # This descendant retains the audited PGID but deliberately closes every
    # inherited ownership descriptor. Group custody must still cover its writes.
    os.closerange(3, 4096)
    null = os.open('/dev/null', os.O_RDWR)
    for target in [0, 1, 2]:
        os.dup2(null, target)
    if null > 2:
        os.close(null)
    lifetime = 0.02 if mode == 'short-descendant' else 15
else:
    lifetime = 15
with (root / 'heartbeat').open('ab', buffering=0) as output:
    until = time.monotonic() + lifetime
    while time.monotonic() < until:
        output.write(b'x')
        time.sleep(0.005)
os._exit(0)
";

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    lock_path: PathBuf,
    loan_path: PathBuf,
    launcher: PathBuf,
    file: File,
    loans: Vec<File>,
    record: Owner,
}

impl Fixture {
    fn new(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let lock_path = root.join("primary.lock");
        let loan_path = root.join("loan.lock");
        let mut file = Self::locked(&lock_path);
        let mut loan = Self::locked(&loan_path);
        let record = Owner {
            version: PROTOCOL,
            pid: std::process::id(),
            start: birth(std::process::id()).unwrap(),
            session: sid(None).unwrap(),
        };
        let initial = Record {
            version: PROTOCOL,
            owner: record.clone(),
            process: None,
        };
        persist(&mut file, &initial).unwrap();
        persist(&mut loan, &initial).unwrap();
        fs::write(root.join("mode"), mode).unwrap();
        let launcher = root.join("launcher");
        fs::write(&launcher, LAUNCHER).unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _directory: directory,
            root,
            lock_path,
            loan_path,
            launcher,
            file,
            loans: vec![loan],
            record,
        }
    }

    fn locked(path: &Path) -> File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.try_lock().unwrap();
        file
    }

    fn run(
        &mut self,
        message: &Value,
        input: &Receiver<io::Result<Value>>,
        emit: impl FnMut(&Value) -> io::Result<()>,
    ) -> io::Result<bool> {
        execute_with_launcher(
            message,
            &self.root,
            &self.lock_path,
            &mut self.file,
            &self.loans,
            &self.record,
            input,
            &self.launcher,
            emit,
        )
    }

    fn readonly_loan(&mut self) {
        self.loans.clear();
        let loan = File::open(&self.loan_path).unwrap();
        lock_after_closed_aliases(&loan);
        self.loans.push(loan);
    }

    fn body_count(&self) -> usize {
        fs::read_to_string(self.root.join("body")).map_or(0, |body| body.lines().count())
    }

    fn launch_count(&self) -> usize {
        fs::read_to_string(self.root.join("launches")).map_or(0, |log| log.lines().count())
    }

    fn assert_closed(&mut self, frames: &[Value]) {
        let process = frames
            .iter()
            .find_map(|frame| frame.get("process").filter(|value| value.is_object()))
            .map(|value| serde_json::from_value::<ProcessIdentity>(value.clone()).unwrap());
        if let Some(process) = process {
            assert!(
                !lean_toolchain::process_group::group_present(process.process_group).unwrap(),
                "the actual private group remains live"
            );
        }
        let before = heartbeat(&self.root);
        thread::sleep(Duration::from_millis(40));
        assert_eq!(
            heartbeat(&self.root),
            before,
            "descendant writes continued after return"
        );
        for path in [&self.lock_path, &self.loan_path] {
            let record: Record = serde_json::from_reader(File::open(path).unwrap()).unwrap();
            assert!(
                record.process.is_none(),
                "terminal ownership was not cleared in every loan"
            );
            let probe = OpenOptions::new().read(true).write(true).open(path).unwrap();
            assert!(matches!(probe.try_lock(), Err(TryLockError::WouldBlock)));
        }
        // Only after execute has confirmed group plus descriptor closure do the
        // fixture's final real lock guards close and permit another writer.
        self.loans.clear();
        drop(std::mem::replace(&mut self.file, tempfile::tempfile().unwrap()));
        for path in [&self.lock_path, &self.loan_path] {
            let probe = OpenOptions::new().read(true).write(true).open(path).unwrap();
            lock_after_closed_aliases(&probe);
        }
    }
}

// Parallel unit tests may briefly fork with another fixture's CLOEXEC FD.
// Reacquisition waits for those exec-time aliases, while still failing a real
// inherited-descriptor leak at the fixture's bounded deadline.
fn lock_after_closed_aliases(file: &File) {
    wait_until(|| match file.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(error) => panic!("fixture lock failed: {error}"),
    });
}

fn message(timeout: f64) -> Value {
    json!({"action":"run", "command":["fixture-command"], "timeout":timeout})
}

fn heartbeat(root: &Path) -> u64 {
    fs::metadata(root.join("heartbeat")).map_or(0, |metadata| metadata.len())
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < LIMIT,
            "private writer fixture did not reach the expected state"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn assert_no_result(frames: &[Value]) {
    assert!(
        frames
            .iter()
            .any(|frame| frame["state"] == "still_running" && frame["result"] == "no_result")
    );
    let terminal = frames.last().unwrap();
    assert_eq!(
        terminal["state"], "stopped",
        "provisional cleanup must have an exact terminal receipt"
    );
    assert_eq!(terminal["result"], "no_result");
    assert_eq!(terminal["intervened"], true);
    assert_eq!(terminal["owned_work_stopped"], true);
    assert!(terminal["exit_code"].as_i64().is_some());
    assert!(terminal["process"]["pid"].as_u64().is_some());
}

#[test]
fn clean_command_returns_actual_output_and_clears_every_locked_record() {
    let mut fixture = Fixture::new("clean");
    let (_sender, input) = mpsc::channel();
    let mut frames = Vec::new();
    assert!(
        fixture
            .run(&message(3.0), &input, |frame| {
                frames.push(frame.clone());
                Ok(())
            })
            .unwrap()
    );
    let stopped = frames.iter().find(|frame| frame["state"] == "stopped").unwrap();
    assert_eq!(stopped["exit_code"], 0);
    assert_eq!(stopped["stdout"], "actual stdout\n");
    assert_eq!(stopped["stderr"], "actual stderr\n");
    assert_eq!(stopped["owned_work_stopped"], true);
    assert_eq!(stopped["intervened"], false);
    assert_eq!(stopped["result"], "result");
    assert_eq!(fixture.body_count(), 1);
    assert_eq!(fixture.launch_count(), 1);
    fixture.assert_closed(&frames);
}

#[test]
fn natural_root_zero_with_fd_closing_writer_is_cleaned_up_without_a_result() {
    let mut fixture = Fixture::new("descendant");
    let root = fixture.root.clone();
    let (_sender, input) = mpsc::channel();
    let mut frames = Vec::new();
    assert!(
        !fixture
            .run(&message(3.0), &input, |frame| {
                frames.push(frame.clone());
                if frame["state"] == "started" {
                    wait_until(|| heartbeat(&root) > 0);
                }
                Ok(())
            })
            .unwrap()
    );
    assert_no_result(&frames);
    assert_eq!(
        frames.last().unwrap()["exit_code"],
        0,
        "actual root zero never upgrades forced descendant cleanup"
    );
    assert!(frames.iter().any(|frame| {
        frame["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("surviving owned work"))
    }));
    assert_eq!(fixture.body_count(), 1);
    fixture.assert_closed(&frames);
}

#[test]
fn short_lived_descendant_can_complete_inside_the_natural_exit_grace() {
    let mut fixture = Fixture::new("short-descendant");
    let (_sender, input) = mpsc::channel();
    let mut frames = Vec::new();
    assert!(
        fixture
            .run(&message(3.0), &input, |frame| {
                frames.push(frame.clone());
                Ok(())
            })
            .unwrap()
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame["state"] == "stopped" && frame["exit_code"] == 0)
    );
    assert!(!frames.iter().any(|frame| frame["state"] == "still_running"));
    fixture.assert_closed(&frames);
}

#[test]
fn hanging_command_deadline_waits_for_cleanup_and_returns_no_result() {
    let mut fixture = Fixture::new("hang");
    let root = fixture.root.clone();
    let (_sender, input) = mpsc::channel();
    let mut frames = Vec::new();
    let started = Instant::now();
    assert!(
        !fixture
            .run(&message(0.3), &input, |frame| {
                frames.push(frame.clone());
                if frame["state"] == "started" {
                    wait_until(|| heartbeat(&root) > 0);
                }
                Ok(())
            })
            .unwrap()
    );
    assert!(started.elapsed() < LIMIT);
    assert_no_result(&frames);
    assert_eq!(fixture.body_count(), 1);
    fixture.assert_closed(&frames);
}

fn cancel_after_started(disconnect: bool) {
    let mut fixture = Fixture::new("hang");
    let root = fixture.root.clone();
    let (sender, input) = mpsc::channel();
    let mut sender: Option<Sender<io::Result<Value>>> = Some(sender);
    let mut frames = Vec::new();
    assert!(
        !fixture
            .run(&message(3.0), &input, |frame| {
                frames.push(frame.clone());
                if frame["state"] == "started" {
                    wait_until(|| heartbeat(&root) > 0);
                    if disconnect {
                        drop(sender.take());
                    } else {
                        sender.as_ref().unwrap().send(Ok(json!({"action":"cancel"}))).unwrap();
                    }
                }
                Ok(())
            })
            .unwrap()
    );
    assert_no_result(&frames);
    assert_eq!(fixture.body_count(), 1);
    assert_eq!(
        fixture.launch_count(),
        1,
        "a lost/cancelled result must not replay the body"
    );
    fixture.assert_closed(&frames);
}

#[test]
fn cancellation_after_dispatch_stops_once_without_replay() {
    cancel_after_started(false);
}

#[test]
fn client_eof_after_dispatch_stops_once_without_replay() {
    cancel_after_started(true);
}

#[test]
fn failed_started_delivery_cleans_up_before_returning() {
    let mut fixture = Fixture::new("hang");
    let root = fixture.root.clone();
    let (_sender, input) = mpsc::channel();
    let mut frames = Vec::new();
    assert!(
        !fixture
            .run(&message(3.0), &input, |frame| {
                frames.push(frame.clone());
                if frame["state"] == "started" {
                    wait_until(|| heartbeat(&root) > 0);
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                Ok(())
            })
            .unwrap()
    );
    assert_no_result(&frames);
    assert_eq!(fixture.body_count(), 1);
    fixture.assert_closed(&frames);
}

#[test]
fn blocked_large_go_write_obeys_deadline_without_starting_payload() {
    let mut fixture = Fixture::new("blocked");
    let (_sender, input) = mpsc::channel();
    let mut launch = message(0.2);
    launch["padding"] = json!("x".repeat(256 * 1024));
    let mut frames = Vec::new();
    let started = Instant::now();
    assert!(
        !fixture
            .run(&launch, &input, |frame| {
                frames.push(frame.clone());
                Ok(())
            })
            .unwrap()
    );
    assert!(
        started.elapsed() < LIMIT,
        "blocked GO write ignored the request deadline"
    );
    assert_no_result(&frames);
    assert!(!frames.iter().any(|frame| frame["state"] == "started"));
    assert_eq!(fixture.body_count(), 0);
    fixture.assert_closed(&frames);
}

#[test]
fn post_spawn_loan_commit_failure_never_releases_the_blocked_payload() {
    let mut fixture = Fixture::new("clean");
    fixture.readonly_loan();
    let (_sender, input) = mpsc::channel();
    let mut frames = Vec::new();
    let started = Instant::now();
    assert!(
        fixture
            .run(&message(3.0), &input, |frame| {
                frames.push(frame.clone());
                Ok(())
            })
            .is_err()
    );
    assert!(started.elapsed() < LIMIT);
    assert_no_result(&frames);
    assert!(!frames.iter().any(|frame| frame["state"] == "started"));
    assert!(frames.iter().any(|frame| frame["process"]["pid"].as_u64().is_some()));
    assert_eq!(fixture.body_count(), 0);
    fixture.assert_closed(&frames);
}

#[test]
fn output_overflow_stops_scope_with_bounded_terminal_no_result() {
    let mut fixture = Fixture::new("flood");
    let (_sender, input) = mpsc::channel();
    let mut frames = Vec::new();
    assert!(
        !fixture
            .run(&message(3.0), &input, |frame| {
                frames.push(frame.clone());
                Ok(())
            })
            .unwrap()
    );
    assert_no_result(&frames);
    let terminal = frames.last().unwrap();
    assert!(terminal["stdout"].as_str().unwrap().len() <= CAPTURE_BYTES);
    assert!(terminal["stderr"].as_str().unwrap().len() <= CAPTURE_BYTES);
    assert!(serde_json::to_vec(terminal).unwrap().len() < FRAME_BYTES as usize);
    assert!(terminal["reason"].as_str().unwrap().contains("output limit"));
    fixture.assert_closed(&frames);
}
