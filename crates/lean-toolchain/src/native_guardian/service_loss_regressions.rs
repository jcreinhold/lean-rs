//! Service loss with a writer already contending, without controller-held locks.
use super::*;
use std::mem::{MaybeUninit, size_of};

const CONTENDER: &str = "native_guardian::tests::service_loss_regressions::fixture_contender";
const MARKER: &[u8] = b"|contender|";
const RECOVERY_LIMIT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Observation {
    pub(super) group_present: bool,
    pub(super) worker_live: bool,
    pub(super) managed_writer_ready: bool,
    pub(super) payload_bytes_after_marker: usize,
}

fn write_json(root: &Path, name: &str, value: &impl Serialize) {
    let temporary = root.join(format!("{name}.tmp"));
    fs::write(&temporary, serde_json::to_vec(value).unwrap()).unwrap();
    fs::rename(temporary, root.join(name)).unwrap();
}

// Own only this actual service/contender child. In particular, the service-loss
// controller never opens the artifact File or adopts the worker Child.
struct TestChild(Child);
impl TestChild {
    fn wait(&mut self) -> ExitStatus {
        let mut status = None;
        wait_until(|| {
            status = self.0.try_wait().unwrap();
            status.is_some()
        });
        status.unwrap()
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _kill = self.0.kill();
            let _wait = self.0.wait();
        }
    }
}

pub(super) struct Contender {
    child: TestChild,
    root: PathBuf,
}
impl Contender {
    pub(super) fn start(fixture: &Fixture, worker: u32) -> Self {
        let process = NativeProcessIdentity::capture(worker).unwrap().unwrap();
        write_json(&fixture.root, "contender-worker.json", &process);
        let errors = File::create(fixture.root.join("contender.stderr")).unwrap();
        let child = TestChild(
            fixture_command(&fixture.root, CONTENDER)
                .stderr(errors)
                .spawn()
                .unwrap(),
        );
        wait_until(|| fixture.root.join("contender-blocked").exists());
        Self {
            child,
            root: fixture.root.clone(),
        }
    }
    pub(super) fn acquired(&self) -> Observation {
        wait_until(|| self.root.join("contender-acquired.json").exists());
        serde_json::from_slice(&fs::read(self.root.join("contender-acquired.json")).unwrap()).unwrap()
    }
    pub(super) fn finish(mut self) -> Observation {
        fs::write(self.root.join("scope-closed"), b"observed by test controller").unwrap();
        assert!(self.child.wait().success(), "contender failed: {}", self.root.display());
        serde_json::from_slice(&fs::read(self.root.join("contender-result.json")).unwrap()).unwrap()
    }
}

#[test]
fn fixture_contender() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let process: NativeProcessIdentity =
        serde_json::from_slice(&fs::read(root.join("contender-worker.json")).unwrap()).unwrap();
    let lock_path = crate::shared_writer::project_lock_path(&root.join("project")).unwrap();
    let file = OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
    assert!(
        matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "first real attempt must block before fault"
    );
    fs::write(root.join("contender-blocked"), b"actual EX attempt blocked").unwrap();
    wait_until_within(Duration::from_secs(2), || match file.try_lock() {
        Ok(()) => true,
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(error) => panic!("contender lock failed: {error}"),
    });
    // These observations FOLLOW the lock call; the later marker/log cannot
    // establish the exact acquisition instant or every scheduling interleaving.
    let mut observation = Observation {
        group_present: crate::process_group::group_present(process.process_group).unwrap(),
        worker_live: process.is_live().unwrap(),
        managed_writer_ready: false,
        payload_bytes_after_marker: 0,
    };
    let metadata = file.metadata().unwrap();
    observation.managed_writer_ready =
        protected_native_reads_stopped(&root.join("owners"), &lock_path, (metadata.dev(), metadata.ino())).unwrap();
    // This intentionally probes lock-only behavior EVEN IF the managed-writer
    // gate says no. It is not a production SharedWriter executing forbidden work.
    OpenOptions::new()
        .append(true)
        .open(root.join("heartbeat"))
        .unwrap()
        .write_all(MARKER)
        .unwrap();
    write_json(&root, "contender-acquired.json", &observation);
    wait_until(|| root.join("scope-closed").exists());
    thread::sleep(Duration::from_millis(35));
    let bytes = fs::read(root.join("heartbeat")).unwrap();
    let marker = bytes.windows(MARKER.len()).position(|part| part == MARKER).unwrap();
    observation.payload_bytes_after_marker = bytes.len() - marker - MARKER.len();
    write_json(&root, "contender-result.json", &observation);
    // Close-only custody: never explicitly unlock or convert a live lock mode.
    drop(file);
}

