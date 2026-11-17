//! Actor admission uses the same durable Preparing/Running owner record.
use super::*;
use std::sync::{Arc, Barrier};

fn slot(key: &str, scope: &str, limit: usize) -> NativeCapacitySlot {
    NativeCapacitySlot {
        key: key.to_owned(),
        scope: scope.to_owned(),
        limit,
    }
}
fn reserve(fixture: &Fixture, index: usize, slot: &NativeCapacitySlot) -> io::Result<PathBuf> {
    reserve_owner_record_with_slot(
        &fixture.registry,
        &format!("{index:048x}"),
        Duration::from_secs(1),
        Some(slot),
    )
}
fn private_group() -> Child {
    let mut child = Command::new("python3")
        .args([
            "-c",
            "import os,time; os.setsid(); print(os.getpid(),flush=True); time.sleep(6)",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    use std::io::BufRead as _;
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim().parse::<u32>().unwrap(), child.id());
    child
}
fn running(record: &Path, child: &Child) {
    let mut owner = read_owner(record).unwrap();
    owner["phase"] = json!("running");
    owner["process"] = json!(NativeProcessIdentity::capture(child.id()).unwrap().unwrap());
    // Header-only fixture, not physical custody evidence. Collection queries
    // THIS test process with a deliberately mismatched birth, so the synthetic
    // old guardian is absent without looking up an unrelated guessed PID.
    owner["guardian"] = json!(NativeProcessIdentity {
        version: 1,
        pid: std::process::id(),
        start: "fixture-guardian-header".to_owned(),
        session: child.id(),
        process_group: std::process::id(),
    });
    persist_owner(record, &owner).unwrap();
}
fn stopped(child: &mut Child) {
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!crate::process_group::group_present(child.id()).unwrap());
}
#[test]
fn one_actor_admits_two_scopes_without_admitting_a_second_actor_or_replacement() {
    let fixture = Fixture::new();
    let main = reserve(&fixture, 1, &slot("actor-a", "main", 1)).unwrap();
    let semantic = reserve(&fixture, 2, &slot("actor-a", "semantic", 1)).unwrap();
    for (index, requested) in [
        (3, slot("actor-a", "main", 1)),
        (4, slot("actor-b", "main", 1)),
        (5, slot("actor-b", "main", 2)),
    ] {
        assert_eq!(
            reserve(&fixture, index, &requested).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    assert!(main.exists() && semantic.exists());
    assert_eq!(
        read_owner(&main).unwrap()["capacity_slot"],
        json!(slot("actor-a", "main", 1))
    );
}
#[test]
#[allow(
    clippy::needless_collect,
    reason = "launch every concurrent contender before releasing their shared barrier"
)]
fn concurrent_actor_admission_respects_one_durable_slot() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (1..=2)
        .map(|index| {
            let root = fixture.registry.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                reserve_owner_record_with_slot(
                    &root,
                    &format!("{index:048x}"),
                    Duration::from_secs(2),
                    Some(&slot(&format!("actor-{index}"), "main", 1)),
                )
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock))
            .count(),
        1
    );
}
#[test]
fn occupied_actor_key_is_bound_to_exact_parent_birth() {
    let fixture = Fixture::new();
    let record = reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    let mut child = private_group();
    running(&record, &child);
    let mut value = read_owner(&record).unwrap();
    value["parent"]["start"] = json!("previous-parent-incarnation");
    persist_owner(&record, &value).unwrap();
    assert_eq!(
        reserve(&fixture, 2, &slot("actor", "semantic", 1)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        reserve(&fixture, 3, &slot("new-actor", "main", 1)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    stopped(&mut child);
    let replacement = reserve(&fixture, 4, &slot("new-actor", "main", 1)).unwrap();
    assert!(replacement.exists());
    assert!(!record.exists());
}
#[test]
fn legacy_or_unscoped_live_generation_cannot_bypass_actor_quota() {
    let fixture = Fixture::new();
    let record = reserve_owner_record(&fixture.registry, NONCE, Duration::from_secs(1)).unwrap();
    let mut child = private_group();
    running(&record, &child);
    assert_eq!(
        reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let mut value = read_owner(&record).unwrap();
    value["version"] = json!(1);
    value["scope"] = json!("inherited-process-group-v1");
    persist_owner(&record, &value).unwrap();
    assert!(!protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).unwrap());
    assert_eq!(
        reserve(&fixture, 2, &slot("actor", "main", 1)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    stopped(&mut child);
    assert!(reserve(&fixture, 3, &slot("actor", "main", 1)).is_ok());
}
#[test]
fn closed_native_domain_recovers_after_its_checkout_disappears() {
    let fixture = Fixture::new();
    let record = reserve(&fixture, 1, &slot("old-actor", "main", 1)).unwrap();
    let mut child = private_group();
    running(&record, &child);
    let mut value = read_owner(&record).unwrap();
    value["protected_domains"] = json!([NativeReadDomain {
        project_root: fixture.project.clone(),
        lock_path: fixture.lock(),
        device: 1,
        inode: 2,
        epoch: "old".to_owned(),
    }]);
    persist_owner(&record, &value).unwrap();
    let lock_path = fixture.lock();
    fs::remove_dir(&fixture.project).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &lock_path, (1, 2)).is_err());
    stopped(&mut child);
    assert!(protected_native_reads_stopped(&fixture.registry, &lock_path, (1, 2)).unwrap());
    assert!(reserve(&fixture, 2, &slot("new-actor", "main", 1)).is_ok());
    assert!(!record.exists());
}
#[test]
fn admission_mutex_obeys_the_original_budget_without_creating_a_record() {
    let fixture = Fixture::new();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.registry.join(".admission.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let started = Instant::now();
    let error = reserve_owner_record_with_slot(
        &fixture.registry,
        NONCE,
        Duration::from_millis(30),
        Some(&slot("actor", "main", 1)),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_millis(400));
    assert!(!fixture.registry.join(format!("worker-{NONCE}")).exists());
    drop(lock);
    assert!(reserve(&fixture, 1, &slot("actor", "main", 1)).is_ok());
}
#[test]
fn invalid_capacity_or_zero_budget_refuses_before_namespace_creation() {
    let fixture = Fixture::new();
    let root = fixture.root.join("unused-registry");
    for requested in [
        slot("", "main", 1),
        slot("actor", "third", 1),
        slot("actor", "main", 0),
        slot("actor", "main", MAX_OWNER_RECORDS + 1),
    ] {
        assert!(reserve_owner_record_with_slot(&root, NONCE, Duration::from_secs(1), Some(&requested)).is_err());
    }
    assert!(reserve_owner_record_with_slot(&root, NONCE, Duration::ZERO, Some(&slot("actor", "main", 1))).is_err());
    assert!(!root.exists());
}
#[test]
fn malformed_preparing_phase_or_owner_never_releases_capacity() {
    let fixture = Fixture::new();
    let record = reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    let original = read_owner(&record).unwrap();
    for (name, replacement) in [
        ("phase", json!("unknown")),
        ("parent", json!({})),
        ("process", json!({"pid":1})),
    ] {
        let mut owner = original.clone();
        owner[name] = replacement;
        persist_owner(&record, &owner).unwrap();
        assert!(reserve(&fixture, 2, &slot("actor", "semantic", 1)).is_err());
        assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).is_err());
    }
}

#[test]
fn malformed_closed_looking_headers_never_grant_writer_or_capacity_admission() {
    let fixture = Fixture::new();
    let record = reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    let mut child = private_group();
    running(&record, &child);
    let valid = read_owner(&record).unwrap();
    stopped(&mut child);
    for pointer in [
        "/process/start",
        "/parent/start",
        "/guardian/start",
        "/generation_id",
        "/scope",
    ] {
        let mut value = valid.clone();
        *value.pointer_mut(pointer).unwrap() = json!("");
        persist_owner(&record, &value).unwrap();
        assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).is_err());
        assert!(reserve(&fixture, 2, &slot("new-actor", "main", 1)).is_err());
        assert!(record.exists());
    }
    let mut value = valid.clone();
    value.as_object_mut().unwrap().remove("guardian");
    persist_owner(&record, &value).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).is_err());
    persist_owner(&record, &valid).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).unwrap());
    assert!(reserve(&fixture, 2, &slot("new-actor", "main", 1)).is_ok());
}

