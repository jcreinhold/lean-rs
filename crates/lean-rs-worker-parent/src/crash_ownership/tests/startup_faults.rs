//! Reviewed, opt-in startup faults. Neither test changes production semantics.
use super::*;
use std::cell::RefCell;
use std::net::Shutdown;

thread_local! {
    static WRITE_HOOK: RefCell<Option<Box<dyn FnMut(usize)>>> = RefCell::new(None);
}
pub(in crate::crash_ownership) fn write_limit(length: usize) -> usize {
    WRITE_HOOK.with(|hook| if hook.borrow().is_some() { 1 } else { length })
}
pub(in crate::crash_ownership) fn after_write(bytes: usize) {
    WRITE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(bytes);
        }
    });
}
struct ResetHook;
impl Drop for ResetHook {
    fn drop(&mut self) {
        WRITE_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

pub(super) fn on_first_ack_write(action: impl FnOnce(usize) + 'static) -> impl Drop {
    let mut action = Some(action);
    WRITE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |count| {
            if let Some(action) = action.take() {
                action(count);
            }
        }));
    });
    ResetHook
}

#[test]
#[ignore = "fresh opt-in transport qualification; exact command review required"]
fn partial_ack_fails_after_positive_wire_write() {
    let root = fresh_root();
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&observed);
    let writer = stream.try_clone().unwrap();
    let evidence_root = root.clone();
    let mut first = true;
    WRITE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |count| {
            if !first {
                return;
            }
            first = false;
            fs::write(
                evidence_root.join("write-evidence.json"),
                serde_json::to_vec(&serde_json::json!({"successful_write_bytes":count})).unwrap(),
            )
            .unwrap();
            assert_eq!(count, 1);
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
            output.lock().unwrap().extend(byte);
            fs::write(
                evidence_root.join("peer-evidence.json"),
                serde_json::to_vec(&serde_json::json!({"peer_bytes":byte})).unwrap(),
            )
            .unwrap();
            // Controlled local transport fault, not a remote-shutdown timing claim.
            // This clone shares the writer socket; the next real write must fail.
            writer.shutdown(Shutdown::Write).unwrap();
        }));
    });
    let _reset = ResetHook;
    let identity = LeanWorkerOwnedProcess::capture(std::process::id()).unwrap().unwrap();
    let generation = LeanWorkerOwnedGeneration {
        version: 2,
        scope: "transport-test-only".into(),
        generation_id: "transport-test-only".into(),
        parent: identity.clone(),
        process: identity.clone(),
        phase: "running".into(),
        guardian: Some(identity.clone()),
        protected_domains: Vec::new(),
        capacity_slot: None,
    };
    // This synthetic admitted lease isolates ACK framing, not admission or OS custody.
    let mut lease = ParentLifetimeLease {
        stream,
        owner: identity.clone(),
        guardian: Some(identity),
        record: root.join("owner.json"),
        nonce: "transport-test-only".into(),
        guarded: true,
        generation,
        deadline: Instant::now() + Duration::from_secs(2),
        running_published: true,
    };
    let result = lease.acknowledge();
    let bytes = observed.lock().unwrap().clone();
    fs::write(
        root.join("receipt.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "positive_wire_bytes": bytes, "error": result.as_ref().err().map(ToString::to_string),
            "error_kind":result.as_ref().err().map(|error| format!("{:?}",error.kind())),
            "ack_returned_ok":result.is_ok(), "ack_calls": 1,
            "complete_frame": bytes.contains(&b'\n'),
            "scope": "real acknowledge/write loop; synthetic admission; controlled local write shutdown"
        }))
        .unwrap(),
    )
    .unwrap();
    let error = result.expect_err("partial ACK must not succeed");
    assert_eq!(bytes, b"{");
    assert!(!bytes.contains(&b'\n'));
    assert!(matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    ));
}

