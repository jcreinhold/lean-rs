#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

#[path = "support/ordinary_typed_selection.rs"]
mod ordinary_typed_selection;
#[path = "support/ordinary_typed_worker.rs"]
mod ordinary_typed_worker;

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerProcessTracker, LeanWorkerRestartPolicy,
    LeanWorkerRestartReason, LeanWorkerSessionConfig, LeanWorkerStatus,
};

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

fn ensure_fixture_built() {
    let fixture = fixture_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsFixture").expect("fixture Lake target builds");
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
}

fn scalar_fixture() -> (PathBuf, LeanWorkerConfig) {
    ordinary_typed_selection::ordinary_scalar_selection().map_or_else(
        || (fixture_root(), worker_config()),
        |selected| {
            (
                selected.fixture.clone(),
                LeanWorkerConfig::new(&selected.worker).checked_native_loader_environment(),
            )
        },
    )
}

#[test]
fn public_health_check_succeeds() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker.health().expect("health check succeeds");
    assert_eq!(worker.status().expect("status succeeds"), LeanWorkerStatus::Running);
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly");
}

#[test]
fn public_fixture_export_call_succeeds() {
    let (fixture, config) = scalar_fixture();
    let mut worker = LeanWorker::spawn(&config).expect("worker starts");
    worker
        .load_fixture_capability(&fixture)
        .expect("fixture capability loads in worker");
    let value = worker
        .call_fixture_mul(&fixture, 9, 8)
        .expect("worker calls fixture export");
    assert_eq!(value, 72);
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly");
}

#[test]
fn explicit_restart_replaces_live_child() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker.health().expect("health check succeeds before restart");
    worker.restart().expect("worker restarts");
    worker.health().expect("health check succeeds after restart");
    let stats = worker.stats();
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.explicit_cycles, 1);
    assert_eq!(stats.last_restart_reason, Some(LeanWorkerRestartReason::Explicit));
    assert_eq!(stats.replacement_attempts, 1);
    assert_eq!(stats.replacement_successes, 1);
    assert_eq!(stats.replacement_failures, 0);
    let timing = stats
        .last_replacement_timing
        .as_ref()
        .expect("explicit restart records replacement timing");
    assert_eq!(timing.replacement_reason, "explicit");
    assert_eq!(timing.replacement_budget_status, "synchronous-no-overlap");
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly after restart");
}

#[test]
fn explicit_cycle_replaces_child_and_records_reason() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker.health().expect("health check succeeds before cycle");
    worker.cycle().expect("worker cycles");
    worker.health().expect("health check succeeds after cycle");
    let stats = worker.stats();
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.explicit_cycles, 1);
    assert_eq!(stats.exits, 1);
    assert_eq!(stats.last_restart_reason, Some(LeanWorkerRestartReason::Explicit));
    assert_eq!(stats.replacement_attempts, 1);
    assert_eq!(stats.replacement_successes, 1);
    assert_eq!(
        stats
            .last_replacement_timing
            .as_ref()
            .map(|timing| timing.replacement_reason.as_str()),
        Some("explicit")
    );
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly after cycle");
}

#[test]
fn max_request_policy_restarts_before_next_request() {
    let config = worker_config().restart_policy(LeanWorkerRestartPolicy::default().max_requests(1));
    let mut worker = LeanWorker::spawn(&config).expect("worker starts");
    worker.health().expect("first request succeeds");
    assert_eq!(worker.stats().restarts, 0);

    worker.health().expect("second request succeeds after policy restart");
    let stats = worker.stats();
    assert_eq!(stats.requests, 2);
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.max_request_restarts, 1);
    assert_eq!(
        stats.last_restart_reason,
        Some(LeanWorkerRestartReason::MaxRequests { limit: 1 })
    );
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly after policy restart");
}

