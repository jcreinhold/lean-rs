//! Real cooperating writer processes, with no simulated disk pressure.
#![allow(
    clippy::expect_used,
    reason = "integration fixture assertions retain the failing operation"
)]
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

fn root(role: &str, path: &Path) -> Value {
    let meta = path.metadata().expect("selected directory");
    json!({"role":role,"path":path,"device":meta.dev(),"inode":meta.ino()})
}

fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or(&Value::Null)
}

fn start(project: &Path, registry: &Path, scratch: &Path, budget: u64) -> io::Result<Child> {
    start_storage(project, registry, scratch, budget, false)
}

fn start_storage(
    project: &Path,
    registry: &Path,
    scratch: &Path,
    budget: u64,
    command_only: bool,
) -> io::Result<Child> {
    let lock = lean_toolchain::shared_writer::project_lock_path(project)?;
    let mut request = json!({"version":1,"floor_bytes":1,"budget_bytes":budget,
        "roots":[root("project",project),root("registry",registry),root("scratch_parent",scratch)]});
    if command_only {
        request
            .as_object_mut()
            .expect("fixture storage request object")
            .insert("command_only".to_owned(), json!(true));
    }
    Command::new(env!("CARGO_BIN_EXE_lean-rs-writer"))
        .arg("writer")
        .arg(project)
        .arg(lock)
        .arg("3")
        .arg("--storage-request")
        .arg(request.to_string())
        .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", registry)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn reply(reader: &mut BufReader<ChildStdout>) -> io::Result<Value> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

fn send(child: &mut Child, frame: &Value) -> io::Result<()> {
    writeln!(child.stdin.as_mut().expect("writer control"), "{frame}")
}

#[test]
fn two_projects_share_one_reserve_until_explicit_close() -> io::Result<()> {
    let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
    let base = temp.path().canonicalize()?;
    let (a, b, registry, scratch) = (
        base.join("a"),
        base.join("b"),
        base.join("owners"),
        base.join("scratch"),
    );
    for path in [&a, &b, &registry, &scratch] {
        fs::create_dir(path)?;
    }
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
    let stat = rustix::fs::statvfs(&base)?;
    let free = stat.f_bavail.checked_mul(stat.f_frsize).expect("fixture pool capacity");
    let budget = free - free / 4;
    for (project, exit) in [(&a, 1), (&b, 0)] {
        let mut child = start(project, &registry, &scratch, budget)?;
        let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
        let locked = reply(&mut reader)?;
        assert_eq!(field(&locked, "state"), "locked");
        let assets = Path::new(field(&locked, "asset_dir").as_str().expect("scratch path"));
        assert_eq!(assets.parent(), Some(scratch.as_path()));
        send(
            &mut child,
            &json!({"action":"run","command":[if exit==0 {"/usr/bin/true"} else {"/usr/bin/false"}],"timeout":3}),
        )?;
        assert_eq!(field(&reply(&mut reader)?, "state"), "started");
        let stopped = reply(&mut reader)?;
        assert_eq!(field(&stopped, "state"), "stopped");
        assert_eq!(field(&stopped, "exit_code"), exit);
        assert_eq!(field(&stopped, "owned_work_stopped"), true);
        if project == &a {
            let mut contender = start(&b, &registry, &scratch, budget)?;
            // Admission treats control EOF as cancellation. Keep the control pipe live.
            let _control = contender.stdin.take().expect("contender lifeline");
            let refused = contender.wait_with_output()?;
            assert!(!refused.status.success());
            assert!(refused.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&refused.stderr)
                    .contains("insufficient space for outstanding full-turn writer budgets"),
                "{}",
                String::from_utf8_lossy(&refused.stderr)
            );
            assert_eq!(fs::read_dir(&scratch)?.count(), 1);
        }
        send(&mut child, &json!({"action":"close"}))?;
        assert!(child.wait()?.success());
        assert!(!assets.exists());
    }
    let mut abandoned = start(&a, &registry, &scratch, budget)?;
    let mut reader = BufReader::new(abandoned.stdout.take().expect("managed replies"));
    let locked = reply(&mut reader)?;
    let assets = Path::new(field(&locked, "asset_dir").as_str().expect("abandoned scratch"));
    let lock = lean_toolchain::shared_writer::project_lock_path(&a)?;
    let locked_owner: Value = serde_json::from_slice(&fs::read(&lock)?)?;
    let turn = field(&locked_owner, "storage_turn")
        .as_str()
        .expect("managed lock turn");
    assert!(locked_owner.get("command_phase").is_none());
    assert_eq!(
        assets.file_name().and_then(|name| name.to_str()),
        Some(format!("lean-writer-assets-{turn}").as_str())
    );
    let charge: Value = serde_json::from_slice(&fs::read(registry.join(".writer-storage-v1").join(turn))?)?;
    assert!(charge.get("command_only").is_none());
    let scratch_identity = assets.metadata()?;
    assert_eq!(
        field(&charge, "scratch"),
        &json!({"path":assets,"device":scratch_identity.dev(),"inode":scratch_identity.ino()})
    );

    send(
        &mut abandoned,
        &json!({"action":"run","command":["/usr/bin/true"],"timeout":3}),
    )?;
    assert_eq!(field(&reply(&mut reader)?, "state"), "started");
    let stopped = reply(&mut reader)?;
    assert_eq!(field(&stopped, "state"), "stopped");
    assert_eq!(field(&stopped, "owned_work_stopped"), true);
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(&lock)?)?, locked_owner);
    drop(abandoned.stdin.take());
    assert!(!abandoned.wait()?.success());
    assert!(assets.is_dir());
    let mut contender = start(&b, &registry, &scratch, budget)?;
    let _control = contender.stdin.take().expect("contender lifeline");
    let refused = contender.wait_with_output()?;
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("insufficient space for outstanding full-turn writer budgets")
    );
    assert_eq!(fs::read_dir(&scratch)?.count(), 1);

    // This harness owns the exact scratch and has no caller-owned writes. Its
    // observations are not durable proof that a restarted writer can use.
    fs::remove_dir(assets)?;
    assert!(!assets.exists());
    let mut restarted = start(&a, &registry, &scratch, budget)?;
    let _restart_control = restarted.stdin.take().expect("restart lifeline");
    let refused = restarted.wait_with_output()?;
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("prior writer storage custody remains open"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(fs::read_dir(&scratch)?.count(), 0);
    let mut legacy = Command::new(env!("CARGO_BIN_EXE_lean-rs-writer"))
        .arg("writer")
        .arg(&a)
        .arg(&lock)
        .arg("3")
        .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &registry)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let _legacy_control = legacy.stdin.take().expect("unbudgeted restart lifeline");
    let refused = legacy.wait_with_output()?;
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("prior writer storage custody remains open"));
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(&lock)?)?, locked_owner);

    // Exercise passive admission with a real session owner and a test-owned
    // fault cut after Starting, before a process identity could be committed.
    let mut starting = locked_owner;
    starting
        .as_object_mut()
        .expect("fixture writer record object")
        .insert("command_phase".to_owned(), json!("starting"));
    let mut bytes = serde_json::to_vec(&starting)?;
    bytes.push(b'\n');
    fs::write(&lock, bytes)?;
    assert!(!lean_toolchain::shared_writer::previous_writer_stopped(
        &fs::File::open(&lock)?
    )?);
    Ok(())
}

