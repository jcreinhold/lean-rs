//! Real post-drain SH holder: metadata lifetime is separate from compute use.
use super::*;

const REQUEST: &str = "pause-guardian-after-payload";
const READY: &str = "guardian-payload-closed.json";
const RELEASE: &str = "release-guardian-after-payload";
const RELEASED: &str = "guardian-drain-released";
const RETURNED: &str = "guardian-cli-returned";
const RESCUE: &str = "guardian-drain-safety-used";
const BARRIER_LIMIT: Duration = Duration::from_secs(5);

pub(super) fn pause_after_payload_closed(guardian: &Guardian) {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    if !root.join(REQUEST).exists() {
        return;
    }
    assert!(!crate::process_group::group_present(guardian.worker).unwrap());
    assert!(!guardian.loans.is_empty(), "barrier must retain actual loan files");
    let actual = NativeProcessIdentity::capture(std::process::id()).unwrap().unwrap();
    let temporary = root.join(format!("{READY}.pending"));
    fs::write(
        &temporary,
        serde_json::to_vec(&json!({"guardian":actual,"worker":guardian.worker,"loan_count":guardian.loans.len()}))
            .unwrap(),
    )
    .unwrap();
    fs::rename(temporary, root.join(READY)).unwrap();
    let until = Instant::now() + BARRIER_LIMIT;
    while !root.join(RELEASE).exists() {
        if Instant::now() >= until {
            // Failure-only rescue after the entire payload group is gone.
            // No extra File alias, process or stored-PID signal is introduced.
            let _marker = fs::write(root.join(RESCUE), b"not a passing release");
            let _stop = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
            panic!("guardian record test barrier exceeded safety deadline");
        }
        thread::sleep(POLL);
    }
    fs::write(root.join(RELEASED), b"normal test barrier release").unwrap();
}

pub(super) fn record_cli_return(root: &Path) {
    if root.join(REQUEST).exists() {
        fs::write(root.join(RETURNED), b"actual CLI returned after dropping loans").unwrap();
    }
}

// Paths and passive observations only, never a second SH custodian or guardian
// Child. Unwind releases the barrier; a safety release cannot qualify the test.
struct ReleaseOnDrop {
    root: PathBuf,
    guardian: NativeProcessIdentity,
    worker: u32,
    released: bool,
}
impl ReleaseOnDrop {
    fn release(&mut self) {
        fs::write(self.root.join(RELEASE), b"release real post-drain holder").unwrap();
        self.released = true;
    }
}
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let _release = fs::write(self.root.join(RELEASE), b"failed-test barrier release");
        // Do not delay the original actual-child owner if the payload still lives.
        if crate::process_group::group_present(self.worker).is_ok_and(|present| !present) {
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                match self.guardian.is_live() {
                    Ok(false) => return,
                    Ok(true) => thread::sleep(POLL),
                    Err(_) => break,
                }
            }
            eprintln!("failed-test guardian release not observed: {}", self.root.display());
        }
    }
}
fn slot(key: &str) -> NativeCapacitySlot {
    NativeCapacitySlot {
        key: key.to_owned(),
        scope: "main".to_owned(),
        limit: 1,
    }
}
fn reserve(fixture: &Fixture, nonce: &str, key: &str) -> io::Result<PathBuf> {
    reserve_owner_record_with_slot(&fixture.registry, nonce, Duration::from_secs(1), Some(&slot(key)))
}
fn assert_blocked(probe: &File) {
    assert!(
        matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "real guardian must still hold its actual SH descriptor"
    );
}
fn assert_retained_namespace_bound(fixture: &Fixture, original: &Value) {
    let registry = fixture.root.join("retained-record-bound");
    fs::DirBuilder::new().mode(0o700).create(&registry).unwrap();
    // Synthetic records reuse this one REAL live guardian. This bounds stored
    // metadata; it does not claim 128 actual processes or custody transfers.
    for index in 0..MAX_OWNER_RECORDS {
        let nonce = format!("{:048x}", index + 1000);
        let directory = registry.join(format!("worker-{nonce}"));
        fs::DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let mut value = original.clone();
        value["generation_id"] = json!(nonce);
        persist_owner(&directory.join("owner.json"), &value).unwrap();
    }
    let nonce = format!("{:048x}", MAX_OWNER_RECORDS + 1000);
    let error = reserve_owner_record_with_slot(&registry, &nonce, Duration::from_secs(2), Some(&slot("new-actor")))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(error.to_string().contains("generation capacity"), "{error}");
    assert!(!registry.join(format!("worker-{nonce}")).exists());
    assert_eq!(
        fs::read_dir(&registry)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .count(),
        MAX_OWNER_RECORDS,
        "retained live custodians must still count toward the namespace bound"
    );
}

