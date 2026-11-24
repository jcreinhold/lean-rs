#![allow(unsafe_code, clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

#[path = "support/ordinary_typed_selection.rs"]
mod ordinary_typed_selection;

use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::harness::{WorkerDataRow, WorkerHarnessError, WorkerProcess};
use lean_rs_worker_protocol::protocol::{MAX_FRAME_BYTES, Message, PROTOCOL_VERSION, Request, read_frame, write_frame};
use serde_json::json;

fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    workspace_root().join("fixtures").join("lean")
}

fn scalar_fixture() -> (PathBuf, PathBuf) {
    ordinary_typed_selection::ordinary_scalar_selection().map_or_else(
        || (fixture_root(), worker_binary()),
        |selected| (selected.fixture.clone(), selected.worker.clone()),
    )
}

struct RawWorker {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

fn spawn_raw_worker() -> RawWorker {
    let mut child = Command::new(worker_binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LEAN_ABORT_ON_PANIC", "1")
        .env("LEAN_BACKTRACE", "0")
        .env("RUST_BACKTRACE", "0")
        .spawn()
        .expect("worker starts");
    let stdin = child.stdin.take().map(BufWriter::new).expect("worker stdin is piped");
    let stdout = child.stdout.take().map(BufReader::new).expect("worker stdout is piped");
    RawWorker { child, stdin, stdout }
}

fn expect_handshake_and_configure(worker: &mut RawWorker) {
    let frame = read_frame(&mut worker.stdout, MAX_FRAME_BYTES).expect("worker sends handshake");
    match frame.message {
        Message::Handshake { protocol_version, .. } => assert_eq!(protocol_version, PROTOCOL_VERSION),
        other => panic!("expected handshake, got {other:?}"),
    }
    write_frame(
        &mut worker.stdin,
        Message::ConfigureFrameLimit {
            max_frame_bytes: MAX_FRAME_BYTES,
        },
        MAX_FRAME_BYTES,
    )
    .expect("parent sends frame limit");
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("child status can be queried") {
            return Some(status);
        }
        if started.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn health_check_succeeds() {
    let mut worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    worker.health().expect("health check succeeds");
    let status = worker.terminate().expect("worker terminates");
    assert!(status.success(), "worker should exit cleanly");
}

#[cfg(unix)]
#[test]
#[ignore = "requires the built SDK helper selected by LEAN_TOOLCHAIN_READER_TEST_HELPER"]
fn standalone_worker_and_writer_use_the_typed_owner_directory() -> Result<(), Box<dyn std::error::Error>> {
    use lean_rs_worker_parent::{LeanWorker, LeanWorkerConfig};
    use lean_toolchain::native_guardian::{OWNERS_DIRECTORY_ENV, bind_registry_directory};
    use lean_toolchain::shared_writer::SharedWriter;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    let helper =
        PathBuf::from(std::env::var_os("LEAN_TOOLCHAIN_READER_TEST_HELPER").expect("select the built lean-rs-writer"));
    let scratch = tempfile::Builder::new()
        .prefix("nc.")
        .tempdir_in(workspace_root().join("target"))?;
    let root = scratch.path().canonicalize()?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let registry = root.join("owners");
    let project = root.join("project");
    fs::create_dir(&project)?;
    bind_registry_directory(&registry)?;
    assert!(!registry.exists(), "binding creates no owner record");

    let config = LeanWorkerConfig::new(worker_binary())
        .lifetime_directory(&registry)
        .lifetime_socket_directory(&root)
        .native_guardian(&helper);
    let mut worker = LeanWorker::spawn(&config)?;
    worker.health()?;
    let records = fs::read_dir(&registry)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("owner.json")))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    let [record] = records.as_slice() else {
        panic!("expected one healthy worker's owner record, got {records:?}");
    };
    let owner: serde_json::Value = serde_json::from_slice(&fs::read(record)?)?;
    assert_eq!(owner.get("phase").and_then(serde_json::Value::as_str), Some("running"));

    let timeout = Duration::from_secs(5);
    let mut writer = SharedWriter::open(&helper, &project, timeout)?;
    let mut command = Command::new("/usr/bin/printenv");
    command.arg(OWNERS_DIRECTORY_ENV).current_dir(&project);
    let output = writer.run(&command, timeout)?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        registry.to_str().expect("fixture path")
    );
    writer.close(timeout)?;
    worker.try_shutdown()?;
    Ok(())
}