const FAULT_ROOT: &str = "LEAN_STARTUP_FAULT_ROOT";
// Rescue only: a fired watchdog invalidates qualification. Each process exits
// itself; it never signals a stored PID or an unrelated process.
#[expect(clippy::exit, reason = "bounded test rescue exits only its own fixture process")]
pub(super) fn fixture_watchdog(role: &'static str) {
    let Some(root) = std::env::var_os(FAULT_ROOT).map(PathBuf::from) else {
        return;
    };
    let identity = LeanWorkerOwnedProcess::capture(std::process::id()).unwrap().unwrap();
    fs::write(
        root.join(format!("{role}-identity.write")),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    fs::rename(
        root.join(format!("{role}-identity.write")),
        root.join(format!("{role}-identity.json")),
    )
    .unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(if role == "worker" { 8 } else { 12 }));
        let _saved = fs::write(root.join(format!("{role}-rescue")), b"self exit watchdog");
        std::process::exit(97);
    });
}

fn fresh_root() -> PathBuf {
    let root = PathBuf::from(std::env::var_os(FAULT_ROOT).expect("fresh output path required"));
    fs::create_dir(&root).expect("one-use output directory must not exist");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    root.canonicalize().unwrap()
}
pub(super) fn collide_bootstrap_publication() {
    if std::env::var_os("LEAN_STARTUP_COLLIDE_BOOTSTRAP").is_none() {
        return;
    }
    let record = PathBuf::from(std::env::var_os("LEAN_STARTUP_FAULT_RECORD").unwrap());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(value.get("phase"), Some(&serde_json::json!("dispatching")));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(record.with_extension("write"))
        .unwrap();
}
pub(super) fn pause_drained_guardian(result: io::Result<()>) -> io::Result<()> {
    if std::env::var_os("LEAN_STARTUP_COLLIDE_BOOTSTRAP").is_none() {
        return result;
    }
    let root = PathBuf::from(std::env::var_os(FAULT_ROOT).unwrap());
    let bytes = serde_json::to_vec(&serde_json::json!({
        "ok": result.is_ok(), "error": result.as_ref().err().map(ToString::to_string),
        "kind": result.as_ref().err().map(|error| format!("{:?}", error.kind()))
    }))
    .map_err(io::Error::other)?;
    fs::write(root.join("guardian-result.json"), bytes)?;
    fs::write(root.join("guardian-drained"), b"real guardian drain returned")?;
    // This module's self-watchdog is the bounded rescue.
    while !root.join("guardian-release").exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    match result {
        Err(error)
            if error.kind() == io::ErrorKind::InvalidData
                && error.to_string() == "guardian channel closed before ACK" =>
        {
            Ok(())
        }
        Err(error) => Err(error),
        Ok(()) => Err(io::Error::other("expected missing-ACK failure, got success")),
    }
}