#[test]
fn max_import_policy_restarts_before_next_import() {
    let base = ordinary_typed_worker::worker_config();
    if base.is_none() {
        ensure_fixture_built();
    }
    let tracker = LeanWorkerProcessTracker::new();
    let config = base
        .unwrap_or_else(worker_config)
        .restart_policy(LeanWorkerRestartPolicy::default().max_imports(1))
        .process_tracker(tracker.clone());
    let mut worker = LeanWorker::spawn(&config).expect("worker starts");
    let first = worker.__child_pid_for_test().expect("first worker PID");
    let import = |worker: &mut LeanWorker, module| {
        if let Some(selected) = ordinary_typed_selection::ordinary_typed_selection() {
            let request = LeanWorkerSessionConfig::new(&selected.fixture, "lean_rs_fixture", "LeanRsFixture", [module]);
            let _session = worker.open_session(&request, None, None).expect("session opens");
        } else {
            worker
                .load_fixture_capability(fixture_root())
                .expect("fixture capability loads");
        }
    };

    import(&mut worker, "LeanRsFixture.Handles");
    assert_eq!(worker.__child_pid_for_test(), Some(first));
    assert_eq!(worker.stats().restarts, 0);

    // A repeat of the same key can reuse its session without a new import.
    import(&mut worker, "LeanRsHostShims.Elaboration");
    assert_ne!(worker.__child_pid_for_test().expect("replacement PID"), first);
    let stats = worker.stats();
    assert_eq!(stats.imports, 2);
    assert_eq!(stats.import_like_admission_attempts, 2);
    assert_eq!(stats.import_like_admitted, 2);
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.max_import_restarts, 1);
    assert_eq!(
        stats.last_restart_reason,
        Some(LeanWorkerRestartReason::MaxImports { limit: 1 })
    );
    assert_eq!(stats.replacement_attempts, 1);
    assert_eq!(stats.replacement_successes, 1);
    let timing = stats
        .last_replacement_timing
        .as_ref()
        .expect("max-import restart records replacement timing");
    assert_eq!(timing.replacement_reason, "max_imports");
    assert_eq!(timing.replacement_budget_status, "synchronous-no-overlap");
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly after import policy restart");
    assert_eq!(tracker.active_processes(), 0, "both worker generations must be reaped");
    assert_eq!(
        LeanWorkerRestartPolicy::memory_bounded(1, 1_073_741_824),
        LeanWorkerRestartPolicy::default()
            .max_imports(1)
            .max_rss_kib(1_073_741_824)
    );
}

#[test]
fn idle_restart_policy_restarts_before_next_request() {
    let idle_limit = Duration::from_millis(100);
    let config = worker_config().restart_policy(LeanWorkerRestartPolicy::default().idle_restart_after(idle_limit));
    let mut worker = LeanWorker::spawn(&config).expect("worker starts");
    worker.health().expect("first request succeeds");
    thread::sleep(Duration::from_millis(150));
    worker.health().expect("second request succeeds after idle restart");
    let stats = worker.stats();
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.idle_restarts, 1);
    match stats.last_restart_reason {
        Some(LeanWorkerRestartReason::Idle { limit, .. }) => assert_eq!(limit, idle_limit),
        other => panic!("expected idle restart reason, got {other:?}"),
    }
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly after idle restart");
}

#[test]
fn rss_policy_restarts_or_records_unavailable_sample() {
    let config = worker_config().restart_policy(LeanWorkerRestartPolicy::default().max_rss_kib(1));
    let mut worker = LeanWorker::spawn(&config).expect("worker starts");
    worker.health().expect("request succeeds with RSS policy");
    let stats = worker.stats();
    if stats.rss_samples_unavailable > 0 {
        assert_eq!(stats.rss_restarts, 0);
    } else {
        assert_eq!(stats.restarts, 1);
        assert_eq!(stats.rss_restarts, 1);
        match stats.last_restart_reason {
            Some(LeanWorkerRestartReason::RssCeiling {
                current_kib,
                limit_kib,
                last_import_stats,
            }) => {
                assert!(current_kib >= limit_kib);
                assert_eq!(limit_kib, 1);
                assert!(last_import_stats.is_none());
            }
            other => panic!("expected RSS restart reason, got {other:?}"),
        }
    }
    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success, "worker should exit cleanly after RSS policy check");
}