#[test]
fn sdk_writer_shares_the_standalone_reserve() -> io::Result<()> {
    use lean_toolchain::shared_writer::SharedWriter;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    const FIXTURE: &str = "LEAN_RS_TEST_SDK_STORAGE_FIXTURE";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
        let base = temp.path().canonicalize()?;
        for name in ["a", "b", "owners", "scratch"] {
            fs::create_dir(base.join(name))?;
        }
        fs::set_permissions(base.join("owners"), fs::Permissions::from_mode(0o700))?;
        // Bind the launcher's trusted registry without changing this test process's environment.
        let status = Command::new(std::env::current_exe()?)
            .args(["--exact", "sdk_writer_shares_the_standalone_reserve", "--nocapture"])
            .env(FIXTURE, &base)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", base.join("owners"))
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let base = Path::new(&base);
    let a = base.join("a");
    let b = base.join("b");
    let registry = base.join("owners");
    let scratch = base.join("scratch");
    let stat = rustix::fs::statvfs(base)?;
    let free = stat.f_bavail.checked_mul(stat.f_frsize).expect("fixture pool capacity");
    let budget = free - free / 4;
    let request = json!({"version":1,"floor_bytes":1,"budget_bytes":budget,
        "roots":[root("project",&a), root("registry",&registry), root("scratch_parent",&scratch)]});
    let timeout = Duration::from_secs(3);
    let mut writer = SharedWriter::open_with_storage_request(
        Path::new(env!("CARGO_BIN_EXE_lean-rs-writer")),
        &a,
        &[],
        &request.to_string(),
        timeout,
        &|| false,
    )?;
    let output = writer.run(&Command::new("/usr/bin/false"), timeout)?;
    let assets = writer.asset_dir().to_owned();
    assert_eq!(assets.parent(), Some(scratch.as_path()));
    let mut contender = start(&b, &registry, &scratch, budget)?;
    let _control = contender.stdin.take().expect("keep admission live");
    let refused = contender.wait_with_output()?;
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("insufficient space for outstanding full-turn writer budgets")
    );
    writer.close(timeout)?;
    assert_eq!(output.status.code(), Some(1));
    assert!(!assets.exists());

    let mut successor = start(&b, &registry, &scratch, budget)?;
    let mut reader = BufReader::new(successor.stdout.take().expect("writer replies"));
    assert_eq!(field(&reply(&mut reader)?, "state"), "locked");
    send(&mut successor, &json!({"action":"close"}))?;
    assert!(successor.wait()?.success());
    assert_eq!(fs::read_dir(&scratch)?.count(), 0);
    Ok(())
}