#[test]
fn closed_payload_keeps_guardian_record_without_occupying_compute() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join(REQUEST), b"test-only pause, not SIGSTOP").unwrap();
    let mut generation = Generation::start_with_slot(&fixture.root, Some(&slot("old-actor")));
    let mut release = ReleaseOnDrop {
        root: fixture.root.clone(),
        guardian: generation.guardian.clone(),
        worker: generation.worker.id(),
        released: false,
    };
    let original_bytes = fs::read(&generation.record).unwrap();
    let original = read_owner(&generation.record).unwrap();
    assert_eq!(original["capacity_slot"], json!(slot("old-actor")));
    let domains = original["protected_domains"].as_array().unwrap();
    assert_eq!(domains.len(), 1, "fixture's repeated loans must publish its full union");
    let candidate_nonce = format!("{:048x}", 100);
    let blocked = reserve(&fixture, &candidate_nonce, "replacement-actor").unwrap_err();
    assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
    assert!(!fixture.registry.join(format!("worker-{candidate_nonce}")).exists());
    assert_eq!(generation.kill_root().signal(), Some(libc::SIGKILL));
    wait_until_within(Duration::from_secs(2), || fixture.root.join(READY).exists());
    let entered = Instant::now();
    let ready: Value = serde_json::from_slice(&fs::read(fixture.root.join(READY)).unwrap()).unwrap();
    assert_eq!(ready["worker"], generation.worker.id());
    assert_eq!(ready["guardian"], json!(generation.guardian));
    assert_eq!(ready["loan_count"], 1);
    assert!(!crate::process_group::group_present(generation.worker.id()).unwrap());
    assert!(generation.guardian.is_live().unwrap());
    let probe = OpenOptions::new().read(true).write(true).open(fixture.lock()).unwrap();
    let inode = probe.metadata().unwrap();
    assert_blocked(&probe);
    let replacement = reserve(&fixture, &candidate_nonce, "replacement-actor").unwrap();
    assert_blocked(&probe);
    eprintln!(
        "post-drain collector: group absent=true, guardian live={}, actual SH blocked=true, old record exists={}, new limit-one reservation={}",
        generation.guardian.is_live().unwrap(),
        generation.record.exists(),
        replacement.exists()
    );
    assert!(
        generation.record.exists(),
        "collector removed a live guardian's recovery record while its SH remained held"
    );
    assert_eq!(fs::read(&generation.record).unwrap(), original_bytes);
    assert_eq!(
        read_owner(&generation.record).unwrap()["protected_domains"],
        original["protected_domains"]
    );
    ownership::retire_preparing_owner_record(&replacement, &candidate_nonce).unwrap();
    assert_retained_namespace_bound(&fixture, &original);
    assert_blocked(&probe);
    assert_eq!(
        (probe.metadata().unwrap().dev(), probe.metadata().unwrap().ino()),
        (inode.dev(), inode.ino())
    );
    release.release();
    generation.closed();
    assert!(fixture.root.join(RELEASED).exists());
    assert!(fixture.root.join(RETURNED).exists());
    assert!(!fixture.root.join(RESCUE).exists());
    assert!(!fixture.root.join("guardian-error").exists());
    assert!(
        entered.elapsed() < BARRIER_LIMIT,
        "safety expiry cannot qualify release"
    );
    probe.try_lock().unwrap();
    let next_nonce = format!("{:048x}", 101);
    let next = reserve(&fixture, &next_nonce, "next-actor").unwrap();
    assert!(
        !generation.record.exists(),
        "closed guardian record should now be collected"
    );
    ownership::retire_preparing_owner_record(&next, &next_nonce).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (inode.dev(), inode.ino())).unwrap());
    eprintln!("normal release: guardian returned/exited, same-inode EX acquired, original record collected; no rescue");
}
