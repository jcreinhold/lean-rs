//! Real private worker/guardian generations, plus bounded admission faults.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::exit,
    clippy::significant_drop_tightening,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "isolated lifecycle fixtures assert exact kernel identities and durable protocol outcomes"
)]

use super::*;
use std::fs::OpenOptions;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::UnixListener;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicUsize, Ordering};

const ROOT_ENV: &str = "LEAN_NATIVE_GUARDIAN_TEST_ROOT";
const NONCE: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdef";
const LIMIT: Duration = Duration::from_secs(7);
const GUARDIAN_RESCUE: &str = "guardian-fixture-deadline";
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[path = "service_loss_regressions.rs"]
mod service_loss_regressions;
const LEAF: &str = r"
import os, pathlib, signal, sys, time
root = pathlib.Path(sys.argv[1])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
signal.alarm(4)
keep = int(sys.argv[2])
for descriptor in range(3, 4096):
    if descriptor != keep:
        try:
            os.close(descriptor)
        except OSError:
            pass
if keep > 2:
    os.fstat(keep)
    (root / 'control-retained').write_text(str(keep))
null = os.open('/dev/null', os.O_RDWR)
for target in [0, 1, 2]:
    os.dup2(null, target)
if null > 2:
    os.close(null)
with (root / 'heartbeat').open('ab', buffering=0) as output:
    until = time.monotonic() + 4
    while time.monotonic() < until:
        output.write(b'x')
        time.sleep(0.005)
os._exit(0)
";

struct Fixture {
    root: PathBuf,
    project: PathBuf,
    registry: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = Path::new("/tmp").join(format!(
            "native-guard-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let registry = root.join("owners");
        fs::create_dir(&project).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&registry).unwrap();
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .replace('\'', "'\\''");
        fs::write(
            root.join("guardian"),
            format!("#!/bin/sh\nexec '{executable}' --exact native_guardian::tests::fixture_guardian --nocapture\n"),
        )
        .unwrap();
        fs::set_permissions(root.join("guardian"), fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            project,
            registry,
        }
    }
    fn lock(&self) -> PathBuf {
        crate::shared_writer::project_lock_path(&self.project).unwrap()
    }
    fn heartbeat(&self) -> u64 {
        heartbeat(&self.root)
    }
    fn exclusive(&self) -> File {
        let file = OpenOptions::new().read(true).write(true).open(self.lock()).unwrap();
        wait_until(|| file.try_lock().is_ok());
        file
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!("preserved failed native guardian fixture: {}", self.root.display());
            return;
        }
        let _removed = fs::remove_dir_all(&self.root);
    }
}

