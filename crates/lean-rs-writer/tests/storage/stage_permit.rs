use super::*;
use lean_toolchain::native_guardian::{OWNERS_DIRECTORY_ENV, guardian_deadline_ticks};
use lean_toolchain::shared_writer::{SharedWriter, WriterDisposition, WriterFailure, WriterStagePermit};
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

fn expiry(delay: Duration) -> io::Result<u64> {
    u64::try_from(guardian_deadline_ticks()?.saturating_add(delay).as_nanos()).map_err(io::Error::other)
}

struct StageFixture {
    temp: tempfile::TempDir,
    project: std::path::PathBuf,
    registry: std::path::PathBuf,
    scratch: std::path::PathBuf,
}

impl StageFixture {
    fn new() -> io::Result<Self> {
        let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
        let base = temp.path().canonicalize()?;
        let project = base.join("demo");
        let registry = base.join("owners");
        let scratch = base.join("scratch");
        for path in [&project, &registry, &scratch] {
            fs::create_dir(path)?;
        }
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            temp,
            project,
            registry,
            scratch,
        })
    }
}

#[test]
fn conflicting_setup_markers_refuse_the_stage_before_command_dispatch() -> io::Result<()> {
    let fixture = StageFixture::new()?;
    let mut child = start_storage(&fixture.project, &fixture.registry, &fixture.scratch, 1024 * 1024, true)?;
    let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
    let locked = reply(&mut reader)?;
    assert_eq!(field(&locked, "state"), "locked");
    let marker = fixture.project.join("command-dispatched");
    send(
        &mut child,
        &json!({"action":"run","command":["/usr/bin/touch",marker],"timeout":3,
            "env":{(MODULE_SETUP_CAPTURE_ENV):"1",(LEGACY_MODULE_SETUP_CAPTURE_ENV):"0"}}),
    )?;
    let output = child.wait_with_output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr.contains("conflicting writer stage settings"), "{stderr}");
    assert!(stderr.contains(MODULE_SETUP_CAPTURE_ENV), "{stderr}");
    assert!(stderr.contains(LEGACY_MODULE_SETUP_CAPTURE_ENV), "{stderr}");
    let mut frame = String::new();
    assert_eq!(reader.read_line(&mut frame)?, 0, "{frame}");
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn raw_writer_payload_preserves_the_owner_directory() -> io::Result<()> {
    let fixture = StageFixture::new()?;
    let mut child = start_storage(&fixture.project, &fixture.registry, &fixture.scratch, 1024 * 1024, true)?;
    let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
    let ready = reply(&mut reader)?;
    assert_eq!(field(&ready, "state"), "locked");

    let marker = fixture.project.join("command-dispatched");
    for override_value in [Value::Null, json!(fixture.project.join("other-owners"))] {
        send(
            &mut child,
            &json!({"action":"run","command":["/usr/bin/touch",marker],"timeout":3,
                "env":{(OWNERS_DIRECTORY_ENV):override_value}}),
        )?;
        let refused = reply(&mut reader)?;
        assert_eq!(field(&refused, "state"), "stage_refused", "{refused}");
        assert_eq!(field(&refused, "owner"), field(&ready, "owner"));
        assert!(
            field(&refused, "reason")
                .as_str()
                .expect("owner binding refusal")
                .contains(OWNERS_DIRECTORY_ENV)
        );
        assert!(!marker.exists());
    }

    let expected = format!("{}\n", fixture.registry.display());
    for env in [json!({}), json!({(OWNERS_DIRECTORY_ENV):fixture.registry})] {
        send(
            &mut child,
            &json!({"action":"run","command":["/usr/bin/printenv",OWNERS_DIRECTORY_ENV],"timeout":3,"env":env}),
        )?;
        assert_eq!(field(&reply(&mut reader)?, "state"), "started");
        let stopped = reply(&mut reader)?;
        assert_eq!(field(&stopped, "state"), "stopped", "{stopped}");
        assert_eq!(field(&stopped, "exit_code"), 0, "{stopped}");
        assert_eq!(field(&stopped, "stdout").as_str(), Some(expected.as_str()));
    }
    send(&mut child, &json!({"action":"close"}))?;
    assert!(child.wait()?.success());
    assert!(fs::read_dir(&fixture.scratch)?.next().is_none());
    Ok(())
}

