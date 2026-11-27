//! The standalone helper binary, driven through its control frames and through
//! the SDK client.
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use lean_rs_writer::log::{LOG_ENV, LOG_LIMIT_ENV};
use lean_toolchain::native_guardian::OWNERS_DIRECTORY_ENV;
use lean_toolchain::shared_writer::{
    SharedWriter, WriterDisposition, WriterExitObservation, WriterFailure, project_lock_path,
};
use serde_json::{Value, json};

const HELPER: &str = env!("CARGO_BIN_EXE_lean-rs-writer");

fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or(&Value::Null)
}

fn missing(what: &str) -> io::Error {
    io::Error::other(format!("{what} missing"))
}

/// A project, a private owner registry and a scratch directory under target/.
struct Fixture {
    temp: tempfile::TempDir,
    project: PathBuf,
    registry: PathBuf,
    scratch: PathBuf,
}

impl Fixture {
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

    fn base(&self) -> io::Result<PathBuf> {
        self.temp.path().canonicalize()
    }
}

/// Run the named test of this binary again, in a child that carries `env`.
fn rerun(test: &str, env: &[(&str, &Path)]) -> io::Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["--exact", test, "--nocapture"]);
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // libtest exits zero when no test matches the name.
    assert!(stdout.contains("running 1 test"), "{stdout}");
    Ok(())
}

/// Resumes a stopped process when dropped, so a failed assertion cannot leave
/// the helper stopped.
struct Resume(rustix::process::Pid);

impl Drop for Resume {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

fn reply(reader: &mut BufReader<ChildStdout>) -> io::Result<Value> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

fn send(child: &mut Child, frame: &Value) -> io::Result<()> {
    writeln!(
        child.stdin.as_mut().ok_or_else(|| missing("writer control"))?,
        "{frame}"
    )
}

#[test]
fn lock_descriptor_names_protocol_root_and_lock() -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    let output = Command::new(HELPER).arg("writer-lock-path").arg(&root).output()?;
    assert!(output.status.success(), "{output:?}");
    let descriptor: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        descriptor,
        json!({"protocol":1,"project_root":root,"lock_path":project_lock_path(&root)?})
    );
    Ok(())
}

#[test]
fn raw_writer_payload_preserves_the_owner_directory() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let mut child = Command::new(HELPER)
        .arg("writer")
        .arg(&fixture.project)
        .arg(project_lock_path(&fixture.project)?)
        .arg("3")
        .env(OWNERS_DIRECTORY_ENV, &fixture.registry)
        .env("TMPDIR", &fixture.scratch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut reader = BufReader::new(child.stdout.take().ok_or_else(|| missing("writer replies"))?);
    let ready = reply(&mut reader)?;
    assert_eq!(field(&ready, "state"), "locked", "{ready}");

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
                .is_some_and(|reason| reason.contains(OWNERS_DIRECTORY_ENV)),
            "{refused}"
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
    assert!(
        fs::read_dir(&fixture.scratch)?.next().is_none(),
        "the helper left its asset directory"
    );
    Ok(())
}

