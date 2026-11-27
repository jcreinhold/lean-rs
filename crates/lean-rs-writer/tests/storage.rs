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

use lean_toolchain::shared_writer::MODULE_SETUP_CAPTURE_ENV;
use serde_json::{Value, json};

fn root(role: &str, path: &Path) -> Value {
    let meta = path.metadata().expect("selected directory");
    json!({"role":role,"path":path,"device":meta.dev(),"inode":meta.ino()})
}

fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or(&Value::Null)
}

fn rollout(registry: &Path, action: &str) -> io::Result<std::process::Output> {
    let meta = registry.metadata()?;
    Command::new(env!("CARGO_BIN_EXE_lean-rs-writer"))
        .args(["writer-storage", action, "--registry"])
        .arg(registry)
        .arg("--registry-device")
        .arg(meta.dev().to_string())
        .arg("--registry-inode")
        .arg(meta.ino().to_string())
        .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", registry)
        .output()
}

fn approve(registry: &Path) -> io::Result<String> {
    let output = rollout(registry, "approve-handoff")?;
    assert!(output.status.success(), "{output:?}");
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    Ok(field(&reply, "approval_generation")
        .as_str()
        .expect("issued generation")
        .to_owned())
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
    assert!(locked_owner.get("scratch_identity").is_none());
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
    // A provisional stop on overflow is followed by an exact terminal receipt.
    // The borrowed-lock SDK caller may settle this turn, but cannot run again.
    let mut flood = Command::new("/usr/bin/printf");
    flood.args(["%s", &"x".repeat(64 * 1024 + 1)]);
    let overflow = writer.run(&flood, timeout).expect_err("unusable overflow result");
    let stopped = lean_toolchain::shared_writer::WriterFailure::from_io(&overflow).expect("typed closed failure");
    assert_eq!(
        stopped.disposition,
        lean_toolchain::shared_writer::WriterDisposition::Stopped
    );
    assert_eq!(stopped.detail, "writer captured output limit exceeded");
    assert_eq!(
        field(
            stopped.stop_receipt.as_ref().expect("terminal receipt"),
            "owned_work_stopped"
        ),
        true
    );
    let marker = a.join("replayed");
    assert!(
        writer
            .run(Command::new("/usr/bin/touch").arg(&marker), timeout)
            .is_err()
    );
    assert!(!marker.exists());
    assert!(assets.is_dir());
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
                &json!([
                    "command_only_storage_v1",
                    "command_only_handoff_v1",
                    "command_only_handoff_approval_v1",
                    "lake_module_setup_capture_v1",
                    "lake_module_setup_capture_env_v2"
                ])
            );
            assert_eq!(field(&descriptor, "handoff_max_bytes"), 262_144);
        } else {
            assert!(descriptor.get("capabilities").is_none());
            assert!(descriptor.get("handoff_max_bytes").is_none());
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
        assert!(owner.get("scratch_identity").is_none());
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

#[path = "storage/handoff.rs"]
mod handoff;

#[test]
#[ignore = "requires the workspace-pinned Elan installation"]
fn charged_lake_setup_preserves_required_bytes_and_close() -> io::Result<()> {
    use sha2::{Digest as _, Sha256};
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temp = tempfile::tempdir_in(workspace.join("target"))?;
    let base = temp.path().canonicalize()?;
    let project = base.join("project");
    let registry = base.join("owners");
    let scratch = base.join("scratch");
    for dir in [&project, &registry, &scratch] {
        fs::create_dir(dir)?;
    }
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
    let pin = fs::read_to_string(workspace.join("lean-toolchain"))?;
    fs::write(project.join("lean-toolchain"), &pin)?;
    let id = pin
        .trim()
        .strip_prefix("leanprover/lean4:")
        .expect("workspace Elan release");
    let home = std::env::var_os("ELAN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::home_dir().map(|p| p.join(".elan")))
        .expect("Elan home");
    let lake = home
        .join("toolchains")
        .join(format!("leanprover--lean4---{id}"))
        .join("bin/lake")
        .canonicalize()?;
    fs::write(
        project.join("lake-manifest.json"),
        br#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"demo","lakeDir":".lake"}"#,
    )?;
    let source = project.join("Demo.lean");
    fs::write(&source, "prelude\n")?;
    let bad_project = base.join("bad-project");
    fs::create_dir(&bad_project)?;
    fs::write(bad_project.join("lean-toolchain"), &pin)?;
    fs::copy(
        project.join("lake-manifest.json"),
        bad_project.join("lake-manifest.json"),
    )?;
    fs::write(bad_project.join("lakefile.toml"), "bad = [")?;
    let bad_source = bad_project.join("Bad.lean");
    fs::write(&bad_source, "prelude\n")?;
    let option = "x".repeat(72 * 1024);
    fs::write(
        project.join("lakefile.toml"),
        format!("name = \"demo\"\n[[lean_lib]]\nname = \"Demo\"\nleanOptions = {{ fixture = \"{option}\" }}\n"),
    )?;
    for overflow in [false, true] {
        let budget = if overflow { 64 * 1024 } else { 1024 * 1024 };
        let mut child = start_storage(&project, &registry, &scratch, budget, overflow)?;
        let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
        let locked = reply(&mut reader)?;
        assert_eq!(field(&locked, "state"), "locked");
        let assets = Path::new(field(&locked, "asset_dir").as_str().expect("charged scratch"));
        let staged = assets.join("module-setup.json");
        let mut env = serde_json::Map::from_iter([
            ("LAKE_CACHE_DIR".to_owned(), json!(assets)),
            ("LAKE_ARTIFACT_CACHE".to_owned(), json!("false")),
            (MODULE_SETUP_CAPTURE_ENV.to_owned(), json!("1")),
        ]);
        if !overflow {
            env.insert(
                lean_toolchain::native_guardian::OWNERS_DIRECTORY_ENV.to_owned(),
                json!(registry),
            );
        }
        send(
            &mut child,
            &json!({"action":"run","cwd":project,"timeout":10,
            "command":[lake,"--no-build","--no-cache","setup-file",source],
            "env":env}),
        )?;
        assert_eq!(field(&reply(&mut reader)?, "state"), "started");
        let mut result = reply(&mut reader)?;
        if field(&result, "state") == "still_running" {
            result = reply(&mut reader)?;
        }
        assert_eq!(field(&result, "state"), "stopped");
        assert_eq!(field(&result, "owned_work_stopped"), true);
        if overflow {
            assert_eq!(field(&result, "result"), "no_result");
            assert!(
                field(&result, "reason")
                    .as_str()
                    .expect("overflow reason")
                    .contains("exceeds")
            );
            assert_eq!(field(&result, "stdout"), "");
            assert!(!staged.exists());
            assert!(!child.wait()?.success());
            assert!(assets.is_dir(), "unconfirmed close must retain the charge");
        } else {
            assert_eq!(field(&result, "exit_code"), 0, "{}", field(&result, "stderr"));
            assert_eq!(field(&result, "result"), "result");
            let bytes = fs::read(&staged)?;
            assert!(bytes.len() > 64 * 1024);
            let setup: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(
                field(field(&setup, "options"), "fixture").as_str(),
                Some(option.as_str())
            );
            let digest = field(&result, "stdout").as_str().expect("hex digest");
            assert_eq!(digest.len(), 64);
            for (encoded, expected) in digest.as_bytes().as_chunks::<2>().0.iter().zip(Sha256::digest(&bytes)) {
                assert_eq!(encoded, format!("{expected:02x}").as_bytes());
            }
            send(&mut child, &json!({"action":"close"}))?;
            assert!(child.wait()?.success());
            assert!(!assets.exists());
        }
    }
    // The command-only owner can recover the exact stopped turn. An ordinary
    // command still reports its own exit and stdout, without a setup file.
    let mut child = start_storage(&bad_project, &registry, &scratch, 1024 * 1024, true)?;
    let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
    let locked = reply(&mut reader)?;
    let assets = Path::new(field(&locked, "asset_dir").as_str().expect("fresh charged scratch"));
    assert_eq!(fs::read_dir(&scratch)?.count(), 1);
    send(
        &mut child,
        &json!({"action":"run","command":["/usr/bin/printf","ordinary"],"timeout":3}),
    )?;
    assert_eq!(field(&reply(&mut reader)?, "state"), "started");
    let ordinary = reply(&mut reader)?;
    assert_eq!(field(&ordinary, "exit_code"), 0);
    assert_eq!(field(&ordinary, "stdout"), "ordinary");
    assert!(!assets.join("module-setup.json").exists());
    // Lake's own error remains usable. A failed command publishes no setup.
    send(
        &mut child,
        &json!({"action":"run","cwd":bad_project,"timeout":10,
        "command":[lake,"--no-build","--no-cache","setup-file",bad_source],
        "env":{(MODULE_SETUP_CAPTURE_ENV):"1","LAKE_CACHE_DIR":assets,"LAKE_ARTIFACT_CACHE":"false"}}),
    )?;
    assert_eq!(field(&reply(&mut reader)?, "state"), "started");
    let failure = reply(&mut reader)?;
    assert_eq!(field(&failure, "result"), "result");
    assert!(field(&failure, "exit_code").as_i64().expect("Lake exit") != 0);
    assert!(!field(&failure, "stderr").as_str().expect("Lake diagnostic").is_empty());
    assert_eq!(field(&failure, "stdout"), "");
    assert!(!assets.join("module-setup.json").exists());
    send(&mut child, &json!({"action":"close"}))?;
    assert!(child.wait()?.success());
    assert_eq!(fs::read_dir(&scratch)?.count(), 0);
    Ok(())
}

#[test]
fn stopped_overflow_accepts_only_explicit_close_for_its_charge() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    for action in ["close", "run", "EOF"] {
        let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
        let base = temp.path().canonicalize()?;
        let (project, registry, scratch) = (base.join("project"), base.join("owners"), base.join("scratch"));
        for path in [&project, &registry, &scratch] {
            fs::create_dir(path)?;
        }
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
        let mut child = start(&project, &registry, &scratch, 1024 * 1024)?;
        let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
        let locked = reply(&mut reader)?;
        let assets = Path::new(field(&locked, "asset_dir").as_str().expect("charged scratch"));
        let lock = lean_toolchain::shared_writer::project_lock_path(&project)?;
        let owner: Value = serde_json::from_slice(&fs::read(&lock)?)?;
        let charge = registry
            .join(".writer-storage-v1")
            .join(field(&owner, "storage_turn").as_str().expect("admitted turn"));
        send(
            &mut child,
            &json!({"action":"run","timeout":3,
            "command":["/usr/bin/printf","%s","x".repeat(64 * 1024 + 1)]}),
        )?;
        assert_eq!(field(&reply(&mut reader)?, "state"), "started");
        let mut terminal = reply(&mut reader)?;
        if field(&terminal, "state") == "still_running" {
            terminal = reply(&mut reader)?;
        }
        assert_eq!(field(&terminal, "state"), "stopped");
        assert_eq!(field(&terminal, "result"), "no_result");
        assert_eq!(field(&terminal, "owned_work_stopped"), true);
        assert_eq!(field(&terminal, "reason"), "writer captured output limit exceeded");
        let stopped: Value = serde_json::from_slice(&fs::read(lock)?)?;
        assert!(field(&stopped, "process").is_null());
        assert!(charge.is_file() && assets.is_dir());
        let marker = project.join("second-command-ran");
        match action {
            "close" => send(&mut child, &json!({"action":"close"}))?,
            "run" => send(
                &mut child,
                &json!({"action":"run","command":["/usr/bin/touch",marker],"timeout":3}),
            )?,
            _ => drop(child.stdin.take()),
        }
        assert_eq!(child.wait()?.success(), action == "close");
        assert_eq!(reader.read_line(&mut String::new())?, 0);
        assert!(!marker.exists());
        assert_eq!(assets.is_dir(), action != "close");
        assert_eq!(charge.is_file(), action != "close");
    }
    Ok(())
}

#[path = "storage/stage_permit.rs"]
mod stage_permit;