#[test]
fn interrupted_initial_publication_recovers_before_host_preparation() {
    let fixture = Fixture::new();
    // This is the exact on-disk cut after create_new/write but before the
    // initial owner.json rename. Native spawn has not been permitted yet.
    let directory = fixture.registry.join(format!("worker-{NONCE}"));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("owner.guardian-write"), b"{partial-initial-preparing").unwrap();
    let mutex = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.registry.join(".admission.lock"))
        .unwrap();
    mutex.try_lock().unwrap();
    assert!(!protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).unwrap());
    assert!(directory.exists(), "a live reservation must not be collected");
    drop(mutex);
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).unwrap());
    assert!(!directory.exists());
    assert!(reserve(&fixture, 1, &slot("actor", "main", 1)).is_ok());
}
#[test]
fn interrupted_initial_publication_is_also_collected_during_reservation() {
    let fixture = Fixture::new();
    let directory = fixture.registry.join(format!("worker-{NONCE}"));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("owner.guardian-write"), b"{partial").unwrap();
    assert!(reserve(&fixture, 1, &slot("actor", "main", 1)).is_ok());
    assert!(!directory.exists());
}
#[test]
fn unknown_unpublished_files_and_malformed_committed_owners_stay_fenced() {
    let fixture = Fixture::new();
    let directory = fixture.registry.join(format!("worker-{NONCE}"));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("unrecognized"), b"unknown").unwrap();
    let mutex = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.registry.join(".admission.lock"))
        .unwrap();
    drop(mutex);
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).is_err());
    assert!(reserve(&fixture, 1, &slot("actor", "main", 1)).is_err());
    fs::remove_file(directory.join("unrecognized")).unwrap();
    fs::write(directory.join("owner.json"), b"{malformed-committed").unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).is_err());
    assert!(reserve(&fixture, 1, &slot("actor", "main", 1)).is_err());
    assert!(directory.join("owner.json").exists());
}