#[test]
fn sdk_writer_payload_preserves_the_owner_directory() -> io::Result<()> {
    const FIXTURE: &str = "LEAN_RS_TEST_WRITER_PAYLOAD_OWNER_FIXTURE";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = Fixture::new()?;
        return rerun(
            "sdk_writer_payload_preserves_the_owner_directory",
            &[
                (FIXTURE, fixture.base()?.as_path()),
                (OWNERS_DIRECTORY_ENV, fixture.registry.as_path()),
                ("TMPDIR", fixture.scratch.as_path()),
            ],
        );
    };
    let base = Path::new(&base);
    let project = base.join("demo");
    let registry = base.join("owners");
    let timeout = Duration::from_secs(3);
    let mut writer = SharedWriter::open(Path::new(HELPER), &project, timeout)?;
    let marker = project.join("command-dispatched");
    for override_value in [None, Some(base.join("other-owners"))] {
        let mut command = Command::new("/usr/bin/touch");
        command.arg(&marker);
        match override_value {
            Some(path) => command.env(OWNERS_DIRECTORY_ENV, path),
            None => command.env_remove(OWNERS_DIRECTORY_ENV),
        };
        let Err(error) = writer.run(&command, timeout) else {
            return Err(io::Error::other("an owner override dispatched its command"));
        };
        let refused = WriterFailure::from_io(&error).ok_or_else(|| missing("typed refusal"))?;
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
fn sdk_writer_logs_large_output_and_survives_a_refused_log() -> io::Result<()> {
    const TEST: &str = "sdk_writer_logs_large_output_and_survives_a_refused_log";
    const FIXTURE: &str = "LEAN_RS_TEST_WRITER_LOG_FIXTURE";
    const TRUNCATED: &[u8] = b"\n[lean-rs-writer: log limit reached; later output dropped]\n";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = Fixture::new()?;
        return rerun(
            TEST,
            &[
                (FIXTURE, fixture.base()?.as_path()),
                (OWNERS_DIRECTORY_ENV, fixture.registry.as_path()),
                ("TMPDIR", fixture.scratch.as_path()),
            ],
        );
    };
    let base = Path::new(&base);
    let project = base.join("demo");
    let timeout = Duration::from_secs(3);
    let mut writer = SharedWriter::open(Path::new(HELPER), &project, timeout)?;

    // A log without its cap is refused before launch, and the writer stays open.
    let marker = project.join("command-dispatched");
    let mut command = Command::new("/usr/bin/touch");
    command.arg(&marker).env(LOG_ENV, base.join("refused.log"));
    let Err(error) = writer.run(&command, timeout) else {
        return Err(io::Error::other("a log without a cap dispatched its command"));
    };
    let refused = WriterFailure::from_io(&error).ok_or_else(|| missing("typed refusal"))?;
    assert_eq!(refused.disposition, WriterDisposition::NotStarted);
    assert!(refused.detail.contains(LOG_LIMIT_ENV), "{error}");
    assert!(!marker.exists());

    // So is a log path the helper will not open.
    let elsewhere = base.join("elsewhere.log");
    fs::write(&elsewhere, [])?;
    let link = base.join("link.log");
    std::os::unix::fs::symlink(&elsewhere, &link)?;
    let mut command = Command::new("/usr/bin/touch");
    command.arg(&marker).env(LOG_ENV, &link).env(LOG_LIMIT_ENV, "100");
    let Err(error) = writer.run(&command, timeout) else {
        return Err(io::Error::other("a linked log dispatched its command"));
    };
    let refused = WriterFailure::from_io(&error).ok_or_else(|| missing("typed refusal"))?;
    assert_eq!(refused.disposition, WriterDisposition::NotStarted);
    assert!(!marker.exists());

    // Output far past the 64 KiB capture limit goes to the log, which keeps
    // its cap, and the stage still finishes with a result.
    let log = base.join("build.log");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "printf note; /usr/bin/head -c 200000 /dev/zero; /usr/bin/head -c 200000 /dev/zero >&2",
        ])
        .env(LOG_ENV, &log)
        .env(LOG_LIMIT_ENV, "100000");
    let output = writer.run(&command, timeout)?;
    assert!(output.status.success());
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let bytes = fs::read(&log)?;
    assert_eq!(bytes.len(), 100_000 + TRUNCATED.len());
    assert!(bytes.starts_with(b"note\0"));
    assert!(bytes.ends_with(TRUNCATED));

    // The stage never sees the log settings.
    let log = base.join("env.log");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            &format!("printf %s \"${{{LOG_ENV}-unset}} ${{{LOG_LIMIT_ENV}-unset}}\""),
        ])
        .env(LOG_ENV, &log)
        .env(LOG_LIMIT_ENV, "100");
    assert!(writer.run(&command, timeout)?.status.success());
    assert_eq!(fs::read(&log)?, b"unset unset");
    writer.close(timeout)?;
    Ok(())
}