#[test]
fn command_only_capability_is_explicit_and_default_descriptor_is_unchanged() -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    for advertise in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
        command.arg("writer-lock-path").arg(&root);
        if advertise {
            command.arg("--capabilities");
        }
        let output = command.output()?;
        assert!(output.status.success());
        let descriptor: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(field(&descriptor, "project_root"), &json!(root));
        assert_eq!(field(&descriptor, "protocol"), 1);
        if advertise {
            assert_eq!(
                field(&descriptor, "capabilities"),
                &json!(["command_only_storage_v1", "command_only_handoff_v1"])
            );
        } else {
            assert!(descriptor.get("capabilities").is_none());
        }
    }
    Ok(())
}

#[test]
fn command_only_idle_recovery_keeps_substitutions_and_retries_cleanup() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
    let base = temp.path().canonicalize()?;
    let (project, registry, scratch) = (base.join("project"), base.join("owners"), base.join("scratch"));
    for path in [&project, &registry, &scratch] {
        fs::create_dir(path)?;
    }
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
    let lock = lean_toolchain::shared_writer::project_lock_path(&project)?;
    let start = || start_storage(&project, &registry, &scratch, 1, true);
    let abandon_idle = || -> io::Result<_> {
        let mut child = start()?;
        let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
        let locked = reply(&mut reader)?;
        assert_eq!(field(&locked, "state"), "locked");
        let assets = Path::new(field(&locked, "asset_dir").as_str().expect("scratch path")).to_path_buf();
        send(
            &mut child,
            &json!({"action":"run","command":["/usr/bin/true"],"timeout":3}),
        )?;
        assert_eq!(field(&reply(&mut reader)?, "state"), "started");
        assert_eq!(field(&reply(&mut reader)?, "owned_work_stopped"), true);
        let owner: Value = serde_json::from_slice(&fs::read(&lock)?)?;
        assert_eq!(field(&owner, "command_phase"), "idle");
        let charge = registry
            .join(".writer-storage-v1")
            .join(field(&owner, "storage_turn").as_str().expect("idle turn"));
        drop(child.stdin.take());
        assert!(!child.wait()?.success());
        Ok((assets, charge))
    };
    let refuse = |message: &str| -> io::Result<()> {
        let mut child = start()?;
        let _control = child.stdin.take().expect("writer control");
        let result = child.wait_with_output()?;
        assert!(!result.status.success());
        assert!(result.stdout.is_empty(), "refused writer advertised admission");
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(message), "{error}");
        Ok(())
    };

    let (old_scratch, old_charge) = abandon_idle()?;
    let old_bytes = fs::read(&old_charge)?;
    let idle_owner = fs::read(&lock)?;
    let mut legacy: Value = serde_json::from_slice(&idle_owner)?;
    legacy
        .as_object_mut()
        .expect("writer owner object")
        .remove("command_phase");
    let mut legacy_bytes = serde_json::to_vec(&legacy)?;
    legacy_bytes.push(b'\n');
    fs::write(&lock, legacy_bytes)?;
    refuse("prior writer storage custody remains open")?;
    assert!(old_scratch.is_dir());
    assert_eq!(fs::read(&old_charge)?, old_bytes);
    fs::write(&lock, idle_owner)?;

    fs::write(old_scratch.join("old-scratch"), b"owned")?;
    let moved = scratch.join("displaced");
    fs::rename(&old_scratch, &moved)?;
    refuse("No such file")?;
    assert_eq!(fs::read(&old_charge)?, old_bytes);
    fs::create_dir(&old_scratch)?;
    fs::write(old_scratch.join("substitute"), b"not owned")?;
    refuse("scratch inode changed")?;
    assert_eq!(fs::read(old_scratch.join("substitute"))?, b"not owned");
    assert_eq!(fs::read(&old_charge)?, old_bytes);
    fs::remove_file(old_scratch.join("substitute"))?;
    fs::remove_dir(&old_scratch)?;
    fs::rename(moved, &old_scratch)?;

    // A new standalone helper must reclaim the old idle charge and its scratch.
    let (next_scratch, next_charge) = abandon_idle()?;
    assert!(!old_scratch.exists());
    assert!(!old_charge.exists());
    assert_ne!(next_scratch, old_scratch);

    // Model the cut after a durable cleanup intent and unlink, before settlement.
    let mut cut: Value = serde_json::from_slice(&fs::read(&next_charge)?)?;
    cut.as_object_mut()
        .expect("fixture writer charge object")
        .insert("scratch_cleanup_started".to_owned(), json!(true));
    fs::write(&next_charge, serde_json::to_vec(&cut)?)?;
    fs::remove_dir(&next_scratch)?;
    let mut child = start()?;
    let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
    assert_eq!(field(&reply(&mut reader)?, "state"), "locked");
    assert!(!next_charge.exists());
    send(&mut child, &json!({"action":"close"}))?;
    assert!(child.wait()?.success());
    assert!(fs::read_dir(&scratch)?.next().is_none());
    assert!(!registry.join(".writer-storage-v1").exists());
    Ok(())
}