fn wait_until(condition: impl FnMut() -> bool) {
    wait_until_within(LIMIT, condition);
}
fn wait_until_within(limit: Duration, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < limit, "private native generation did not settle");
        thread::sleep(POLL);
    }
}
fn heartbeat(root: &Path) -> u64 {
    fs::metadata(root.join("heartbeat")).map_or(0, |metadata| metadata.len())
}
fn fixture_command(root: &Path, name: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env(ROOT_ENV, root)
        .env(OWNERS_DIRECTORY_ENV, root.join("owners"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

struct Generation {
    worker: Child,
    parent: Option<UnixStream>,
    record: PathBuf,
    guardian: NativeProcessIdentity,
    root: PathBuf,
}
impl Generation {
    fn start(root: &Path) -> Self {
        Self::start_with_slot(root, None)
    }
    fn start_with_slot(root: &Path, slot: Option<&NativeCapacitySlot>) -> Self {
        let registry = root.join("owners");
        let record = reserve_owner_record_with_slot(&registry, NONCE, Duration::from_secs(2), slot).unwrap();
        let socket = root.join("parent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        ownership::dispatch_owner_record(&record, NONCE).unwrap();
        let worker = fixture_command(root, "native_guardian::tests::fixture_worker")
            .spawn()
            .unwrap();
        let mut connection = None;
        wait_until(|| match listener.accept() {
            Ok((stream, _)) => {
                connection = Some(stream);
                true
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("private guardian accept: {error}"),
        });
        let mut parent = connection.unwrap();
        parent.set_nonblocking(true).unwrap();
        let hello = read_frame(&mut parent, deadline(Duration::from_secs(2)).unwrap()).unwrap();
        assert_eq!(field(&hello, "nonce"), NONCE);
        assert_eq!(field(&hello, "worker_pid"), worker.id());
        let guardian_pid = pid(field(&hello, "guardian_pid")).unwrap();
        let process = NativeProcessIdentity::capture(worker.id()).unwrap().unwrap();
        let guardian = NativeProcessIdentity::capture(guardian_pid).unwrap().unwrap();
        assert_eq!(process.pid, process.session);
        assert_eq!(process.pid, process.process_group);
        assert_eq!(guardian.session, process.session);
        assert_eq!(guardian.pid, guardian.process_group);
        assert_ne!(guardian.process_group, process.process_group);
        let server = NativeProcessIdentity::capture(std::process::id()).unwrap().unwrap();
        let mut value = json!({"version":2,"scope":"inherited-process-group-v2",
            "phase":"bootstrap","generation_id":NONCE,"parent":server,"process":process,
            "guardian":guardian,"protected_domains":[],"capacity_slot":slot});
        ownership::publish_startup_owner(&record, &value, "dispatching", Duration::from_secs(2)).unwrap();
        value["phase"] = json!("running");
        ownership::publish_startup_owner(&record, &value, "bootstrap", Duration::from_secs(2)).unwrap();
        write_frame(
            &mut parent,
            &json!({"state":"durable-owner-ready","nonce":NONCE,"record":record}),
            deadline(Duration::from_secs(2)).unwrap(),
        )
        .unwrap();
        let generation = Self {
            worker,
            parent: Some(parent),
            record,
            guardian,
            root: root.to_path_buf(),
        };
        wait_until(|| root.join("body-ready").exists());
        generation
    }
    fn reap(&mut self) -> ExitStatus {
        let mut status = None;
        wait_until(|| {
            status = self.worker.try_wait().unwrap();
            status.is_some()
        });
        status.unwrap()
    }
    fn kill_root(&mut self) -> ExitStatus {
        if self.worker.try_wait().unwrap().is_none() {
            self.worker.kill().unwrap();
        }
        self.reap()
    }
    fn closed(&mut self) {
        self.reap();
        // This deadline precedes both the leaf's four-second safety alarm
        // and worker's six-second fallback, so neither can mask failed cleanup.
        wait_until_within(Duration::from_secs(2), || {
            !crate::process_group::group_present(self.worker.id()).unwrap()
        });
        wait_until_within(Duration::from_secs(2), || !self.guardian.is_live().unwrap());
        assert!(!self.root.join(GUARDIAN_RESCUE).exists());
        let before = heartbeat(&self.root);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(heartbeat(&self.root), before);
    }
    fn kill_guardian_through_its_actual_owner(&self) {
        fs::write(self.root.join("kill-guardian"), b"owned Child kill").unwrap();
        wait_until(|| self.root.join("guardian-reaped").exists());
    }
}
impl Drop for Generation {
    fn drop(&mut self) {
        drop(self.parent.take());
        if matches!(self.worker.try_wait(), Ok(None)) {
            let _kill = self.worker.kill();
        }
        let until = Instant::now() + LIMIT;
        while Instant::now() < until {
            if matches!(self.worker.try_wait(), Ok(Some(_)))
                && matches!(crate::process_group::group_present(self.worker.id()), Ok(false))
            {
                break;
            }
            thread::sleep(POLL);
        }
    }
}

#[test]
fn fixture_guardian() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let watched = root.clone();
    thread::spawn(move || {
        // The private payload expires after at most six seconds. If startup
        // fails before binding a record, bound the fixture guardian's drain.
        thread::sleep(Duration::from_secs(15));
        let _saved = fs::write(watched.join(GUARDIAN_RESCUE), b"failed fixture cleanup");
        std::process::exit(92);
    });
    let result = if root.join("service-loss-watchdog").exists() {
        service_loss_regressions::guardian_fixture(&root)
    } else {
        run_guardian_cli()
    };
    if result.is_ok() {
        record_lifetime_tests::record_cli_return(&root);
    }
    if let Err(error) = result {
        let _saved = fs::write(root.join("guardian-error"), error.to_string());
        eprintln!("private guardian: {error}");
        std::process::exit(91);
    }
}

#[test]
#[allow(
    clippy::zombie_processes,
    reason = "the fixture deliberately leaves its descendant alive for guardian cleanup when the worker exits"
)]
fn fixture_worker() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    rustix::process::setsid().unwrap();
    let mut guardian = GuardianHandle::spawn(
        &root.join("guardian"),
        &root.join("parent.sock"),
        NONCE,
        Duration::from_secs(3),
    )
    .unwrap();
    let project = root.join("project");
    let lock = crate::shared_writer::project_lock_path(&project).unwrap();
    let record = root.join("owners").join(format!("worker-{NONCE}")).join("owner.json");
    let hold_alias = root.join("hold-read-alias").exists();
    let mut read_alias = None;
    for sequence in 0..if hold_alias { 1 } else { 3 } {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
            .unwrap();
        file.try_lock_shared().unwrap();
        let metadata = file.metadata().unwrap();
        let domain = NativeReadDomain {
            project_root: project.clone(),
            lock_path: lock.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
            epoch: "generation-epoch".to_owned(),
        };
        guardian
            .retain_reads(
                &[GuardianReadLoan {
                    domain: domain.clone(),
                    descriptor: file.as_fd(),
                }],
                Duration::from_secs(2),
            )
            .unwrap();
        // A returned ACK must already be observable in the sole durable record.
        let published = read_owner(&record).unwrap();
        assert_eq!(field(&published, "protected_domains"), &json!([domain]));
        fs::write(root.join(format!("ack-{sequence}")), b"durable").unwrap();
        if hold_alias {
            read_alias = Some(file.try_clone().unwrap());
        }
        drop(file);
    }
    let retain_control = root.join("retain-worker-control").exists();
    let mut command = Command::new("python3");
    command
        .args(["-c", LEAF])
        .arg(&root)
        .arg(if retain_control {
            guardian.control.as_raw_fd().to_string()
        } else {
            "-1".to_owned()
        })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut leaf = if retain_control {
        // Simulates a fork-only descendant retaining the endpoint. The
        // child-only FD flag change cannot alter the worker's own table.
        crate::process_group::spawn_inheriting(command, &[guardian.control.as_fd()]).unwrap()
    } else {
        command.spawn().unwrap()
    };
    wait_until(|| heartbeat(&root) > 0);
    fs::write(root.join("body-ready"), std::process::id().to_string()).unwrap();
    if let Some(alias) = &read_alias {
        wait_until_within(Duration::from_secs(2), || root.join("unlock-read-alias").exists());
        // Negative control: explicitly release the lock through a clone of the
        // description loaned to the guardian while both still hold their aliases.
        alias.unlock().unwrap();
    }
    let until = Instant::now() + Duration::from_secs(6);
    let mut guardian_killed = false;
    while Instant::now() < until {
        if root.join("kill-guardian").exists() && !guardian_killed {
            guardian.child.kill().unwrap();
            assert!(!guardian.child.wait().unwrap().success());
            fs::write(root.join("guardian-reaped"), b"exact Child reaped").unwrap();
            guardian_killed = true;
        }
        if root.join("finish-root").exists() {
            let _leaf_exit = leaf.try_wait();
            std::process::exit(0);
        }
        thread::sleep(POLL);
    }
    std::process::exit(0);
}

#[test]
fn fixture_server() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let generation = Generation::start(&root);
    fs::write(
        root.join("server-ready"),
        serde_json::to_vec(&json!({"worker":generation.worker.id(),
        "guardian":generation.guardian,"record":generation.record}))
        .unwrap(),
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(6);
    while Instant::now() < until {
        thread::sleep(POLL);
    }
    drop(generation);
}

#[test]
fn durable_fd_union_ack_survives_abrupt_root_exit_and_fd_closing_descendant() {
    let fixture = Fixture::new();
    let mut generation = Generation::start(&fixture.root);
    assert_eq!(
        read_owner(&generation.record).unwrap()["process"]["pid"],
        generation.worker.id()
    );
    for sequence in 0..3 {
        assert!(fixture.root.join(format!("ack-{sequence}")).exists());
    }
    assert_eq!(
        read_owner(&generation.record).unwrap()["protected_domains"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "repeated root loans must not grow the union"
    );
    let probe = OpenOptions::new().read(true).write(true).open(fixture.lock()).unwrap();
    assert!(
        matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "guardian must retain actual SH custody after worker aliases close"
    );
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);
    assert_eq!(generation.kill_root().signal(), Some(libc::SIGKILL));
    generation.closed();
    let exclusive = fixture.exclusive();
    let identity = exclusive.metadata().unwrap();
    assert!(
        protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (identity.dev(), identity.ino())).unwrap()
    );
}