#[test]
fn sdk_writer_payload_preserves_the_owner_directory() -> io::Result<()> {
    const FIXTURE: &str = "LEAN_RS_TEST_WRITER_PAYLOAD_OWNER_FIXTURE";
    const TEST: &str = "stage_permit::sdk_writer_payload_preserves_the_owner_directory";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = StageFixture::new()?;
        let output = Command::new(std::env::current_exe()?)
            .args(["--exact", TEST, "--nocapture"])
            .env(FIXTURE, fixture.temp.path().canonicalize()?)
            .env(OWNERS_DIRECTORY_ENV, &fixture.registry)
            .env("TMPDIR", &fixture.scratch)
            .output()?;
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    };
    let base = Path::new(&base);
    let project = base.join("demo");
    let registry = base.join("owners");
    let helper = Path::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
    let timeout = Duration::from_secs(3);
    let mut writer = SharedWriter::open(helper, &project, timeout)?;
    let marker = project.join("command-dispatched");
    for override_value in [None, Some(base.join("other-owners"))] {
        let mut command = Command::new("/usr/bin/touch");
        command.arg(&marker);
        match override_value {
            Some(path) => command.env(OWNERS_DIRECTORY_ENV, path),
            None => command.env_remove(OWNERS_DIRECTORY_ENV),
        };
        let error = writer
            .run(&command, timeout)
            .expect_err("owner override must not dispatch");
        let refused = WriterFailure::from_io(&error).expect("typed pre-delivery refusal");
        assert_eq!(refused.disposition, WriterDisposition::NotStarted);
        assert!(refused.detail.contains(OWNERS_DIRECTORY_ENV), "{error}");
        assert!(!marker.exists());
    }
    for explicit in [false, true] {
        let mut command = Command::new("/usr/bin/printenv");
        command.arg(OWNERS_DIRECTORY_ENV);
        if explicit {
            command.env(OWNERS_DIRECTORY_ENV, &registry);
        }
        let output = writer.run(&command, timeout)?;
        assert!(output.status.success());
        assert_eq!(output.stdout, format!("{}\n", registry.display()).as_bytes());
    }
    writer.close(timeout)?;
    assert!(fs::read_dir(base.join("scratch"))?.next().is_none());
    Ok(())
}