#[test]
fn conflicting_writer_executables_refuse_worker_startup() {
    use lean_rs_worker_parent::{LeanWorker, LeanWorkerConfig, LeanWorkerError};
    use lean_toolchain::shared_writer::SHARED_WRITER_EXECUTABLE_ENV;

    let legacy = "LEAN_HOST_MCP_WRITER_EXECUTABLE";
    let output = Command::new(worker_binary())
        .env(SHARED_WRITER_EXECUTABLE_ENV, "/current")
        .env(legacy, "/former")
        .output()
        .expect("run worker startup");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "worker began its handshake");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("conflicting")
            && diagnostic.contains(SHARED_WRITER_EXECUTABLE_ENV)
            && diagnostic.contains(legacy),
        "{diagnostic}"
    );

    let config = LeanWorkerConfig::new(worker_binary())
        .env(SHARED_WRITER_EXECUTABLE_ENV, "/current")
        .env(legacy, "/former");
    match LeanWorker::spawn(&config) {
        Err(LeanWorkerError::Setup { message }) => {
            assert!(message.contains("conflicting"), "{message}");
            assert!(message.contains(SHARED_WRITER_EXECUTABLE_ENV), "{message}");
            assert!(message.contains(legacy), "{message}");
        }
        Err(error) => panic!("expected startup configuration error, got {error}"),
        Ok(_) => panic!("conflicting values started a worker"),
    }
}

#[test]
fn fixture_capability_loads_and_exported_call_succeeds() {
    let (fixture, binary) = scalar_fixture();
    let mut worker = WorkerProcess::spawn(&binary).expect("worker starts");
    worker
        .load_fixture_capability(&fixture)
        .expect("fixture capability loads in worker");
    let value = worker
        .call_fixture_mul(&fixture, 6, 7)
        .expect("worker calls fixture export");
    assert_eq!(value, 42);
    let status = worker.terminate().expect("worker terminates");
    assert!(status.success(), "worker should exit cleanly");
}

#[test]
fn terminate_request_exits_cleanly() {
    let worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let status = worker.terminate().expect("worker terminates");
    assert!(status.success(), "worker should exit cleanly");
}

#[test]
fn stdin_eof_exits_without_waiting_for_another_request() {
    let mut worker = spawn_raw_worker();
    expect_handshake_and_configure(&mut worker);

    drop(worker.stdin);

    let status =
        wait_for_exit(&mut worker.child, Duration::from_secs(5)).expect("worker exits promptly after stdin EOF");
    assert!(
        !status.success(),
        "stdin EOF is reported as a protocol/channel loss, not graceful termination"
    );
}

#[test]
fn broken_stdout_write_exits_without_waiting_for_another_request() {
    let mut worker = spawn_raw_worker();
    expect_handshake_and_configure(&mut worker);

    drop(worker.stdout);
    write_frame(&mut worker.stdin, Message::Request(Request::Health), MAX_FRAME_BYTES)
        .expect("parent sends health request before closing stdin");
    drop(worker.stdin);

    let status = wait_for_exit(&mut worker.child, Duration::from_secs(5))
        .expect("worker exits promptly after stdout write failure");
    assert!(
        !status.success(),
        "broken stdout is reported as a protocol/channel loss, not graceful termination"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_parent_death_signal_exits_when_stdio_stays_open() {
    use std::fs;
    use std::os::unix::fs::OpenOptionsExt as _;

    let temp_root = std::env::temp_dir().join(format!("lean-rs-worker-parent-death-{}", std::process::id()));
    fs::create_dir_all(&temp_root).expect("temp dir is created");
    let fifo = temp_root.join("stdin.fifo");
    let stdout = temp_root.join("stdout.log");
    let stderr = temp_root.join("stderr.log");
    let pidfile = temp_root.join("worker.pid");

    let fifo_c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).expect("fifo path has no nul");
    // SAFETY: `mkfifo` receives a valid nul-terminated path and creates one
    // filesystem node used only by this test.
    let rc = unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

    let _held_fifo = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&fifo)
        .expect("test holds FIFO open so child stdin does not reach EOF");

    let helper = Command::new("sh")
        .arg("-c")
        .arg("\"$1\" < \"$2\" > \"$3\" 2> \"$4\" & echo $! > \"$5\"")
        .arg("sh")
        .arg(worker_binary())
        .arg(&fifo)
        .arg(&stdout)
        .arg(&stderr)
        .arg(&pidfile)
        .status()
        .expect("helper shell starts worker");
    assert!(helper.success(), "helper shell exits cleanly");

    let mut pid_text = String::new();
    for _ in 0..100 {
        pid_text = fs::read_to_string(&pidfile).unwrap_or_default();
        if !pid_text.trim().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid: libc::pid_t = pid_text.trim().parse().expect("worker pid is written");

    let exited = wait_for_pid_to_disappear(pid, Duration::from_secs(5));
    if !exited {
        // SAFETY: best-effort cleanup for a failed test; `pid` came from the
        // helper shell that spawned the worker child.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    drop(fs::remove_dir_all(&temp_root));

    assert!(
        exited,
        "worker pid {pid} stayed alive after its parent shell exited while stdin remained open"
    );
}

#[cfg(target_os = "linux")]
fn wait_for_pid_to_disappear(pid: libc::pid_t, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"));
        match status {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
            Ok(status)
                if status
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains('Z')) =>
            {
                return true;
            }
            Ok(_) | Err(_) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            Ok(_) | Err(_) => return false,
        }
    }
}