// This deadline is TEST RESCUE, not a lease. It runs in the SAME guardian, with
// its existing validated live SID anchor, and adds no custodian or lock alias.
pub(super) fn guardian_fixture(root: &Path) -> io::Result<()> {
    let worker: u32 = std::env::var(WORKER_ENV)
        .map_err(io::Error::other)?
        .parse()
        .map_err(io::Error::other)?;
    native::validate_pin(worker)?;
    let watched = root.to_path_buf();
    thread::spawn(move || {
        thread::sleep(RECOVERY_LIMIT);
        if crate::process_group::group_present(worker).unwrap_or(true) {
            rescue(&watched, worker);
        }
    });
    let result = run_guardian_cli();
    if crate::process_group::group_present(worker)? {
        rescue(root, worker);
        return Err(io::Error::other("guardian returned before scope closure"));
    }
    if result.is_ok() {
        fs::write(root.join("guardian-returned"), b"real native drain returned")?;
    }
    result
}
fn rescue(root: &Path, worker: u32) {
    fs::write(root.join("test-rescue-used"), b"not a passing cleanup").unwrap();
    while crate::process_group::group_present(worker).unwrap_or(true) {
        // The real helper checks this very guardian's SID/PGID before signalling.
        // No deserialized process identity authorizes a signal.
        let _stop = native::kill_payload(worker);
        thread::sleep(POLL);
    }
}

#[allow(
    unsafe_code,
    reason = "read-only macOS state query confirms the isolated worker is stopped, with exact birth checked separately"
)]
fn stopped(process: &NativeProcessIdentity) -> bool {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: proc_pidinfo writes into this correctly sized, aligned output. It
    // only inspects the authenticated fixture worker; it grants no stop authority.
    let written = unsafe {
        libc::proc_pidinfo(
            i32::try_from(process.pid).unwrap(),
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            i32::try_from(size_of::<libc::proc_bsdinfo>()).unwrap(),
        )
    };
    assert_eq!(
        written,
        i32::try_from(size_of::<libc::proc_bsdinfo>()).unwrap(),
        "native process query failed: {}",
        io::Error::last_os_error()
    );
    // SAFETY: successful exact-sized proc_pidinfo initialized the whole structure.
    let info = unsafe { info.assume_init() };
    info.pbi_status == libc::SSTOP && NativeProcessIdentity::capture(process.pid).unwrap().as_ref() == Some(process)
}

pub(super) fn assert_service_loss(freeze_worker: bool) {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("service-loss-watchdog"), b"test-only deadline").unwrap();
    let errors = File::create(fixture.root.join("server.stderr")).unwrap();
    let started = Instant::now();
    let mut server = TestChild(
        fixture_command(&fixture.root, "native_guardian::tests::fixture_server")
            .stderr(errors)
            .spawn()
            .unwrap(),
    );
    wait_until(|| fixture.root.join("server-ready").exists());
    let info: Value = serde_json::from_slice(&fs::read(fixture.root.join("server-ready")).unwrap()).unwrap();
    let worker = pid(field(&info, "worker")).unwrap();
    let process = NativeProcessIdentity::capture(worker).unwrap().unwrap();
    let guardian: NativeProcessIdentity = serde_json::from_value(field(&info, "guardian").clone()).unwrap();
    let contender = Contender::start(&fixture, worker);
    if freeze_worker {
        assert!(server.0.try_wait().unwrap().is_none());
        // Test fault only, while the live SDK service retains this worker's
        // actual Child. No worker PID is signalled after service death.
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(i32::try_from(worker).unwrap()).unwrap(),
            rustix::process::Signal::STOP,
        )
        .unwrap();
        wait_until(|| stopped(&process));
        fs::write(fixture.root.join("worker-stopped"), b"exact stopped state").unwrap();
    }
    server.0.kill().unwrap();
    assert_eq!(server.wait().signal(), Some(libc::SIGKILL));
    // Observation only. The healthy SDK guardian, not this controller, stops the
    // whole worker group and retains its SH loan until that group is absent.
    let acquired = contender.acquired();
    wait_until_within(Duration::from_secs(2), || {
        !crate::process_group::group_present(worker).unwrap() && !guardian.is_live().unwrap()
    });
    assert!(
        started.elapsed() < RECOVERY_LIMIT,
        "setup plus cleanup must precede the leaf's four-second expiry"
    );
    assert!(!fixture.root.join("test-rescue-used").exists());
    assert!(!fixture.root.join("guardian-error").exists());
    assert!(fixture.root.join("guardian-returned").exists());
    let observed = contender.finish();
    assert!(!acquired.group_present && !acquired.worker_live, "{acquired:?}");
    assert!(acquired.managed_writer_ready, "{acquired:?}");
    assert_eq!(observed.payload_bytes_after_marker, 0, "{observed:?}");
    assert!(!crate::process_group::group_present(guardian.process_group).unwrap());
}