#[test]
fn committed_generation_identity_must_match_its_directory() {
    let fixture = Fixture::new();
    let record = reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    let mut value = read_owner(&record).unwrap();
    value["generation_id"] = json!(format!("{:048x}", 2));
    persist_owner(&record, &value).unwrap();
    assert!(protected_native_reads_stopped(&fixture.registry, &fixture.lock(), (0, 0)).is_err());
    assert!(reserve(&fixture, 3, &slot("other", "main", 1)).is_err());
    assert!(record.exists());
}

#[test]
fn fixture_registry_lookup() {
    let Some(result) = std::env::var_os("LEAN_NATIVE_TEST_REGISTRY_RESULT") else {
        return;
    };
    let receipt = match registry_directory() {
        Ok(root) => json!({"root":root}),
        Err(error) => json!({"error":error.to_string()}),
    };
    fs::write(result, serde_json::to_vec(&receipt).unwrap()).unwrap();
}
fn isolated_lookup(fixture: &Fixture, root: &Path) -> Value {
    let result = fixture.root.join("lookup-result.json");
    let status = fixture_command(
        &fixture.root,
        "native_guardian::tests::capacity_tests::fixture_registry_lookup",
    )
    .env(OWNERS_DIRECTORY_ENV, root)
    .env("LEAN_NATIVE_TEST_REGISTRY_RESULT", &result)
    .status()
    .unwrap();
    assert!(status.success());
    serde_json::from_slice(&fs::read(result).unwrap()).unwrap()
}
#[test]
fn explicit_registry_lookup_binds_the_namespace_without_creating_it() {
    let fixture = Fixture::new();
    let root = fixture.root.join("fresh").join("owners");
    assert_eq!(isolated_lookup(&fixture, &root), json!({"root":root}));
    assert!(!fixture.root.join("fresh").exists());
    assert_eq!(
        isolated_lookup(&fixture, &fixture.registry),
        json!({"root":fixture.registry})
    );
}
#[test]
fn registry_lookup_rejects_public_alias_or_relative_namespace() {
    let fixture = Fixture::new();
    let public = fixture.root.join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(isolated_lookup(&fixture, &public).get("error").is_some());
    let alias = fixture.root.join("alias");
    std::os::unix::fs::symlink(&fixture.registry, &alias).unwrap();
    assert!(isolated_lookup(&fixture, &alias).get("error").is_some());
    assert!(
        isolated_lookup(&fixture, Path::new("relative-owners"))
            .get("error")
            .is_some()
    );
}