#[test]
fn explicit_unlock_releases_alias_lock_but_registry_fences_live_group() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("hold-read-alias"), b"single loan with live clone").unwrap();
    let mut generation = Generation::start(&fixture.root);
    let contender = service_loss_regressions::Contender::start(&fixture, generation.worker.id());
    assert!(generation.guardian.is_live().unwrap());
    fs::write(fixture.root.join("unlock-read-alias"), b"deliberate unlock control").unwrap();
    let observed = contender.acquired();
    assert!(observed.group_present && observed.worker_live, "{observed:?}");
    assert!(generation.guardian.is_live().unwrap());
    assert!(!observed.managed_writer_ready, "{observed:?}");
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);

    drop(generation.parent.take());
    generation.closed();
    let closed = contender.finish();
    assert!(closed.payload_bytes_after_marker > 0, "{closed:?}");
    let exclusive = fixture.exclusive();
    let metadata = exclusive.metadata().unwrap();
    assert!(
        protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (metadata.dev(), metadata.ino())).unwrap()
    );
}

#[test]
fn root_death_is_detected_while_a_descendant_keeps_the_control_endpoint() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("retain-worker-control"), b"hold private endpoint").unwrap();
    let mut generation = Generation::start(&fixture.root);
    assert!(fixture.root.join("control-retained").exists());
    assert!(generation.parent.is_some(), "server lifeline remains open");
    assert_eq!(generation.kill_root().signal(), Some(libc::SIGKILL));
    generation.closed();
}