#[test]
fn writer_stage_permit_refuses_delayed_delivery_and_cannot_be_replayed() -> io::Result<()> {
    let fixture = StageFixture::new()?;
    for case in ["expired", "wrong_boot", "wrong_birth", "fresh"] {
        let expired = case == "expired";
        let mut child = start_storage(&fixture.project, &fixture.registry, &fixture.scratch, 1024 * 1024, true)?;
        let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
        let ready = reply(&mut reader)?;
        let binding = field(&ready, "stage_ready");
        let owner = field(&ready, "owner");
        assert_eq!(field(binding, "pid"), child.id());
        assert_eq!(field(binding, "start"), field(owner, "start"));
        assert_eq!(field(binding, "session"), field(owner, "session"));
        let marker = fixture.project.join("entry");
        let cutoff = expiry(if expired {
            Duration::from_millis(30)
        } else {
            Duration::from_secs(10)
        })?;
        if expired {
            while guardian_deadline_ticks()?.as_nanos() < u128::from(cutoff) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let mut binding = binding.as_object().expect("stage binding").clone();
        match case {
            "wrong_boot" => {
                binding.insert("boot".into(), json!("00000000-0000-0000-0000-000000000000"));
            }
            "wrong_birth" => {
                binding.insert("start".into(), json!("other birth"));
            }
            _ => {}
        }
        let run = json!({"action":"run_permitted","command":["/usr/bin/touch", marker],
            "permit":{"ready":binding,"expires_ns":cutoff}});
        send(&mut child, &run)?;
        let result = reply(&mut reader)?;
        if expired {
            assert_eq!(field(&result, "state"), "stage_refused");
            assert_eq!(field(&result, "reason"), "writer stage permit expired");
            assert!(!marker.exists());
        } else if case != "fresh" {
            assert_eq!(field(&result, "state"), "stage_refused");
            assert_eq!(
                field(&result, "reason"),
                "writer permit differs from exact Ready helper"
            );
            assert!(!marker.exists());
        } else {
            assert_eq!(field(&result, "state"), "started");
            let stopped = reply(&mut reader)?;
            assert_eq!(field(&stopped, "state"), "stopped");
            assert_eq!(field(&stopped, "owned_work_stopped"), true);
            assert_eq!(field(&stopped, "exit_code"), 0);
            assert!(marker.is_file());
            fs::remove_file(&marker)?;
        }
        send(&mut child, &run)?;
        assert_eq!(
            field(&reply(&mut reader)?, "reason"),
            "writer stage permit is not available"
        );
        assert!(!marker.exists());
        send(&mut child, &json!({"action":"close"}))?;
        assert!(child.wait()?.success());
        assert!(fs::read_dir(&fixture.scratch)?.next().is_none());
    }
    Ok(())
}

#[test]
#[ignore = "requires the workspace-pinned Elan installation"]
fn permitted_sdk_writer_runs_real_lake_no_build() -> io::Result<()> {
    const FIXTURE: &str = "LEAN_RS_TEST_PERMITTED_NO_BUILD";
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = StageFixture::new()?;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "stage_permit::permitted_sdk_writer_runs_real_lake_no_build",
                "--ignored",
                "--nocapture",
            ])
            .env(FIXTURE, fixture.temp.path().canonicalize()?)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &fixture.registry)
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let project = Path::new(&base).join("demo");
    let pin = fs::read_to_string(workspace.join("lean-toolchain"))?;
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
    fs::write(project.join("lean-toolchain"), pin)?;
    fs::write(
        project.join("lakefile.toml"),
        "name = \"demo\"\n[[lean_lib]]\nname = \"Demo\"\n",
    )?;
    fs::write(
        project.join("lake-manifest.json"),
        br#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"demo","lakeDir":".lake"}"#,
    )?;
    let source = project.join("Demo.lean");
    fs::write(&source, "prelude\n")?;
    let helper = Path::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
    let timeout = Duration::from_secs(10);
    let base = Path::new(&base);
    let registry = base.join("owners");
    let scratch = base.join("scratch");
    let request = json!({"version":1,"floor_bytes":1,"budget_bytes":1024 * 1024,
        "roots":[root("project",&project),root("registry",&registry),root("scratch_parent",&scratch)]});
    let mut writer =
        SharedWriter::open_with_storage_request(helper, &project, &[], &request.to_string(), timeout, &|| false)?;
    let ready = writer.stage_ready()?.clone();
    assert_eq!(ready.pid(), ready.session());
    assert!(!ready.start().is_empty());
    assert!(!ready.boot().is_empty());
    let deadline = expiry(timeout)?;
    assert!(WriterStagePermit::new(ready.clone(), "00000000-0000-0000-0000-000000000000", deadline).is_err());
    let permit = WriterStagePermit::new(ready.clone(), ready.boot(), deadline)?;
    let mut command = Command::new(lake);
    command
        .current_dir(&project)
        .args(["--no-build", "--no-cache", "setup-file"])
        .arg(source)
        .env("LAKE_CACHE_DIR", writer.asset_dir())
        .env("LAKE_ARTIFACT_CACHE", "false")
        .env(MODULE_SETUP_CAPTURE_ENV, "1");
    let output = writer.run_permitted(&command, permit, &|| false)?;
    assert!(output.status.success(), "{output:?}");
    use sha2::{Digest as _, Sha256};
    let bytes = fs::read(writer.asset_dir().join("module-setup.json"))?;
    use std::fmt::Write as _;
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(&bytes) {
        write!(digest, "{byte:02x}").map_err(io::Error::other)?;
    }
    assert_eq!(output.stdout, digest.as_bytes());
    let setup: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(field(&setup, "name"), "Demo");
    let replay = writer
        .run_permitted(
            &command,
            WriterStagePermit::new(ready.clone(), ready.boot(), deadline)?,
            &|| false,
        )
        .expect_err("spent writer cannot dispatch again");
    assert_eq!(
        WriterFailure::from_io(&replay).expect("typed refusal").disposition,
        WriterDisposition::NotStarted
    );
    let assets = writer.asset_dir().to_owned();
    writer.close(timeout)?;
    assert!(!assets.exists());
    Ok(())
}