fn kill_and_observe_exit(worker: &mut LeanWorker) {
    worker.__kill_for_test().expect("worker kill succeeds");
    let started = Instant::now();
    loop {
        if let LeanWorkerStatus::Exited(exit) = worker.status().expect("observe killed worker") {
            assert!(!exit.success, "killed worker exited successfully");
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "worker exit remained unconfirmed"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// The next request replaces a child that died between requests, and the
/// replacement records the exit that it found.
#[test]
fn next_request_replaces_a_dead_child() {
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    worker.health().expect("health check succeeds");
    kill_and_observe_exit(&mut worker);
    worker.health().expect("the next request replaces the killed child");
    let stats = worker.stats();
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.abnormal_restarts, 1);
    match stats.last_restart_reason {
        Some(LeanWorkerRestartReason::ChildAbort { operation, .. }) => assert_eq!(operation, "health"),
        other => panic!("expected a child_abort restart, got {other:?}"),
    }
}

#[test]
fn child_crash_is_typed_and_parent_survives() {
    let (fixture, config) = scalar_fixture();
    let worker = LeanWorker::spawn(&config).expect("worker starts");
    let exit = worker
        .__trigger_lean_panic_fixture(&fixture)
        .expect("parent observes child fatal exit");
    assert!(!exit.success, "panic fixture should terminate the child");
    if !exit.diagnostics.is_empty() {
        assert!(
            exit.diagnostics.contains("lean_rs_fixture: deliberate Lean panic"),
            "child diagnostics should contain Lean panic message, got:\n{}",
            exit.diagnostics,
        );
    }
}

#[test]
fn child_crash_and_policy_restart_are_distinguishable() {
    let config = worker_config().restart_policy(LeanWorkerRestartPolicy::default().max_requests(1));
    let mut worker = LeanWorker::spawn(&config).expect("worker starts");
    worker.health().expect("first request succeeds");
    worker.health().expect("policy restart happens before second request");
    let stats = worker.stats();
    assert_eq!(stats.max_request_restarts, 1);
    assert_eq!(stats.abnormal_restarts, 0);
    assert!(
        stats
            .last_restart_reason
            .as_ref()
            .is_some_and(LeanWorkerRestartReason::is_planned)
    );
    let exit = worker.shutdown().expect("policy worker terminates").exit;
    assert!(exit.success, "policy worker should exit cleanly");

    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    kill_and_observe_exit(&mut worker);
    worker.health().expect("the next request replaces the killed child");
    let stats = worker.stats();
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.abnormal_restarts, 1);
    assert!(
        stats
            .last_restart_reason
            .as_ref()
            .is_some_and(|reason| !reason.is_planned())
    );
}

#[test]
fn missing_fixture_path_maps_to_capability_build_error() {
    let missing = workspace_root()
        .join("fixtures")
        .join("definitely-missing-worker-fixture");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let err = worker
        .load_fixture_capability(&missing)
        .expect_err("missing fixture path should be a typed capability build error");
    match err {
        LeanWorkerError::CapabilityBuild { diagnostic } => {
            let message = diagnostic.to_string();
            assert!(
                message.contains("definitely-missing-worker-fixture"),
                "message should identify missing fixture path, got {message}",
            );
        }
        other => panic!("expected capability build error, got {other:?}"),
    }
    let exit = worker.shutdown().expect("worker terminates after typed error").exit;
    assert!(exit.success, "worker should stay alive after typed load error");
}

#[test]
fn startup_failure_is_typed() {
    let missing = workspace_root().join("target").join("definitely-missing-worker-child");
    let err = LeanWorker::spawn(&LeanWorkerConfig::new(&missing)).expect_err("spawn should fail");
    match err {
        LeanWorkerError::Spawn { executable, .. } => assert_eq!(executable, missing),
        other => panic!("expected spawn error, got {other:?}"),
    }
}

#[test]
fn startup_timeout_is_typed() {
    let err = LeanWorker::spawn(
        &LeanWorkerConfig::new("/bin/cat")
            .env("unused", "unused")
            .startup_timeout(Duration::from_millis(20)),
    )
    .expect_err("non-worker child should not handshake");
    match err {
        LeanWorkerError::Timeout {
            operation, resource, ..
        } => {
            assert_eq!(operation, "startup");
            assert_eq!(resource.cause, "worker_timeout");
            assert!(!resource.work_entered_child);
        }
        other => panic!("expected startup timeout, got {other:?}"),
    }
}

/// The supervisor's per-spawn `LeanWorkerConfig::env` is the carrier for
/// the per-toolchain `LEAN_SYSROOT` set by `LeanWorkerChild::for_toolchain`.
/// Confirm it actually reaches the spawned child's environment.
#[cfg(unix)]
#[test]
fn config_env_propagates_lean_sysroot_to_spawned_child() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    const SENTINEL: &str = "/synthetic/sysroot/marker-7f3a";
    let script_path = std::env::temp_dir().join(format!(
        "lean-rs-worker-sysroot-env-{}-{}.sh",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    ));
    // The script echoes the LEAN_SYSROOT it sees and exits non-zero without
    // a handshake. Stderr is captured by the post-mortem path and surfaces
    // in LeanWorkerExit.diagnostics.
    fs::write(
        &script_path,
        "#!/bin/sh\nprintf 'sysroot=%s\\n' \"$LEAN_SYSROOT\" >&2\nexit 1\n",
    )
    .expect("temp script writes");
    let mut perms = fs::metadata(&script_path).expect("temp script stat").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("temp script chmod");

    let err = LeanWorker::spawn(&LeanWorkerConfig::new(&script_path).env("LEAN_SYSROOT", SENTINEL))
        .expect_err("script exits without a handshake; spawn must fail");
    drop(fs::remove_file(&script_path));

    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => {
            assert!(
                exit.diagnostics.contains(&format!("sysroot={SENTINEL}")),
                "spawned child must see the LEAN_SYSROOT set via LeanWorkerConfig::env; got: {:?}",
                exit.diagnostics,
            );
        }
        other => panic!("expected ChildPanicOrAbort, got {other:?}"),
    }
}