#[test]
fn disjoint_admission_reclaims_only_the_dead_idle_holder() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
    let base = temp.path().canonicalize()?;
    let (a, b, registry, scratch) = (
        base.join("a"),
        base.join("b"),
        base.join("owners"),
        base.join("scratch"),
    );
    for path in [&a, &b, &registry, &scratch] {
        fs::create_dir(path)?;
    }
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
    let stat = rustix::fs::statvfs(&base)?;
    let free = stat.f_bavail.checked_mul(stat.f_frsize).expect("fixture pool capacity");
    let budget = free - free / 4;
    let mut first = start_storage(&a, &registry, &scratch, budget, true)?;
    let mut reader = BufReader::new(first.stdout.take().expect("first writer replies"));
    let locked = reply(&mut reader)?;
    assert_eq!(field(&locked, "state"), "locked");
    let assets = Path::new(field(&locked, "asset_dir").as_str().expect("first scratch path"));
    send(
        &mut first,
        &json!({"action":"run","command":["/usr/bin/true"],"timeout":3}),
    )?;
    assert_eq!(field(&reply(&mut reader)?, "state"), "started");
    assert_eq!(field(&reply(&mut reader)?, "owned_work_stopped"), true);
    let lock = lean_toolchain::shared_writer::project_lock_path(&a)?;
    let original = fs::read(&lock)?;
    let owner: Value = serde_json::from_slice(&original)?;
    assert_eq!(field(&owner, "command_phase"), "idle");
    let charge = registry
        .join(".writer-storage-v1")
        .join(field(&owner, "storage_turn").as_str().expect("idle turn"));
    let charged = fs::read(&charge)?;

    let mut contender = start(&b, &registry, &scratch, budget)?;
    let _control = contender.stdin.take().expect("live contender control");
    let refused = contender.wait_with_output()?;
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("insufficient space for outstanding full-turn writer budgets")
    );
    assert_eq!(fs::read(&charge)?, charged);
    assert!(assets.is_dir());

    drop(first.stdin.take());
    assert!(!first.wait()?.success());
    let mut second = start(&b, &registry, &scratch, budget)?;
    let mut reader = BufReader::new(second.stdout.take().expect("disjoint writer replies"));
    assert_eq!(field(&reply(&mut reader)?, "state"), "locked");
    assert!(!assets.exists());
    assert!(!charge.exists());
    assert_eq!(fs::read(&lock)?, original, "the first project was not reopened");
    send(&mut second, &json!({"action":"close"}))?;
    assert!(second.wait()?.success());
    assert!(fs::read_dir(&scratch)?.next().is_none());
    Ok(())
}

#[path = "storage/exact_turn.rs"]
mod exact_turn;