#[test]
fn locally_expired_permit_seals_the_public_writer_until_close() -> io::Result<()> {
    const FIXTURE: &str = "LEAN_RS_TEST_EXPIRED_WRITER";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = StageFixture::new()?;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "stage_permit::locally_expired_permit_seals_the_public_writer_until_close",
            ])
            .env(FIXTURE, fixture.temp.path().canonicalize()?)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &fixture.registry)
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let project = Path::new(&base).join("demo");
    let registry = Path::new(&base).join("owners");
    let scratch = Path::new(&base).join("scratch");
    let request = json!({"version":1,"floor_bytes":1,"budget_bytes":1024 * 1024,
        "roots":[root("project",&project),root("registry",&registry),root("scratch_parent",&scratch)]});
    let helper = Path::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
    let timeout = Duration::from_secs(10);
    let mut writer =
        SharedWriter::open_with_storage_request(helper, &project, &[], &request.to_string(), timeout, &|| false)?;
    let ready = writer.stage_ready()?.clone();
    let cutoff = expiry(Duration::from_millis(200))?;
    let permit = WriterStagePermit::new(ready.clone(), ready.boot(), cutoff)?;
    while guardian_deadline_ticks()?.as_nanos() < u128::from(cutoff) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let marker = project.join("entry");
    let mut command = Command::new("/usr/bin/touch");
    command.arg(&marker);
    let expired = writer
        .run_permitted(&command, permit, &|| false)
        .expect_err("expired locally");
    let refusal = WriterFailure::from_io(&expired).expect("typed prelaunch refusal");
    assert_eq!(refusal.disposition, WriterDisposition::NotStarted);
    assert!(refusal.detail.contains("writer stage permit expired"));
    assert!(writer.stage_ready().is_err());
    let replacement = WriterStagePermit::new(ready.clone(), ready.boot(), expiry(timeout)?)?;
    let refused = writer
        .run_permitted(&command, replacement, &|| false)
        .expect_err("fresh permit refused");
    assert!(refused.to_string().contains("writer scope is sealed"));
    let refused = writer.run(&command, timeout).expect_err("ordinary run also refused");
    assert!(refused.to_string().contains("writer scope is sealed"));
    writer.close(timeout)?;
    assert!(!marker.exists());
    assert!(fs::read_dir(scratch)?.next().is_none());
    Ok(())
}

#[test]
fn permitted_writer_waits_for_exact_helper_exit_after_control_closes() -> io::Result<()> {
    const FIXTURE: &str = "LEAN_RS_TEST_PERMITTED_CLOSE";
    const TEST: &str = "stage_permit::permitted_writer_waits_for_exact_helper_exit_after_control_closes";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = StageFixture::new()?;
        let status = Command::new(std::env::current_exe()?)
            .args(["--exact", TEST, "--nocapture"])
            .env(FIXTURE, fixture.temp.path().canonicalize()?)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &fixture.registry)
            .env("TMPDIR", fixture.temp.path().canonicalize()?)
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let base = Path::new(&base);
    let project = base.join("demo");
    let registry = base.join("owners");
    let scratch = base.join("scratch");
    let helper = Path::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
    let timeout = Duration::from_secs(10);
    let request = json!({"version":1,"floor_bytes":1,"budget_bytes":1024 * 1024,
        "roots":[root("project",&project),root("registry",&registry),root("scratch_parent",&scratch)]});
    for managed in [false, true] {
        let mut writer = if managed {
            SharedWriter::open_with_storage_request(helper, &project, &[], &request.to_string(), timeout, &|| false)?
        } else {
            SharedWriter::open(helper, &project, timeout)?
        };
        let ready = writer.stage_ready()?.clone();
        let cutoff = expiry(Duration::from_secs(1))?;
        let permit = WriterStagePermit::new(ready.clone(), ready.boot(), cutoff)?;
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let expired = || guardian_deadline_ticks().map_or(true, |ticks| ticks.as_nanos() >= u128::from(cutoff));
        let failure = writer
            .run_permitted(&command, permit, &expired)
            .expect_err("cancelled stage has no result");
        let failure = WriterFailure::from_io(&failure).expect("typed stop");
        assert_eq!(failure.disposition, WriterDisposition::Stopped);
        let stopped = failure.stop_receipt.as_ref().expect("exact command closure");
        assert_eq!(field(stopped, "owned_work_stopped"), true);
        let assets = writer.asset_dir().to_owned();
        let closed = writer.close(timeout);
        if managed {
            let error = closed.expect_err("EOF cannot settle a charged writer");
            assert!(error.to_string().contains("writer cleanup failed"), "{error}");
            assert!(assets.is_dir(), "the charge still owns scratch");
        } else {
            closed?;
            assert!(!assets.exists(), "successful helper exit followed cleanup");
        }
    }
    Ok(())
}