#[test]
fn independent_guardian_closes_scope_after_actual_server_sigkill() {
    service_loss_regressions::assert_service_loss(service_loss_regressions::Freeze::None);
}

#[test]
fn independent_guardian_closes_frozen_worker_after_actual_server_sigkill() {
    service_loss_regressions::assert_service_loss(service_loss_regressions::Freeze::Worker);
}

#[test]
fn stopped_service_keeps_custody_until_actual_service_death() {
    service_loss_regressions::assert_service_loss(service_loss_regressions::Freeze::Service);
}

#[test]
fn guardian_failure_releases_fd_but_durable_domain_fences_actual_ex_writer() {
    let fixture = Fixture::new();
    let mut generation = Generation::start(&fixture.root);
    let contender = service_loss_regressions::Contender::start(&fixture, generation.worker.id());
    generation.kill_guardian_through_its_actual_owner();
    let acquired = contender.acquired();
    assert!(acquired.group_present && acquired.worker_live, "{acquired:?}");
    assert!(
        !acquired.managed_writer_ready,
        "bare EX is not writer admission: {acquired:?}"
    );
    let metadata = fs::metadata(fixture.lock()).unwrap();
    assert!(
        !protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (metadata.dev(), metadata.ino())).unwrap()
    );
    assert!(
        protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (metadata.dev(), metadata.ino() + 1))
            .is_err(),
        "matching stable domain with replaced inode must fail closed"
    );
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);
    assert!(generation.worker.try_wait().unwrap().is_none());
    generation.kill_root();
    // No guardian remains to stop the leaf. Its bounded self-expiry is the only
    // cleanup here; do not signal a serialized PGID after losing its pin.
    assert!(crate::process_group::group_present(generation.worker.id()).unwrap());
    assert!(
        !protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (metadata.dev(), metadata.ino())).unwrap(),
        "losing both the root and guardian must leave the live leaf fenced"
    );
    wait_until(|| !crate::process_group::group_present(generation.worker.id()).unwrap());
    let observed = contender.finish();
    assert!(
        observed.payload_bytes_after_marker > 0,
        "lock-only control must detect live writes: {observed:?}"
    );
    assert!(
        protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (metadata.dev(), metadata.ino())).unwrap()
    );
}