#[test]
fn lean_internal_panic_kills_only_child() {
    let (fixture, binary) = scalar_fixture();
    let worker = WorkerProcess::spawn(&binary).expect("worker starts");
    let fatal = worker
        .trigger_lean_panic(&fixture)
        .expect("parent observes child fatal exit");
    assert!(
        !fatal.status.is_empty(),
        "fatal exit should include rendered child status"
    );
    if !fatal.stderr.is_empty() {
        assert!(
            fatal.stderr.contains("lean_rs_fixture: deliberate Lean panic"),
            "child stderr should contain Lean panic message, got:\n{}",
            fatal.stderr,
        );
    }
}

#[test]
fn missing_fixture_path_reports_manifest_build_error_without_crashing_child() {
    let missing = workspace_root()
        .join("fixtures")
        .join("definitely-missing-worker-fixture");
    let mut worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let err = worker
        .load_fixture_capability(&missing)
        .expect_err("missing fixture path should be a typed harness build error");
    match err {
        WorkerHarnessError::CapabilityBuild(message) => {
            assert!(
                message.contains("definitely-missing-worker-fixture"),
                "message should identify missing fixture path, got {message}",
            );
        }
        other => panic!("expected CapabilityBuild, got {other:?}"),
    }
    let status = worker.terminate().expect("worker terminates after typed error");
    assert!(status.success(), "worker should stay alive after typed load error");
}

#[test]
fn data_rows_are_delivered_in_pipe_order_with_per_stream_sequences() {
    let mut worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let rows = worker
        .emit_test_rows(vec![
            "rows".to_owned(),
            "warnings".to_owned(),
            "rows".to_owned(),
            "warnings".to_owned(),
        ])
        .expect("worker emits data rows");

    assert_eq!(
        rows,
        vec![
            WorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 0,
                payload: json!({ "stream": "rows", "index": 0 }),
            },
            WorkerDataRow {
                stream: "warnings".to_owned(),
                sequence: 0,
                payload: json!({ "stream": "warnings", "index": 1 }),
            },
            WorkerDataRow {
                stream: "rows".to_owned(),
                sequence: 1,
                payload: json!({ "stream": "rows", "index": 2 }),
            },
            WorkerDataRow {
                stream: "warnings".to_owned(),
                sequence: 1,
                payload: json!({ "stream": "warnings", "index": 3 }),
            },
        ],
    );

    let status = worker.terminate().expect("worker terminates after row stream");
    assert!(status.success(), "worker should exit cleanly");
}

#[test]
fn eof_before_rows_complete_is_reported_as_protocol_failure() {
    let worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let err = worker
        .emit_rows_then_exit()
        .expect_err("child exit before terminal response should fail");
    match err {
        WorkerHarnessError::Protocol(message) => {
            assert!(
                message.contains("before terminal row response"),
                "failure should name missing terminal response, got {message}",
            );
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }
}

#[test]
fn fatal_exit_after_partial_rows_is_reported_as_worker_failure() {
    let worker = WorkerProcess::spawn(&worker_binary()).expect("worker starts");
    let started = Instant::now();
    let err = worker
        .emit_rows_then_panic()
        .expect_err("fatal exit before terminal response should fail");
    let elapsed = started.elapsed();
    match err {
        WorkerHarnessError::FatalExit(exit) => {
            assert!(
                !exit.status.is_empty(),
                "fatal exit should include rendered child status"
            );
        }
        other => panic!("expected FatalExit, got {other:?}"),
    }
    // Regression bound for `child::install_immediate_abort_exit`. Without
    // that fix, `SIGABRT` from a Lean panic on Linux runners with `apport`
    // (or any pipe-based `core_pattern`) keeps the dying child's file
    // descriptors open while the kernel pipes the core image to the
    // handler. The parent's EOF detection then takes 20–110 seconds, long
    // enough that supervisor request timeouts fire before the panic is
    // recognised as `ChildPanicOrAbort`. With the `SIGABRT` handler
    // installed, the child calls `_exit(134)` directly, the pipes close
    // immediately, and the round trip is bounded by ordinary IPC and
    // process-wait latency.
    assert!(
        elapsed < Duration::from_secs(10),
        "panic-to-fatal-exit detection took {elapsed:?}; immediate-abort-exit in the worker child may have regressed",
    );
}