#[test]
fn writer_exit_observation_survives_failed_ready_and_close_timeout() -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    const REAL_FAILURE: &str = "LEAN_RS_TEST_REAL_WRITER_READY_FAILURE";
    const FIXTURE: &str = "LEAN_RS_TEST_WRITER_EXIT_OBSERVATION";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = StageFixture::new()?;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "stage_permit::writer_exit_observation_survives_failed_ready_and_close_timeout",
            ])
            .env(FIXTURE, fixture.temp.path().canonicalize()?)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &fixture.registry)
            .env("TMPDIR", fixture.temp.path().canonicalize()?)
            .status()?;
        assert!(status.success());
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "stage_permit::writer_exit_observation_survives_failed_ready_and_close_timeout",
            ])
            .env(FIXTURE, fixture.temp.path().canonicalize()?)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &fixture.registry)
            .env("TMPDIR", fixture.temp.path().join("missing-temp-directory"))
            .env(REAL_FAILURE, "1")
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let base = Path::new(&base);
    let project = base.join("demo");
    let timeout = Duration::from_secs(5);
    let mut observed = None;
    let open = |helper: &Path, observed: &mut Option<_>| {
        SharedWriter::open_with_write_roots_observed(helper, &project, &[], timeout, &|| false, observed)
    };
    let wait = |observation: &lean_toolchain::shared_writer::WriterExitObservation| {
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = observation.status() {
                return status;
            }
            assert!(started.elapsed() < timeout, "exact helper exit was not observed");
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    let handoff = |helper: &Path, prepared_roots: &[std::path::PathBuf], observed: &mut Option<_>| {
        SharedWriter::open_with_write_roots_handoff_observed(
            helper,
            &project,
            prepared_roots,
            timeout,
            &|| false,
            observed,
        )
    };
    let check_identities =
        |observed: &lean_toolchain::shared_writer::WriterExitObservation, roots: &[&Path]| -> io::Result<()> {
            assert_eq!(observed.lock_identities().len(), roots.len());
            for (identity, root) in observed.lock_identities().iter().zip(roots) {
                let path = lean_toolchain::shared_writer::project_lock_path(root)?;
                let meta = fs::metadata(&path)?;
                assert_eq!(
                    (&identity.path, identity.device, identity.inode),
                    (&path, meta.dev(), meta.ino())
                );
            }
            Ok(())
        };
    let helper = Path::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
    if std::env::var_os(REAL_FAILURE).is_some() {
        // The standalone helper fails before Ready when its asset directory
        // cannot be created, after it inherited the actual lock union.
        let extra = base.join("extra-workspace");
        fs::create_dir(&extra)?;
        assert!(handoff(helper, std::slice::from_ref(&extra), &mut observed).is_err());
        let failed = observed.expect("real helper was spawned");
        assert!(!wait(&failed).success());
        check_identities(&failed, &[&project, &extra])?;
        for identity in failed.lock_identities() {
            let file = fs::OpenOptions::new().read(true).write(true).open(&identity.path)?;
            file.try_lock().map_err(io::Error::other)?;
        }
        return Ok(());
    }

    let broken = base.join("bad-ready");
    let release = base.join("release-bad-ready");
    fs::write(
        &broken,
        "#!/bin/sh\nprintf '{}\\n'\nfor n in 1 2 3 4 5 6 7 8 9 10; do\n  [ ! -e \"$2/../release-bad-ready\" ] || exit 23\n  /bin/sleep 0.5\ndone\nexit 24\n",
    )?;
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o700))?;
    let error = handoff(&broken, &[], &mut observed).err().expect("invalid Ready");
    assert!(error.to_string().contains("ownership/lock-custody handshake failed"));
    let failed_ready = observed.take().expect("actual spawn observed");
    assert_eq!(failed_ready.status(), None);
    check_identities(&failed_ready, &[&project])?;
    let lock = lean_toolchain::shared_writer::project_lock_path(&project)?;
    let contender = fs::OpenOptions::new().read(true).write(true).open(lock)?;
    assert!(matches!(contender.try_lock(), Err(fs::TryLockError::WouldBlock)));
    fs::write(release, [])?;
    assert_eq!(wait(&failed_ready).code(), Some(23));
    assert_eq!(failed_ready.status().and_then(|status| status.code()), Some(23));
    contender.try_lock().map_err(io::Error::other)?;
    drop(contender);
    assert!(handoff(&base.join("absent"), &[], &mut observed).is_err());
    assert!(observed.is_none());

    let writer = open(helper, &mut observed)?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(writer.stage_ready()?.pid()).map_err(io::Error::other)?)
        .expect("exact disposable helper");
    struct Resume(rustix::process::Pid);
    impl Drop for Resume {
        fn drop(&mut self) {
            let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
        }
    }
    rustix::process::kill_process(pid, rustix::process::Signal::STOP)?;
    let resume = Resume(pid);
    let error = writer
        .close(Duration::from_millis(25))
        .expect_err("stopped helper cannot close yet");
    assert!(error.to_string().contains("writer close unconfirmed"));
    let pending = observed.expect("close retains the original spawn observation");
    assert_eq!(pending.status(), None);
    check_identities(&pending, &[&project])?;
    drop(resume);
    assert!(wait(&pending).success());
    Ok(())
}