/// When a child fails *before* sending its handshake frame (closing stdout
/// without a valid frame), the supervisor must surface the child's stderr in
/// `LeanWorkerExit.diagnostics` rather than dropping it. Regression for a
/// race where the prior implementation called `try_wait` (non-blocking) on
/// the still-dying child and reported a bare `Handshake { message }` error
/// with no diagnostic payload.
#[cfg(unix)]
#[test]
fn pre_handshake_child_failure_carries_stderr() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    const MARKER: &str = "pre_handshake_failure_marker_42";
    let script_path = std::env::temp_dir().join(format!(
        "lean-rs-worker-pre-handshake-{}-{}.sh",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    ));
    fs::write(&script_path, format!("#!/bin/sh\necho '{MARKER}' >&2\nexit 1\n")).expect("temp script writes");
    let mut perms = fs::metadata(&script_path).expect("temp script stat").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("temp script chmod");

    let err = LeanWorker::spawn(&LeanWorkerConfig::new(&script_path))
        .expect_err("pre-handshake script exits without a handshake; spawn must fail");
    drop(fs::remove_file(&script_path));

    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => {
            assert!(!exit.success, "child exited non-zero, got exit: {exit:?}");
            assert!(
                exit.diagnostics.contains(MARKER),
                "stderr from a pre-handshake child failure must reach the caller; got diagnostics: {:?}",
                exit.diagnostics,
            );
        }
        other => panic!("expected ChildPanicOrAbort carrying stderr, got {other:?}"),
    }
}