#[test]
fn writer_exit_observation_survives_failed_ready_and_close_timeout() -> io::Result<()> {
    const TEST: &str = "writer_exit_observation_survives_failed_ready_and_close_timeout";
    const REAL_FAILURE: &str = "LEAN_RS_TEST_REAL_WRITER_READY_FAILURE";
    const FIXTURE: &str = "LEAN_RS_TEST_WRITER_EXIT_OBSERVATION";
    let Some(base) = std::env::var_os(FIXTURE) else {
        let fixture = Fixture::new()?;
        let base = fixture.base()?;
        let shared = [
            (FIXTURE, base.as_path()),
            (OWNERS_DIRECTORY_ENV, fixture.registry.as_path()),
        ];
        rerun(TEST, &[shared.as_slice(), &[("TMPDIR", base.as_path())]].concat())?;
        let missing_temp = base.join("missing-temp-directory");
        let failing = [("TMPDIR", missing_temp.as_path()), (REAL_FAILURE, Path::new("1"))];
        return rerun(TEST, &[shared.as_slice(), failing.as_slice()].concat());
    };
    let base = Path::new(&base);
    let project = base.join("demo");
    let timeout = Duration::from_secs(5);
    let mut observed = None;
    let open = |helper: &Path, prepared_roots: &[PathBuf], observed: &mut Option<_>| {
        SharedWriter::open_with_write_roots_observed(helper, &project, prepared_roots, timeout, &|| false, observed)
    };
    let wait = |observation: &WriterExitObservation| {
        let started = Instant::now();
        loop {
            if let Some(status) = observation.status() {
                return status;
            }
            assert!(started.elapsed() < timeout, "exact helper exit was not observed");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let check_identities = |observed: &WriterExitObservation, roots: &[&Path]| -> io::Result<()> {
        assert_eq!(observed.lock_identities().len(), roots.len());
        for (identity, root) in observed.lock_identities().iter().zip(roots) {
            let path = project_lock_path(root)?;
            let meta = fs::metadata(&path)?;
            assert_eq!(
                (&identity.path, identity.device, identity.inode),
                (&path, meta.dev(), meta.ino())
            );
        }
        Ok(())
    };
    let helper = Path::new(HELPER);
    if std::env::var_os(REAL_FAILURE).is_some() {
        // The standalone helper fails before Ready when it cannot create its
        // asset directory, after it inherited every held lock.
        let extra = base.join("extra-workspace");
        fs::create_dir(&extra)?;
        assert!(open(helper, std::slice::from_ref(&extra), &mut observed).is_err());
        let failed = observed.ok_or_else(|| missing("spawn observation"))?;
        assert!(!wait(&failed).success());
        check_identities(&failed, &[&project, &extra])?;
        for identity in failed.lock_identities() {
            let file = fs::OpenOptions::new().read(true).write(true).open(&identity.path)?;
            file.try_lock().map_err(io::Error::other)?;
        }
        return Ok(());
    }

    let broken = base.join("bad-ready");
    fs::write(
        &broken,
        concat!(
            "#!/bin/sh\nprintf '{}\\n'\nfor n in 1 2 3 4 5 6 7 8 9 10; do\n",
            "  [ ! -e \"$2/../release-bad-ready\" ] || exit 23\n  /bin/sleep 0.5\ndone\nexit 24\n",
        ),
    )?;
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o700))?;
    let Err(error) = open(&broken, &[], &mut observed) else {
        return Err(io::Error::other("an invalid Ready frame was accepted"));
    };
    assert!(error.to_string().contains("ownership/lock-custody handshake failed"));
    let failed_ready = observed.take().ok_or_else(|| missing("spawn observation"))?;
    assert_eq!(failed_ready.status(), None);
    check_identities(&failed_ready, &[&project])?;
    let lock = project_lock_path(&project)?;
    let contender = fs::OpenOptions::new().read(true).write(true).open(&lock)?;
    assert!(matches!(contender.try_lock(), Err(fs::TryLockError::WouldBlock)));
    fs::write(base.join("release-bad-ready"), [])?;
    assert_eq!(wait(&failed_ready).code(), Some(23));
    assert_eq!(failed_ready.status().and_then(|status| status.code()), Some(23));
    contender.try_lock().map_err(io::Error::other)?;
    drop(contender);
    assert!(open(&base.join("absent"), &[], &mut observed).is_err());
    assert!(observed.is_none());

    let writer = open(helper, &[], &mut observed)?;
    // The helper records itself as owner in the lock before it reports Ready.
    let record: Value = serde_json::from_reader(fs::File::open(&lock)?)?;
    let pid = field(field(&record, "owner"), "pid")
        .as_i64()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| missing("helper pid"))?;
    rustix::process::kill_process(pid, rustix::process::Signal::STOP)?;
    let resume = Resume(pid);
    let Err(error) = writer.close(Duration::from_millis(25)) else {
        return Err(io::Error::other("a stopped helper confirmed close"));
    };
    assert!(error.to_string().contains("writer close unconfirmed"), "{error}");
    let pending = observed.ok_or_else(|| missing("spawn observation"))?;
    assert_eq!(pending.status(), None);
    check_identities(&pending, &[&project])?;
    drop(resume);
    assert!(wait(&pending).success());
    Ok(())
}