#[test]
fn writer_handoff_leaves_no_caller_lock_alias() -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    const FIXTURE: &str = "LEAN_RS_TEST_WRITER_HANDOFF";
    const TEST: &str = "stage_permit::writer_handoff_leaves_no_caller_lock_alias";
    let command = || -> io::Result<Command> {
        let mut command = Command::new(std::env::current_exe()?);
        command.args(["--exact", TEST, "--nocapture"]);
        Ok(command)
    };
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = StageFixture::new()?;
        let base = fixture.temp.path().canonicalize()?;
        let status = command()?
            .env(FIXTURE, &base)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &fixture.registry)
            .env("TMPDIR", &base)
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let project = Path::new(&base).join("demo");
    let lock = lean_toolchain::shared_writer::project_lock_path(&project)?;
    let lock_identity = |metadata: &fs::Metadata| format!("{}:{}", metadata.dev(), metadata.ino());
    if let Ok(expected) = std::env::var("LEAN_RS_TEST_WRITER_HANDOFF_INODE") {
        let file = fs::OpenOptions::new().read(true).write(true).open(lock)?;
        assert_eq!(lock_identity(&file.metadata()?), expected);
        let result = file.try_lock();
        if std::env::var_os("LEAN_RS_TEST_WRITER_HANDOFF_BLOCKED").is_some() {
            assert!(matches!(result, Err(fs::TryLockError::WouldBlock)), "{result:?}");
        } else {
            result.map_err(io::Error::other)?;
        }
        return Ok(());
    }
    let helper = Path::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
    let timeout = Duration::from_secs(5);
    let mut observed = None;
    let mut writer =
        SharedWriter::open_with_write_roots_handoff_observed(helper, &project, &[], timeout, &|| false, &mut observed)?;
    let identity = lock_identity(&fs::metadata(&lock)?);
    let contender = |blocked| -> io::Result<()> {
        let mut attempt = command()?;
        attempt.env("LEAN_RS_TEST_WRITER_HANDOFF_INODE", &identity);
        if blocked {
            attempt.env("LEAN_RS_TEST_WRITER_HANDOFF_BLOCKED", "1");
        }
        assert!(attempt.status()?.success());
        Ok(())
    };
    contender(true)?;
    let ready = writer.stage_ready()?.clone();
    let cutoff = expiry(Duration::from_secs(1))?;
    let permit = WriterStagePermit::new(ready.clone(), ready.boot(), cutoff)?;
    let marker = project.join("handoff-stage-entered");
    let mut payload = Command::new("/bin/sh");
    payload
        .args(["-c", "touch \"$1\" && exec /bin/sleep 30", "handoff-stage"])
        .arg(&marker);
    let checked_during_payload = std::cell::Cell::new(false);
    let expired = || {
        if marker.is_file() && !checked_during_payload.replace(true) {
            contender(true).expect("independent contender while the payload is active");
        }
        guardian_deadline_ticks().map_or(true, |ticks| ticks.as_nanos() >= u128::from(cutoff))
    };
    let failure = writer
        .run_permitted(&payload, permit, &expired)
        .expect_err("deadline stops payload");
    let failure = WriterFailure::from_io(&failure).expect("exact command stop");
    assert_eq!(failure.disposition, WriterDisposition::Stopped);
    assert_eq!(
        field(
            failure.stop_receipt.as_ref().expect("owned closure"),
            "owned_work_stopped"
        ),
        true
    );

    assert!(
        checked_during_payload.get(),
        "the real payload reached the contention check"
    );

    let observed = observed.expect("helper exit observation");
    let until = std::time::Instant::now()
        .checked_add(timeout)
        .expect("bounded exit wait");
    while observed.status().is_none() {
        assert!(std::time::Instant::now() < until, "helper did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(observed.status().is_some_and(|status| status.success()));
    // The original handle is still alive and has never called close or Drop.
    contender(false)?;
    assert_eq!(writer.project_root(), project);
    writer.close(timeout)?;
    Ok(())
}