/// A worker child dies with its parent, even while its main thread is inside a
/// Lean call.
///
/// The test binary runs itself again as the parent. That parent opens the write
/// end of a probe FIFO in a scratch directory, clears its `FD_CLOEXEC` flag and
/// starts a real worker there. It then closes its own copy, so the worker holds
/// the only write end. It writes the worker's pid and asks the worker to
/// elaborate an `#eval` that creates a marker file and then sleeps for a minute.
///
/// Once the marker exists, the probe must still be open. That shows the worker
/// inherited a descriptor its parent left open without `FD_CLOEXEC`, and that
/// the SDK did not close it. The test then kills the parent with SIGKILL. The
/// probe reaches end of file when the worker exits, which must happen long
/// before its Lean call would return.
///
/// Only an application worker may elaborate source. The SDK fixture
/// qualification runs this test with its sealed worker, whose fixed policy
/// admits exactly `BUSY_SOURCE` under `BUSY_FILE`.
#[cfg(unix)]
mod parent_death {
    use std::fs;
    use std::io::{ErrorKind, Read as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use lean_rs_worker_parent::{
        LeanWorker, LeanWorkerElabOptions, LeanWorkerModuleQuerySelector, LeanWorkerOutputBudgets,
        LeanWorkerSessionConfig,
    };

    use super::{ensure_fixture_built, fixture_root, ordinary_typed_selection, ordinary_typed_worker, worker_config};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Names the directory the parent role works in. Its absence means the
    /// parent role runs as an ordinary ignored test and does nothing.
    const DIRECTORY_ENV: &str = "LEAN_RS_TEST_PARENT_DEATH_DIRECTORY";

    /// The file label the fixture worker's policy admits for `BUSY_SOURCE`.
    const BUSY_FILE: &str = "/parent-death/busy.lean";

    /// Creates `MARKER` in the worker's directory, then keeps the worker
    /// inside Lean for `LEAN_SLEEP`.
    const BUSY_SOURCE: &str = "#eval show IO Unit from do\n  IO.FS.writeFile \"busy\" \"\"\n  IO.sleep 60000\n";

    /// The file `BUSY_SOURCE` creates once the worker is inside the Lean call.
    const MARKER: &str = "busy";

    /// The FIFO whose write end only the worker holds once it is busy.
    const PROBE: &str = "probe";

    /// How long the Lean call would keep the worker busy if nothing stopped it.
    const LEAN_SLEEP: Duration = Duration::from_secs(60);

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn session_root() -> PathBuf {
        ordinary_typed_selection::ordinary_typed_selection()
            .map_or_else(fixture_root, |selected| selected.fixture.clone())
    }

    /// Removes the test directory on every exit path.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> std::io::Result<Self> {
            let directory = std::env::temp_dir().join(format!(
                "lean-rs-parent-death-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory)?;
            Ok(Self(directory.canonicalize()?))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _cleanup = fs::remove_dir_all(&self.0);
        }
    }

    /// Stands in for a host: it starts a worker and keeps it busy in Lean
    /// until the test kills this process.
    #[test]
    #[ignore = "parent role; busy_worker_exits_when_its_parent_is_killed starts it in its own process"]
    fn parent_role() -> TestResult {
        let Some(directory) = std::env::var_os(DIRECTORY_ENV).map(PathBuf::from) else {
            return Ok(());
        };
        // `.cargo/config.toml` caps each worker's Lean runtime at 1.5 GiB. On
        // Linux this worker's resident memory passes that cap during the
        // `#eval`, and Lean stops it with "(kernel) excessive memory
        // consumption detected" before `BUSY_SOURCE` writes `MARKER`. This test
        // checks the lifeline, not the cap, so its worker runs without one.
        let config = ordinary_typed_worker::worker_config()
            .unwrap_or_else(worker_config)
            .current_dir(&directory)
            .env("LEAN_RS_LEAN_MAX_MEMORY_KIB", "0");
        let probe = fs::OpenOptions::new().write(true).open(directory.join(PROBE))?;
        // Leave the probe open across exec, as a server leaves its store lock.
        rustix::io::fcntl_setfd(&probe, rustix::io::FdFlags::empty())?;
        let mut worker = LeanWorker::spawn(&config)?;
        drop(probe);
        let pid = worker.__child_pid_for_test().ok_or("worker has no live child")?;
        fs::write(directory.join("worker.pid"), pid.to_string())?;
        let session_config = LeanWorkerSessionConfig::new(
            session_root(),
            "lean_rs_fixture",
            "LeanRsFixture",
            ["LeanRsHostShims.Elaboration"],
        );
        let mut session = worker.open_session(&session_config, None, None)?;
        let outcome = session.process_module_query_batch(
            BUSY_SOURCE,
            &[LeanWorkerModuleQuerySelector::Diagnostics { id: "busy".to_owned() }],
            &LeanWorkerOutputBudgets::default(),
            &LeanWorkerElabOptions::new().file_label(BUSY_FILE),
            None,
            None,
        );
        // Reaching this line means the Lean call ended before the test killed
        // this process; the test reports it as a missing marker or a slow exit.
        Err(format!("busy Lean call returned: {outcome:?}").into())
    }

    fn wait_for(
        limit: Duration,
        mut done: impl FnMut() -> Result<bool, Box<dyn std::error::Error>>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let started = Instant::now();
        while started.elapsed() < limit {
            if done()? {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        done()
    }

    /// Whether some process still holds a write end of the probe.
    fn probe_open(probe: &mut fs::File) -> Result<bool, Box<dyn std::error::Error>> {
        match probe.read(&mut [0_u8; 1]) {
            Ok(0) => Ok(false),
            Ok(_) => Err("the probe carried data".into()),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    fn process_group_of(pid: &str) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("ps").args(["-o", "pgid=", "-p", pid]).output()?;
        if !output.status.success() {
            return Err(format!("ps found no process {pid}").into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    /// Kill a worker that outlived the test, so a failure leaves nothing behind.
    fn kill_leftover(pid: &str) {
        let _cleanup = Command::new("kill").args(["-KILL", pid]).status();
    }

    #[test]
    fn busy_worker_exits_when_its_parent_is_killed() -> TestResult {
        if ordinary_typed_selection::ordinary_typed_selection().is_none() {
            ensure_fixture_built();
        }
        let scratch = Scratch::new()?;
        let fifo = scratch.0.join(PROBE);
        if !Command::new("mkfifo").arg(&fifo).status()?.success() {
            return Err("mkfifo failed".into());
        }
        // A blocking open of a FIFO's read end would wait for a writer.
        let mut probe = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", "parent_death::parent_role", "--ignored", "--nocapture"])
            .env(DIRECTORY_ENV, &scratch.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        // When the parent dies, Linux hands the worker to the nearest
        // subreaper above it, or to init. After the worker exits it stays a
        // zombie until that process reaps it, and the fixture qualification
        // then finds its PID still in use. As the subreaper, this process can
        // reap the worker itself and read its exit status.
        #[cfg(target_os = "linux")]
        rustix::process::set_child_subreaper(Some(rustix::process::getpid()))?;
        let mut parent = command.spawn()?;

        let marker = scratch.0.join(MARKER);
        let mut early_exit = None;
        // Stay inside Nextest's four-minute limit so a failure reports here.
        let busy = wait_for(Duration::from_secs(120), || {
            early_exit = parent.try_wait()?;
            Ok(early_exit.is_some() || marker.exists())
        })?;
        let pid = fs::read_to_string(scratch.0.join("worker.pid")).unwrap_or_default();
        if !busy || early_exit.is_some() || pid.is_empty() {
            parent.kill()?;
            parent.wait()?;
            if !pid.is_empty() {
                kill_leftover(&pid);
            }
            return Err(format!("worker never became busy in Lean; parent exit: {early_exit:?}").into());
        }

        let group = process_group_of(&pid);
        if group.as_deref().ok() != Some(pid.as_str()) {
            parent.kill()?;
            parent.wait()?;
            kill_leftover(&pid);
            return Err(format!("worker {pid} does not lead its own process group: {group:?}").into());
        }

        // The parent closed its copy before it wrote the pid, so only the
        // worker can hold the probe open now.
        if !probe_open(&mut probe)? {
            parent.kill()?;
            parent.wait()?;
            kill_leftover(&pid);
            return Err(format!("worker {pid} does not hold the probe its parent left open").into());
        }

        // SIGKILL leaves the parent no chance to stop the worker itself.
        parent.kill()?;
        parent.wait()?;
        let killed = Instant::now();
        if !wait_for(LEAN_SLEEP / 6, || Ok(!probe_open(&mut probe)?))? {
            kill_leftover(&pid);
            return Err(format!("worker {pid} kept the probe open 10 s after its parent died").into());
        }
        let elapsed = killed.elapsed();
        assert!(
            elapsed < LEAN_SLEEP / 6,
            "worker took {elapsed:?} to exit after its parent died"
        );
        #[cfg(target_os = "linux")]
        reap_orphan(&pid)?;
        Ok(())
    }

    /// Reaps the worker this process inherited as its subreaper, and checks
    /// that the lifeline stopped it: `parent_watchdog.rs` exits with 125.
    #[cfg(target_os = "linux")]
    fn reap_orphan(pid: &str) -> TestResult {
        use rustix::process::{Pid, WaitOptions, waitpid};

        let worker = Pid::from_raw(pid.trim().parse()?).ok_or("worker PID is not positive")?;
        let mut reaped = None;
        // The worker closes the probe as it exits, a moment before it becomes
        // a zombie, so the first `waitpid` may find it still running.
        wait_for(Duration::from_secs(10), || {
            reaped = waitpid(Some(worker), WaitOptions::NOHANG)
                .map_err(|error| format!("waitpid on worker {pid}: {error}"))?;
            Ok(reaped.is_some())
        })?;
        let Some((_, status)) = reaped else {
            kill_leftover(pid);
            return Err(format!("worker {pid} closed the probe but did not exit").into());
        };
        assert_eq!(
            status.exit_status(),
            Some(125),
            "worker {pid} ended with {status:?}, not the lifeline's exit status"
        );
        Ok(())
    }
}
