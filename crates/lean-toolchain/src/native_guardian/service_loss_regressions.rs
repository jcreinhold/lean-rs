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
    lock_identity: (u64, u64),
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
    let open = || OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
    let first = open();
    let initial = first.metadata().unwrap();
    let identity = (initial.dev(), initial.ino());
    assert!(
        matches!(first.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "first real attempt must block before fault"
    );
    drop(first);
    fs::write(root.join("contender-blocked"), b"actual EX attempt blocked").unwrap();
    let managed = root.join("managed-contender").exists();
    let mut acquired = None;
    let mut reported_refusal = false;
    let budget = if managed {
        Duration::from_secs(8)
    } else {
        Duration::from_secs(2)
    };
    wait_until_within(budget, || {
        let candidate = open();
        let meta = candidate.metadata().unwrap();
        assert_eq!((meta.dev(), meta.ino()), identity, "writer lock inode changed");
        match candidate.try_lock() {
            Ok(()) => {
                if managed && !protected_native_reads_stopped(&root.join("owners"), &lock_path, identity).unwrap() {
                    if !reported_refusal {
                        fs::write(root.join("managed-contender-refused"), b"registry refused real EX").unwrap();
                        reported_refusal = true;
                    }
                    return false;
                }
                acquired = Some(candidate);
                true
            }
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(error) => panic!("contender lock failed: {error}"),
        }
    });
    let file = acquired.unwrap();
    // These observations FOLLOW the lock call; the later marker/log cannot
    // establish the exact acquisition instant or every scheduling interleaving.
    let mut observation = Observation {
        group_present: crate::process_group::group_present(process.process_group).unwrap(),
        worker_live: process.is_live().unwrap(),
        managed_writer_ready: false,
        payload_bytes_after_marker: 0,
        lock_identity: identity,
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
    let record = root.join("owners").join(format!("worker-{NONCE}")).join("owner.json");
    let tested_zombie_closure = root.join("hold-zombie-custody").exists()
        && read_owner(&record).is_ok_and(|owner| field(&owner, "guardian_zombie_closure") == true);
    if crate::process_group::group_present(worker)? && !tested_zombie_closure {
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

#[test]
fn replacement_requests_drain_from_original_guardian_without_taking_custody() {
    assert_requested_zombie_closure(false, false);
}

#[test]
fn surviving_leaf_prevents_zombie_closure_until_original_guardian_drains_it() {
    assert_requested_zombie_closure(true, false);
}

#[test]
fn stopped_sender_does_not_delay_slot_guardian_or_same_inode_successor() {
    assert_requested_zombie_closure(false, true);
}

#[test]
fn fixture_slot_sender() {
    let Some(root) = std::env::var_os("LEAN_RS_TEST_DRAIN_SLOT_SENDER").map(PathBuf::from) else {
        return;
    };
    let registry = root.join("owners");
    let record = registry.join(format!("worker-{NONCE}/owner.json"));
    let binding = slot_binding(&registry, &read_owner(&record).unwrap());
    submit_original_guardian_drain(&registry, &binding).unwrap();
}

fn slot_binding(registry: &Path, record: &Value) -> NativeGuardianDrainBinding {
    let metadata = fs::metadata(registry).unwrap();
    NativeGuardianDrainBinding {
        registry_device: metadata.dev(),
        registry_inode: metadata.ino(),
        generation: record["generation_id"].as_str().unwrap().to_owned(),
        boot: record["original_worker_boot"].as_str().unwrap().to_owned(),
        parent: serde_json::from_value(record["parent"].clone()).unwrap(),
        worker: serde_json::from_value(record["process"].clone()).unwrap(),
        guardian: serde_json::from_value(record["guardian"].clone()).unwrap(),
    }
}

#[test]
fn fixture_collect_original_closure() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let ready = root.join(format!("closure-collector-ready-{}", std::process::id()));
    fs::write(ready, b"ready to collect").unwrap();
    wait_until(|| root.join("closure-collectors-go").exists());
    collect_closed_owner_records(&root.join("owners"), Duration::from_secs(1)).unwrap();
}

fn assert_requested_zombie_closure(hold_leaf: bool, use_slot: bool) {
    let fixture = Fixture::new();
    for marker in [
        "reap-after-recovery-request",
        "managed-contender",
        "service-loss-watchdog",
        "hold-zombie-custody",
        "recovery-slot",
    ] {
        fs::write(fixture.root.join(marker), b"test-owned recovery request").unwrap();
    }
    let mut service = fixture_command(&fixture.root, "native_guardian::tests::fixture_server");
    service.stderr(Stdio::inherit());
    if use_slot {
        service.env(DRAIN_SLOT_ENV, "1");
        fs::write(fixture.root.join("hold-slot-advertisement"), b"test startup cut").unwrap();
    }
    let mut server = TestChild(service.spawn().unwrap());
    if use_slot {
        wait_until(|| fixture.root.join("slot-advertisement-held").exists());
        let record = fixture.registry.join(format!("worker-{NONCE}/owner.json"));
        let _lock = ownership::lock_publication(&record, Duration::from_secs(1)).unwrap();
        let unadvertised = read_owner(&record).unwrap();
        assert_eq!(unadvertised["phase"], "running");
        assert!(unadvertised.get("guardian_drain").is_none());
        assert!(unadvertised.get("guardian_drain_protocol").is_none());
        let binding = slot_binding(&fixture.registry, &unadvertised);
        let budget = Duration::from_millis(100);
        // Without the pre-lock advertisement check these would time out on our
        // held lock instead of refusing the unadvertised generation.
        let plain = request_original_guardian_drain(&record, &binding.worker, budget).unwrap_err();
        let bound = request_bound_original_guardian_drain(&fixture.registry, &binding, budget).unwrap_err();
        for error in [plain, bound] {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "original guardian did not advertise legacy drain");
        }
        assert_eq!(read_owner(&record).unwrap(), unadvertised);
        fs::write(fixture.root.join("release-slot-advertisement"), b"allow startup").unwrap();
    }
    wait_until(|| fixture.root.join("server-ready").exists());
    let ready: Value = serde_json::from_slice(&fs::read(fixture.root.join("server-ready")).unwrap()).unwrap();
    let worker = pid(field(&ready, "worker")).unwrap();
    let process = NativeProcessIdentity::capture(worker).unwrap().unwrap();
    let guardian: NativeProcessIdentity = serde_json::from_value(field(&ready, "guardian").clone()).unwrap();
    let requester = NativeProcessIdentity::capture(server.0.id()).unwrap().unwrap();
    let record = fixture.registry.join(format!("worker-{NONCE}")).join("owner.json");
    let original = read_owner(&record).unwrap();
    assert_eq!(original["guardian_drain"], "available");
    let expected = slot_binding(&fixture.registry, &original);
    if use_slot {
        assert!(request_original_guardian_drain(&record, &process, Duration::from_secs(1)).is_err());
        assert!(request_bound_original_guardian_drain(&fixture.registry, &expected, Duration::from_secs(1)).is_err());
        submit_original_guardian_drain(&fixture.registry, &expected).unwrap();
        wait_until(|| {
            observe_original_guardian_drain(&fixture.registry, &expected).unwrap() == NativeGuardianDrainStatus::Refused
        });
    } else {
        assert_eq!(
            request_original_guardian_drain(&record, &process, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
        );
    }
    let lock = fs::metadata(fixture.lock()).unwrap();
    let writer = Contender::start(&fixture, worker);

    // Only the harness’s own unreaped service Child receives fixture signals.
    assert!(server.0.try_wait().unwrap().is_none());
    let target = rustix::process::Pid::from_raw(i32::try_from(server.0.id()).unwrap()).unwrap();
    rustix::process::kill_process(target, rustix::process::Signal::STOP).unwrap();
    wait_until(|| stopped(&requester));
    let mut stale = process.clone();
    "wrong-recovery-birth".clone_into(&mut stale.start);
    assert!(request_original_guardian_drain(&record, &stale, Duration::from_secs(1)).is_err());
    for changed in [
        NativeGuardianDrainBinding {
            registry_device: expected.registry_device ^ 1,
            ..expected.clone()
        },
        NativeGuardianDrainBinding {
            registry_inode: expected.registry_inode ^ 1,
            ..expected.clone()
        },
        NativeGuardianDrainBinding {
            generation: format!("{:048x}", 800),
            ..expected.clone()
        },
        NativeGuardianDrainBinding {
            boot: "00000000-0000-4000-8000-000000000000".to_owned(),
            ..expected.clone()
        },
        NativeGuardianDrainBinding {
            parent: process.clone(),
            ..expected.clone()
        },
        NativeGuardianDrainBinding {
            worker: stale,
            ..expected.clone()
        },
        NativeGuardianDrainBinding {
            guardian: requester.clone(),
            ..expected.clone()
        },
    ] {
        if use_slot {
            assert!(submit_original_guardian_drain(&fixture.registry, &changed).is_err());
        } else {
            assert!(
                request_bound_original_guardian_drain(&fixture.registry, &changed, Duration::from_secs(1)).is_err()
            );
        }
        assert_eq!(read_owner(&record).unwrap(), original);
    }
    let before = fixture.heartbeat();
    wait_until_within(Duration::from_millis(200), || fixture.heartbeat() > before);
    assert!(guardian.is_live().unwrap());
    assert!(!fixture.root.join("contender-acquired.json").exists());

    let signal_barrier = fixture.root.join("hold-requested-group-signal");
    if hold_leaf {
        fs::write(&signal_barrier, b"hold actual group signal for negative control").unwrap();
    }
    let mut sender = if use_slot {
        let mut sender = TestChild(
            fixture_command(
                &fixture.root,
                "native_guardian::tests::service_loss_regressions::fixture_slot_sender",
            )
            .env("LEAN_RS_TEST_DRAIN_SLOT_SENDER", &fixture.root)
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
        );
        let exact = NativeProcessIdentity::capture(sender.0.id()).unwrap().unwrap();
        wait_until(|| fixture.root.join("slot-sender-inserted").exists() && stopped(&exact));
        assert!(sender.0.try_wait().unwrap().is_none());
        submit_original_guardian_drain(&fixture.registry, &expected).unwrap();
        Some((sender, exact))
    } else {
        request_bound_original_guardian_drain(&fixture.registry, &expected, Duration::from_secs(1)).unwrap();
        assert_eq!(read_owner(&record).unwrap()["guardian_drain"], "requested");
        None
    };
    if hold_leaf {
        fs::write(fixture.root.join("finish-root"), b"leave real leaf alive").unwrap();
        wait_until_within(Duration::from_secs(1), || {
            ownership::matches_zombie_birth(&process).unwrap()
        });
        assert!(!crate::process_group::observed_group_singleton(worker).unwrap());
        let before = fixture.heartbeat();
        wait_until_within(Duration::from_millis(200), || fixture.heartbeat() > before);
        assert!(read_owner(&record).unwrap().get("guardian_zombie_closure").is_none());
        assert!(!fixture.root.join("contender-acquired.json").exists());
        fs::remove_file(signal_barrier).unwrap();
    }
    wait_until_within(Duration::from_secs(1), || {
        fixture.root.join("zombie-custody-held").exists()
    });
    assert!(stopped(&requester));
    if use_slot {
        assert_eq!(
            observe_original_guardian_drain(&fixture.registry, &expected).unwrap(),
            NativeGuardianDrainStatus::Accepted,
        );
    }
    assert!(guardian.is_live().unwrap());
    assert!(ownership::matches_zombie_birth(&process).unwrap());
    assert!(crate::process_group::observed_group_singleton(worker).unwrap());
    assert_eq!(read_owner(&record).unwrap()["guardian_zombie_closure"], true);
    assert!(!fixture.root.join("contender-acquired.json").exists());
    let next_nonce = format!("{:048x}", 701);
    let reserve = || {
        reserve_owner_record_with_slot(
            &fixture.registry,
            &next_nonce,
            Duration::from_secs(1),
            Some(&recovery_slot()),
        )
    };
    assert_eq!(reserve().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    fs::write(fixture.root.join("release-zombie-custody"), b"release actual SH").unwrap();

    let acquired = writer.acquired();
    assert!(acquired.group_present, "unreaped worker remains in its group");
    assert!(!acquired.worker_live);
    assert!(acquired.managed_writer_ready);
    assert_eq!(acquired.lock_identity, (lock.dev(), lock.ino()));
    if use_slot {
        let spawn = || {
            TestChild(
                fixture_command(
                    &fixture.root,
                    "native_guardian::tests::service_loss_regressions::fixture_collect_original_closure",
                )
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
            )
        };
        let mut first = spawn();
        let mut second = spawn();
        wait_until(|| {
            [first.0.id(), second.0.id()]
                .into_iter()
                .all(|pid| fixture.root.join(format!("closure-collector-ready-{pid}")).exists())
        });
        fs::write(fixture.root.join("closure-collectors-go"), b"collect concurrently").unwrap();
        assert!(first.wait().success() && second.wait().success());
        assert!(stopped(&requester));
        let mut wrong = expected.clone();
        wrong.worker.start.push_str("-changed");
        assert!(observe_original_guardian_closure(&fixture.registry, &wrong, Duration::from_secs(1)).is_err());
        assert!(observe_original_guardian_closure(&fixture.registry, &expected, Duration::from_secs(1)).unwrap());
    }
    assert!(reserve().unwrap().exists());
    assert!(record.exists());
    assert!(
        stopped(&requester),
        "writer and successor must be admitted before resuming the parent"
    );
    assert_eq!(writer.finish().payload_bytes_after_marker, 0);
    if let Some((mut sender, exact)) = sender.take() {
        assert!(
            stopped(&exact),
            "cleanup and reuse completed while submitter was stopped before return"
        );
        sender.0.kill().unwrap();
        assert_eq!(sender.wait().signal(), Some(libc::SIGKILL));
    }
    assert!(!guardian.is_live().unwrap());
    assert!(!fixture.root.join("test-rescue-used").exists());
    fs::write(fixture.root.join("recovery-request-test-finished"), b"closed").unwrap();
    rustix::process::kill_process(target, rustix::process::Signal::CONT).unwrap();
    assert!(server.wait().success());
    assert!(!crate::process_group::group_present(worker).unwrap());
    assert_eq!(
        collect_closed_owner_records(&fixture.registry, Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock, // the independently reserved successor is still open
    );
    assert!(!record.exists());
    assert_eq!(
        observe_original_guardian_closure(&fixture.registry, &expected, Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
    );
}

pub(super) fn recovery_slot() -> NativeCapacitySlot {
    NativeCapacitySlot {
        key: "frozen-recoverer".to_owned(),
        scope: "main".to_owned(),
        limit: 1,
    }
}

#[test]
fn fixture_recovery_claimant() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let record = root.join("owners").join(format!("worker-{NONCE}")).join("owner.json");
    let owner = read_owner(&record).unwrap();
    let worker: NativeProcessIdentity = serde_json::from_value(field(&owner, "process").clone()).unwrap();
    if root.join("require-live-guardian-refusal").exists() {
        assert_eq!(
            claim_native_recovery(&record, &worker, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
        );
        return;
    }
    if root.join("claim-race").exists() {
        let id = std::process::id();
        let identity = NativeProcessIdentity::capture(id).unwrap().unwrap();
        fs::write(root.join(format!("claimant-ready-{id}")), b"actual contender").unwrap();
        wait_until(|| root.join("claim-race-go").exists());
        let winner = match claim_native_recovery(&record, &worker, Duration::from_secs(1)) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("claim race failed: {error}"),
        };
        write_json(
            &root,
            &format!("claimant-result-{id}.json"),
            &json!({"winner":winner,"identity":identity}),
        );
        wait_until(|| root.join("claimant-finish").exists());
        return;
    }
    claim_native_recovery(&record, &worker, Duration::from_secs(1)).unwrap();
    fs::write(
        root.join("claimant-published"),
        b"claim committed after releasing admission lock",
    )
    .unwrap();
    // The controller owns this actual Child and reaps it on every exit.
    wait_until_within(Duration::from_secs(10), || root.join("claimant-finish").exists());
}

fn lose_original_cleanup_owners(fixture: &Fixture, server: &mut TestChild, guardian: &NativeProcessIdentity) {
    // The worker kills and reaps its actual guardian child. The controller
    // holds and signals only its own actual service child.
    fs::write(fixture.root.join("kill-guardian"), b"request to actual Child owner").unwrap();
    wait_until(|| fixture.root.join("guardian-reaped").exists());
    assert!(!guardian.is_live().unwrap());
    server.0.kill().unwrap();
    assert_eq!(server.wait().signal(), Some(libc::SIGKILL));
}

#[test]
fn distinct_replacement_processes_race_for_one_durable_claim() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("claim-race"), b"two actual contenders").unwrap();
    let mut server = TestChild(
        fixture_command(&fixture.root, "native_guardian::tests::fixture_server")
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    wait_until(|| fixture.root.join("server-ready").exists());
    let ready: Value = serde_json::from_slice(&fs::read(fixture.root.join("server-ready")).unwrap()).unwrap();
    let worker = pid(field(&ready, "worker")).unwrap();
    let guardian: NativeProcessIdentity = serde_json::from_value(field(&ready, "guardian").clone()).unwrap();
    let record = fixture.registry.join(format!("worker-{NONCE}")).join("owner.json");
    lose_original_cleanup_owners(&fixture, &mut server, &guardian);
    let spawn_contender = || {
        TestChild(
            fixture_command(
                &fixture.root,
                "native_guardian::tests::service_loss_regressions::fixture_recovery_claimant",
            )
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
        )
    };
    let mut first = spawn_contender();
    let mut second = spawn_contender();
    let ids = [first.0.id(), second.0.id()];
    assert_ne!(ids[0], ids[1]);
    wait_until(|| {
        ids.iter()
            .all(|id| fixture.root.join(format!("claimant-ready-{id}")).exists())
    });
    fs::write(fixture.root.join("claim-race-go"), b"race now").unwrap();
    let result = |id| {
        let path = fixture.root.join(format!("claimant-result-{id}.json"));
        wait_until(|| path.exists());
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let actual = NativeProcessIdentity::capture(id).unwrap().unwrap();
        assert_eq!(value["identity"], json!(actual));
        (value["winner"].as_bool().unwrap(), actual)
    };
    let (first_won, first_identity) = result(ids[0]);
    let (second_won, second_identity) = result(ids[1]);
    assert_ne!(first_won, second_won, "one claimant must be refused");
    assert!(first.0.try_wait().unwrap().is_none());
    assert!(second.0.try_wait().unwrap().is_none());
    let winner = if first_won { first_identity } else { second_identity };
    let persisted = read_owner(&record).unwrap();
    assert_eq!(persisted["recovery_claim"]["claimant"], json!(winner));
    assert!(!record.with_extension("recovery-write").exists());

    fs::write(fixture.root.join("claimant-finish"), b"both contenders observed").unwrap();
    assert!(first.wait().success());
    assert!(second.wait().success());
    // There is still no stop authority after both control owners are lost.
    // Wait for the disposable worker and leaf's natural exit before collection.
    wait_until(|| !crate::process_group::group_present(worker).unwrap());
    collect_closed_owner_records(&fixture.registry, Duration::from_secs(1)).unwrap();
    assert!(!record.exists());
}

#[test]
fn frozen_claimant_cannot_release_live_scope_and_passive_closure_still_retires_it() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("recovery-slot"), b"one actor slot").unwrap();
    fs::write(fixture.root.join("managed-contender"), b"managed writer gate").unwrap();
    let mut server = TestChild(
        fixture_command(&fixture.root, "native_guardian::tests::fixture_server")
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    wait_until(|| fixture.root.join("server-ready").exists());
    let ready: Value = serde_json::from_slice(&fs::read(fixture.root.join("server-ready")).unwrap()).unwrap();
    let worker = pid(field(&ready, "worker")).unwrap();
    let process = NativeProcessIdentity::capture(worker).unwrap().unwrap();
    let guardian: NativeProcessIdentity = serde_json::from_value(field(&ready, "guardian").clone()).unwrap();
    let record = fixture.registry.join(format!("worker-{NONCE}")).join("owner.json");
    let lock = fs::metadata(fixture.lock()).unwrap();
    let writer = Contender::start(&fixture, worker);

    lose_original_cleanup_owners(&fixture, &mut server, &guardian);
    let mut claimant = TestChild(
        fixture_command(
            &fixture.root,
            "native_guardian::tests::service_loss_regressions::fixture_recovery_claimant",
        )
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap(),
    );
    wait_until(|| fixture.root.join("claimant-published").exists());
    let exact = NativeProcessIdentity::capture(claimant.0.id()).unwrap().unwrap();
    assert!(claimant.0.try_wait().unwrap().is_none());
    let target = rustix::process::Pid::from_raw(i32::try_from(claimant.0.id()).unwrap()).unwrap();
    rustix::process::kill_process(target, rustix::process::Signal::STOP).unwrap();
    wait_until(|| stopped(&exact));

    wait_until(|| fixture.root.join("managed-contender-refused").exists());
    let published = read_owner(&record).unwrap();
    assert_eq!(published["recovery_claim"]["claimant"], json!(exact));
    let before = fixture.heartbeat();
    wait_until_within(Duration::from_millis(200), || fixture.heartbeat() > before);
    assert_eq!(
        claim_native_recovery(&record, &process, Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock,
    );
    let next_nonce = format!("{:048x}", 700);
    let reserve = || {
        reserve_owner_record_with_slot(
            &fixture.registry,
            &next_nonce,
            Duration::from_secs(1),
            Some(&recovery_slot()),
        )
    };
    assert_eq!(reserve().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(read_owner(&record).unwrap(), published);
    assert!(!fixture.root.join("contender-acquired.json").exists());

    // The unowned worker and leaf expire on their own fixture deadlines.
    // A claim provides no means to stop that inherited group.
    let acquired = writer.acquired();
    assert!(stopped(&exact), "claimant must still be frozen at passive closure");
    assert!(!acquired.group_present && !acquired.worker_live);
    assert!(acquired.managed_writer_ready);
    assert_eq!(acquired.lock_identity, (lock.dev(), lock.ino()));
    let replacement = reserve().unwrap();
    assert!(!record.exists());
    assert!(replacement.exists());
    assert_eq!(writer.finish().payload_bytes_after_marker, 0);
    claimant.0.kill().unwrap();
    assert_eq!(claimant.wait().signal(), Some(libc::SIGKILL));
}

#[derive(Clone, Copy)]
pub(super) enum Freeze {
    None,
    Worker,
    Service,
}

pub(super) fn assert_service_loss(freeze: Freeze) {
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
    let frozen = match freeze {
        Freeze::None => None,
        Freeze::Worker => Some(process),
        Freeze::Service => Some(NativeProcessIdentity::capture(server.0.id()).unwrap().unwrap()),
    };
    if let Some(frozen) = &frozen {
        assert!(server.0.try_wait().unwrap().is_none());
        // The controller owns the service Child, and the live service owns the
        // worker Child. Neither PID is signalled after its owner is lost.
        assert_eq!(
            NativeProcessIdentity::capture(frozen.pid).unwrap().as_ref(),
            Some(frozen)
        );
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(i32::try_from(frozen.pid).unwrap()).unwrap(),
            rustix::process::Signal::STOP,
        )
        .unwrap();
        wait_until(|| stopped(frozen));
        assert!(guardian.is_live().unwrap());
        assert!(crate::process_group::group_present(worker).unwrap());
        assert!(!fixture.root.join("contender-acquired.json").exists());
        if matches!(freeze, Freeze::Service) {
            let before = fixture.heartbeat();
            wait_until_within(Duration::from_millis(200), || fixture.heartbeat() > before);
            assert!(stopped(frozen));
            assert!(!fixture.root.join("contender-acquired.json").exists());
        }
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

#[test]
#[ignore = "requires a short private checkout-local fixture root"]
fn dead_service_leaves_only_original_guardian_in_charge_of_live_scope() {
    assert!(
        std::env::var_os("LEAN_RS_GUARDIAN_TEST_ROOT").is_some(),
        "supply an owned test root"
    );
    let fixture = Fixture::new();
    for marker in [
        "service-loss-watchdog",
        "hold-service-loss-signal",
        "managed-contender",
        "recovery-slot",
        "require-live-guardian-refusal",
    ] {
        fs::write(fixture.root.join(marker), b"test-owned service loss").unwrap();
    }
    let mut server = TestChild(
        fixture_command(&fixture.root, "native_guardian::tests::fixture_server")
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    wait_until(|| fixture.root.join("server-ready").exists());
    let ready: Value = serde_json::from_slice(&fs::read(fixture.root.join("server-ready")).unwrap()).unwrap();
    let worker = pid(field(&ready, "worker")).unwrap();
    let guardian: NativeProcessIdentity = serde_json::from_value(field(&ready, "guardian").clone()).unwrap();
    let record = fixture.registry.join(format!("worker-{NONCE}")).join("owner.json");
    let original = read_owner(&record).unwrap();
    let parent: NativeProcessIdentity = serde_json::from_value(field(&original, "parent").clone()).unwrap();
    let metadata = fs::metadata(fixture.lock()).unwrap();
    let writer = Contender::start(&fixture, worker);
    server.0.kill().unwrap();
    assert_eq!(server.wait().signal(), Some(libc::SIGKILL));
    wait_until(|| fixture.root.join("service-loss-signal-held").exists());
    assert!(!parent.is_live().unwrap());
    assert!(guardian.is_live().unwrap());
    assert!(crate::process_group::group_present(worker).unwrap());
    let before = fixture.heartbeat();
    wait_until_within(Duration::from_millis(200), || fixture.heartbeat() > before);

    let mut rival = TestChild(
        fixture_command(
            &fixture.root,
            "native_guardian::tests::service_loss_regressions::fixture_recovery_claimant",
        )
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap(),
    );
    assert!(rival.wait().success());
    let reserve = |nonce| {
        reserve_owner_record_with_slot(&fixture.registry, nonce, Duration::from_secs(1), Some(&recovery_slot()))
    };
    let next = format!("{:048x}", 704);
    let another = format!("{:048x}", 705);
    assert_eq!(reserve(&next).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(read_owner(&record).unwrap(), original);
    assert!(guardian.is_live().unwrap());
    assert!(!fixture.root.join("contender-acquired.json").exists());
    fs::remove_file(fixture.root.join("hold-service-loss-signal")).unwrap();

    let acquired = writer.acquired();
    assert!(!acquired.group_present && !acquired.worker_live && acquired.managed_writer_ready);
    assert_eq!(acquired.lock_identity, (metadata.dev(), metadata.ino()));
    wait_until(|| !guardian.is_live().unwrap());
    assert!(fixture.root.join("guardian-returned").exists());
    assert!(!fixture.root.join("test-rescue-used").exists());
    assert!(reserve(&next).unwrap().exists());
    assert_eq!(reserve(&another).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(writer.finish().payload_bytes_after_marker, 0);
}

#[test]
fn fixture_deadline_server() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let mut generation = Generation::start(&root);
    write_json(&root, "deadline-ready", &generation.worker.id());
    wait_until(|| root.join("arm-deadline").exists());
    let expires_ns = u64::try_from(
        deadlines::boot_ticks()
            .unwrap()
            .saturating_add(Duration::from_millis(750))
            .as_nanos(),
    )
    .unwrap();
    let parent = generation.parent.as_mut().unwrap();
    let attempt = [7_u8; 16];
    // Split the actual wire message across reads.
    write_bytes(parent, b"{", deadline(Duration::from_secs(1)).unwrap()).unwrap();
    thread::sleep(POLL.saturating_mul(3));
    let arm = frame(&json!({
        "op":"arm_hard_deadline","protocol":PROTOCOL,"nonce":NONCE,
        "attempt":attempt,"stage":1,"expires_ns":expires_ns
    }))
    .unwrap();
    let (_, arm_tail) = arm.split_first().unwrap();
    write_bytes(parent, arm_tail, deadline(Duration::from_secs(1)).unwrap()).unwrap();
    let ack = read_frame(parent, deadline(Duration::from_secs(1)).unwrap()).unwrap();
    assert_eq!(
        ack,
        json!({
            "state":"hard_deadline_armed","protocol":PROTOCOL,"nonce":NONCE,
            "attempt":attempt,"stage":1,"expires_ns":expires_ns
        })
    );
    write_json(&root, "deadline-armed", &expires_ns);
    assert_eq!(generation.reap().signal(), Some(libc::SIGKILL));
    wait_until(|| root.join("deadline-test-finished").exists());
}

#[test]
fn production_guardian_drains_at_deadline_with_requester_stopped() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("service-loss-watchdog"), b"test-only rescue").unwrap();
    let server = TestChild(
        fixture_command(
            &fixture.root,
            "native_guardian::tests::service_loss_regressions::fixture_deadline_server",
        )
        .spawn()
        .unwrap(),
    );
    wait_until(|| fixture.root.join("deadline-ready").exists());
    let worker: u32 = serde_json::from_slice(&fs::read(fixture.root.join("deadline-ready")).unwrap()).unwrap();
    let payload = NativeProcessIdentity::capture(worker).unwrap().unwrap();
    let contender = Contender::start(&fixture, worker);
    fs::write(fixture.root.join("arm-deadline"), b"arm").unwrap();
    wait_until(|| fixture.root.join("deadline-armed").exists());
    let expires: u64 = serde_json::from_slice(&fs::read(fixture.root.join("deadline-armed")).unwrap()).unwrap();
    let requester = NativeProcessIdentity::capture(server.0.id()).unwrap().unwrap();
    let requester_pid = rustix::process::Pid::from_raw(i32::try_from(requester.pid).unwrap()).unwrap();
    rustix::process::kill_process(requester_pid, rustix::process::Signal::STOP).unwrap();
    wait_until(|| stopped(&requester));
    assert!(deadlines::boot_ticks().unwrap() < Duration::from_nanos(expires));
    assert!(crate::process_group::group_present(worker).unwrap());
    assert!(!fixture.root.join("contender-acquired.json").exists());
    wait_until_within(Duration::from_secs(2), || !payload.is_live().unwrap());
    assert!(deadlines::boot_ticks().unwrap() >= Duration::from_nanos(expires));
    assert!(stopped(&requester), "requester must still hold the open lifeline");
    // The stopped parent cannot reap its exited worker. Custody must
    // remain closed to competing writers until the census clears.
    assert!(!fixture.root.join("contender-acquired.json").exists());
    rustix::process::kill_process(requester_pid, rustix::process::Signal::CONT).unwrap();
    let acquired = contender.acquired();
    assert!(!acquired.group_present && !acquired.worker_live, "{acquired:?}");
    assert!(acquired.managed_writer_ready, "{acquired:?}");
    assert_eq!(contender.finish().payload_bytes_after_marker, 0);
    wait_until(|| fixture.root.join("guardian-returned").exists());
    assert!(!fixture.root.join("test-rescue-used").exists());
    assert!(!fixture.root.join("guardian-error").exists());
    fs::write(fixture.root.join("deadline-test-finished"), b"done").unwrap();
    let mut server = server;
    assert!(server.wait().success());
}

#[test]
fn guardian_deadline_rejects_malformed_and_duplicate_arm() {
    let (mut parent, mut guardian) = UnixStream::pair().unwrap();
    guardian.set_nonblocking(true).unwrap();
    let mut state = deadlines::OwnerDeadline::default();
    parent.write_all(b"{\"op\":\"arm_hard_deadline\"}\n").unwrap();
    assert!(state.poll_parent(&mut guardian, NONCE).is_err());
    assert!(!state.armed(), "malformed request cannot authorize GO");

    let progress = frame(&json!({
        "op": "progress", "protocol": PROTOCOL, "nonce": NONCE,
        "attempt": ([7_u8; 16]), "stage": 1, "sequence": 1,
    }))
    .unwrap();
    let mut state = deadlines::OwnerDeadline::default();
    parent.write_all(&progress).unwrap();
    assert!(state.poll_parent(&mut guardian, NONCE).is_err());
    assert!(!state.armed(), "caller progress cannot authorize GO");

    let mut state = deadlines::OwnerDeadline::default();
    let expires = deadlines::boot_ticks()
        .unwrap()
        .saturating_add(Duration::from_millis(100));
    let request = |expires: Duration| {
        json!({
            "op": "arm_hard_deadline", "protocol": PROTOCOL, "nonce": NONCE,
            "attempt": ([7_u8; 16]), "stage": 1,
            "expires_ns": u64::try_from(expires.as_nanos()).unwrap(),
        })
    };
    parent.write_all(&frame(&request(expires)).unwrap()).unwrap();
    assert!(!state.poll_parent(&mut guardian, NONCE).unwrap());
    parent.set_nonblocking(true).unwrap();
    let ack = read_frame(&mut parent, deadline(Duration::from_secs(1)).unwrap()).unwrap();
    assert_eq!(field(&ack, "state"), "hard_deadline_armed");

    let extension = expires.saturating_add(Duration::from_secs(60));
    parent.write_all(&frame(&request(extension)).unwrap()).unwrap();
    assert!(state.poll_parent(&mut guardian, NONCE).is_err());
    assert_eq!(parent.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    parent.write_all(&progress).unwrap();
    assert!(state.poll_parent(&mut guardian, NONCE).is_err());
    assert_eq!(parent.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    wait_until_within(Duration::from_secs(1), || state.expired().unwrap());
}