// On assertion unwind release only this fixture and retain an honest census.
// No stored identity is used for signalling. PendingCleanup drops before this guard.
struct FinalCensus {
    root: PathBuf,
    record: PathBuf,
}
impl Drop for FinalCensus {
    fn drop(&mut self) {
        let _release = fs::write(self.root.join("guardian-release"), b"finalize owned fixture");
        let Some(until) = Instant::now().checked_add(Duration::from_secs(14)) else {
            return;
        };
        let mut sample = serde_json::json!({"closed": false, "reason": "no authenticated record"});
        while Instant::now() < until {
            if let Ok(bytes) = fs::read(&self.record)
                && let Ok(owner) = serde_json::from_slice::<LeanWorkerOwnedGeneration>(&bytes)
                && let Some(guardian) = owner.guardian
            {
                let group = lean_toolchain::process_group::group_present(owner.process.process_group);
                let live = guardian.is_live();
                let closed = matches!(group, Ok(false)) && matches!(live, Ok(false));
                sample = serde_json::json!({"closed":closed, "worker":owner.process,
                    "guardian":guardian, "group_present":format!("{group:?}"),
                    "guardian_live":format!("{live:?}"), "unwinding":std::thread::panicking()});
                if closed {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(sample) = sample.as_object_mut() {
            sample.insert(
                "rescue".into(),
                serde_json::json!(
                    self.root.join("worker-rescue").exists() || self.root.join("guardian-rescue").exists()
                ),
            );
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&sample) {
            let _saved = fs::write(self.root.join("final-census.json"), bytes);
        }
    }
}

#[test]
#[ignore = "fresh one-use native qualification; exact command review required"]
fn actual_after_spawn_retains_custody_until_guardian_exit() {
    let root = fresh_root();
    write_guardian_script(&root);
    let scope = LeanWorkerNativeScope::prepare(&root.join("owners"), Duration::from_secs(2), None).unwrap();
    let record = scope.0.directory.join("owner.json");
    let nonce = scope.0.nonce.clone();
    let _census = FinalCensus {
        root: root.clone(),
        record: record.clone(),
    };
    let mut command = native_worker_command(&root);
    command
        .env(FAULT_ROOT, &root)
        .env("LEAN_STARTUP_COLLIDE_BOOTSTRAP", "1")
        .env("LEAN_STARTUP_FAULT_RECORD", &record)
        .env_remove("LEAN_STARTUP_QUALIFICATION_ROOT")
        .env_remove("LEAN_NATIVE_GUARDIAN_FAILURE_ROOT");
    let error = scope.spawn(&mut command, Duration::from_secs(3)).unwrap_err();
    assert!(matches!(error, LeanWorkerNativeSpawnError::AfterSpawn { .. }));
    let LeanWorkerNativeSpawnError::AfterSpawn { source, mut pending } = error else {
        return;
    };
    assert_eq!(source.kind(), io::ErrorKind::AlreadyExists);
    wait_for(|| {
        // Advance direct-child reaping while the paused guardian still owns custody.
        assert!(pending.try_wait().unwrap().is_none());
        root.join("guardian-drained").exists()
    });
    let guardian_result: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("guardian-result.json")).unwrap()).unwrap();
    assert_eq!(
        guardian_result.get("error"),
        Some(&serde_json::json!("guardian channel closed before ACK"))
    );
    assert_eq!(guardian_result.get("kind"), Some(&serde_json::json!("InvalidData")));
    let closed: LeanWorkerOwnedGeneration = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(closed.phase, "bootstrap_closed");
    assert_eq!(closed.generation_id, nonce);
    let guardian = closed.guardian.as_ref().unwrap();
    {
        let authenticated = pending.custody.0.lock().unwrap();
        let owner = authenticated.as_ref().unwrap().owner.clone();
        let authenticated_guardian = authenticated.as_ref().unwrap().guardian.clone();
        drop(authenticated);
        assert_eq!(closed.process, owner);
        assert_eq!(closed.guardian, authenticated_guardian);
    }
    assert!(guardian.is_live().unwrap());
    assert!(!lean_toolchain::process_group::group_present(closed.process.process_group).unwrap());
    assert!(!pending.custody_closed().unwrap());
    assert!(pending.try_wait().unwrap().is_none());
    assert_eq!(pending.child.test_active_processes(), 1);
    assert!(!root.join("native-body").exists());
    fs::write(root.join("guardian-release"), b"release only owned fixture").unwrap();
    wait_for(|| {
        pending.try_wait().unwrap();
        pending.custody_closed().unwrap()
    });
    wait_for(|| {
        pending.try_wait().unwrap();
        pending.child.test_active_processes() == 0
    });
    assert!(!guardian.is_live().unwrap());
    assert!(!root.join("worker-rescue").exists());
    assert!(!root.join("guardian-rescue").exists());
    assert!(!root.join("native-body").exists());
    fs::write(
        root.join("receipt.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "error_kind": "AlreadyExists", "typed_after_spawn": true, "closure_record": closed,
            "tracker_while_guardian_live": 1, "tracker_after_closure": 0,
            "body_started": false, "rescue": false,
            "scope": "actual SDK spawn; deliberate publication collision; process-crash not power-loss"
        }))
        .unwrap(),
    )
    .unwrap();
}