#[test]
fn preparation_utility_does_not_deadlock_a_full_resident_actor_quota() {
    let fixture = Fixture::new();
    reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    reserve(&fixture, 2, &slot("actor", "semantic", 1)).unwrap();
    let utility = reserve(&fixture, 3, &slot("utility", "preparation", 1)).unwrap();
    assert!(utility.exists());
    for (index, requested) in [
        (4, slot("second-utility", "preparation", 1)),
        (5, slot("second-actor", "main", 1)),
        (6, slot("other-limit", "preparation", 2)),
    ] {
        assert_eq!(
            reserve(&fixture, index, &requested).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
#[test]
fn preparation_only_preserves_resident_capacity_and_disjoint_keys() {
    let fixture = Fixture::new();
    reserve(&fixture, 1, &slot("utility", "preparation", 1)).unwrap();
    assert_eq!(
        reserve(&fixture, 2, &slot("utility", "main", 1)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    reserve(&fixture, 3, &slot("actor", "main", 1)).unwrap();
    reserve(&fixture, 4, &slot("actor", "semantic", 1)).unwrap();
    assert_eq!(
        reserve(&fixture, 5, &slot("actor-b", "main", 1)).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}
#[test]
fn closed_preparation_utility_recovers_without_evicting_resident_actor() {
    let fixture = Fixture::new();
    let resident = reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    let utility = reserve(&fixture, 2, &slot("utility", "preparation", 1)).unwrap();
    let mut child = private_group();
    running(&utility, &child);
    assert_eq!(
        reserve(&fixture, 3, &slot("new-utility", "preparation", 1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    stopped(&mut child);
    assert!(reserve(&fixture, 4, &slot("new-utility", "preparation", 1)).is_ok());
    assert!(resident.exists());
    assert!(!utility.exists());
}
#[test]
#[allow(
    clippy::needless_collect,
    reason = "start both preparation contenders before releasing their shared barrier"
)]
fn concurrent_preparation_admission_permits_exactly_one_global_utility() {
    let fixture = Fixture::new();
    reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let contenders: Vec<_> = (2..=3)
        .map(|index| {
            let registry = fixture.registry.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                reserve_owner_record_with_slot(
                    &registry,
                    &format!("{index:048x}"),
                    Duration::from_secs(1),
                    Some(&slot(&format!("utility-{index}"), "preparation", 1)),
                )
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = contenders.into_iter().map(|thread| thread.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock))
            .count(),
        1
    );
}

#[path = "input_tests.rs"]
mod input_tests;

#[test]
fn atomic_owner_publication_never_misclassifies_a_replaced_inode_as_malformed() {
    let fixture = Fixture::new();
    let record = reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    let expected = read_owner(&record).unwrap();
    let writer_record = record.clone();
    let writer_value = expected.clone();
    let writer = thread::spawn(move || {
        for _ in 0..100 {
            persist_owner(&writer_record, &writer_value).unwrap();
        }
    });
    let mut success = 0_usize;
    for _ in 0..500 {
        match read_owner(&record) {
            Ok(value) => {
                assert_eq!(value, expected);
                success += 1;
            }
            Err(error) => assert_eq!(
                error.kind(),
                io::ErrorKind::WouldBlock,
                "only demonstrated replacement may backpressure: {error}"
            ),
        }
    }
    writer.join().unwrap();
    assert!(success > 0);
    assert_eq!(read_owner(&record).unwrap(), expected);
    fs::write(&record, b"{stable malformed").unwrap();
    assert_ne!(read_owner(&record).unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn malformed_fifo_owner_is_rejected_without_waiting_for_a_writer() {
    let fixture = Fixture::new();
    let record = reserve(&fixture, 1, &slot("actor", "main", 1)).unwrap();
    fs::remove_file(&record).unwrap();
    nix::unistd::mkfifo(
        record.as_path(),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let started = Instant::now();
    let error = read_owner(&record).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(reserve(&fixture, 2, &slot("other", "main", 1)).is_err());
}