#[test]
fn malformed_and_preparing_owner_records_cannot_weaken_admission() {
    let fixture = Fixture::new();
    let nonce = format!("{:048x}", 1);
    let record = reserve_owner_record(&fixture.registry, &nonce, Duration::from_secs(1)).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.lock())
        .unwrap();
    lock.try_lock().unwrap();
    let metadata = lock.metadata().unwrap();
    let identity = (metadata.dev(), metadata.ino());
    assert!(
        protected_native_reads_stopped(&fixture.registry, &fixture.lock(), identity).unwrap(),
        "valid preparing records have dispatched no artifact reads"
    );
    let preparing = read_owner(&record).unwrap();
    fs::write(&record, b"{not-json").unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), identity).is_err());
    let mut malformed = preparing.clone();
    malformed["version"] = json!(999);
    persist_owner(&record, &malformed).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), identity).is_err());
    let mut malformed = preparing.clone();
    malformed["process"] = json!({"pid":42});
    persist_owner(&record, &malformed).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), identity).is_err());
    let mut malformed = preparing;
    malformed["protected_domains"] = json!([{}]);
    persist_owner(&record, &malformed).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), identity).is_err());
}

#[test]
fn live_preparing_generations_exhaust_the_finite_namespace_without_eviction() {
    let fixture = Fixture::new();
    let mut first = None;
    for index in 0..MAX_OWNER_RECORDS {
        let record = reserve_owner_record(&fixture.registry, &format!("{index:048x}"), Duration::from_secs(1)).unwrap();
        if first.is_none() {
            first = Some(record);
        }
    }
    let error = reserve_owner_record(
        &fixture.registry,
        &format!("{MAX_OWNER_RECORDS:048x}"),
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(
        first.unwrap().exists(),
        "capacity pressure must not evict a live Preparing owner"
    );
    let records = fs::read_dir(&fixture.registry)
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().file_name() != ".admission.lock")
        .count();
    assert_eq!(records, MAX_OWNER_RECORDS);
}

#[test]
fn pure_protocol_limits_reject_ambiguous_or_unbounded_control() {
    assert!(valid_nonce(NONCE));
    assert!(!valid_nonce("short"));
    assert!(deadline(Duration::ZERO).is_err());
    assert!(frame(&json!({"padding":"x".repeat(MAX_FRAME)})).is_err());
    let (control, _other) = UnixStream::pair().unwrap();
    let mut guardian = Guardian {
        worker: std::process::id(),
        nonce: NONCE.to_owned(),
        control,
        parent: None,
        parent_identity: None,
        record: None,
        next_sequence: 0,
        loans: Vec::new(),
        incoming: Incoming::default(),
        bootstrap_identity: None,
    };
    assert!(
        guardian
            .retain(
                &json!({"op":"retain","nonce":NONCE,"sequence":1,"domain":{}}),
                Vec::new()
            )
            .is_err()
    );
    assert_eq!(guardian.next_sequence, 0);
    assert!(guardian.loans.is_empty());
}

#[path = "capacity_tests.rs"]
mod capacity_tests;

#[path = "record_lifetime_tests.rs"]
mod record_lifetime_tests;

pub(super) fn pause_after_payload_closed(guardian: &Guardian) {
    record_lifetime_tests::pause_after_payload_closed(guardian);
}
