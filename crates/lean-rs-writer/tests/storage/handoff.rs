//! Exercise staging through the standalone executable and its ordinary EX locks.
use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;

const LIMIT: usize = 262_144;
// SHA-256 of LIMIT copies of the byte 'x'.
const DIGEST: &str = "d509bff642a353f88582e8a846ecae041c333b79c57a7a24ff310fbdb7e914e9";

struct Fixture {
    _temp: tempfile::TempDir,
    project: PathBuf,
    registry: PathBuf,
    scratch: PathBuf,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
        let base = temp.path().canonicalize()?;
        let project = base.join("project");
        let registry = base.join("registry");
        let scratch = base.join("scratch");
        for path in [&project, &registry, &scratch] {
            fs::create_dir(path)?;
        }
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            _temp: temp,
            project,
            registry,
            scratch,
        })
    }

    fn open(&self, budget: usize, interrupt_write: bool) -> io::Result<(Child, BufReader<ChildStdout>, Value)> {
        let bin = env!("CARGO_BIN_EXE_lean-rs-writer");
        let mut command = if interrupt_write {
            // Only this child inherits the limit. No debugger, timing race, or
            // production fault switch: the kernel interrupts a real partial write.
            let mut shell = Command::new("/bin/sh");
            shell.args(["-c", "ulimit -c 0 && ulimit -f 32 && exec \"$@\"", "limit", bin]);
            shell
        } else {
            Command::new(bin)
        };
        let lock = lean_toolchain::shared_writer::project_lock_path(&self.project)?;
        let request = json!({"version":1,"command_only":true,"handoff_sha256":DIGEST,
            "floor_bytes":1,"budget_bytes":budget,"roots":[root("project",&self.project),
                root("registry",&self.registry),root("scratch_parent",&self.scratch)]});
        let mut child = command
            .arg("writer")
            .arg(&self.project)
            .arg(lock)
            .args(["3", "--storage-request"])
            .arg(request.to_string())
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &self.registry)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut reader = BufReader::new(child.stdout.take().expect("writer replies"));
        let locked = reply(&mut reader)?;
        assert_eq!(field(&locked, "state"), "locked");
        Ok((child, reader, locked))
    }
}

fn turn(locked: &Value) -> &str {
    field(field(locked, "handoff"), "turn").as_str().expect("admitted turn")
}

fn stage(turn: &str, data: &str) -> Value {
    json!({"action":"stage_handoff","turn":turn,"data_hex":data})
}

#[test]
fn interrupted_staging_is_reclaimed_without_replay() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let (mut writer, mut reader, locked) = fixture.open(LIMIT, true)?;
    let original = turn(&locked);
    let scratch = Path::new(field(&locked, "asset_dir").as_str().expect("scratch path"));
    let payload = "78".repeat(LIMIT);
    send(&mut writer, &stage(original, &payload))?;
    drop(writer.stdin.take());
    let status = writer.wait()?;
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::XFSZ.as_raw()),
        "{status}"
    );
    assert_eq!(reader.read_line(&mut String::new())?, 0, "no staging acknowledgement");
    let partial = scratch.join("handoff.json").metadata()?.len();
    assert!((1..LIMIT as u64).contains(&partial));
    let charge = fixture.registry.join(".writer-storage-v1").join(original);
    assert!(charge.is_file());
    let (mut next, _replies, next_locked) = fixture.open(LIMIT, false)?;
    assert_ne!(turn(&next_locked), original);
    assert!(!scratch.exists());
    assert!(!charge.exists());
    send(&mut next, &stage(original, &payload))?;
    let rejected = next.wait_with_output()?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("wrong handoff turn"));
    Ok(())
}

#[test]
fn only_one_matching_stage_can_release_a_command() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let mut prior = None;
    let marker = fixture.project.join("ran");
    let go = json!({"action":"run","command":["/usr/bin/touch",marker],"timeout":3});
    for (fault, reason) in [
        ("absent", "requires a staged handoff"),
        ("digest", "differ from the admitted digest"),
        ("oversize", "invalid handoff size"),
        ("overbudget", "invalid handoff size"),
        ("late", "duplicate stage"),
    ] {
        let budget = if fault == "overbudget" { 1 } else { LIMIT + 1 };
        let (mut writer, mut reader, locked) = fixture.open(budget, false)?;
        if let Some(previous) = prior.take() {
            assert!(!Path::new(&previous).exists());
        }
        let scratch = PathBuf::from(field(&locked, "asset_dir").as_str().expect("scratch path"));
        let id = turn(&locked);
        let frame = match fault {
            "absent" => go.clone(),
            "digest" => stage(id, "79"),
            "oversize" => stage(id, &"78".repeat(LIMIT + 1)),
            "overbudget" => stage(id, "7878"),
            _ => {
                send(&mut writer, &stage(id, &"78".repeat(LIMIT)))?;
                assert_eq!(
                    reply(&mut reader)?,
                    json!({"state":"handoff_staged","turn":id,
                    "handoff_sha256":DIGEST,"bytes":LIMIT,"name":"handoff.json"})
                );
                assert_eq!(fs::read(scratch.join("handoff.json"))?, vec![b'x'; LIMIT]);
                send(
                    &mut writer,
                    &json!({"action":"run","command":["/usr/bin/true"],"timeout":3}),
                )?;
                assert_eq!(field(&reply(&mut reader)?, "state"), "started");
                assert_eq!(field(&reply(&mut reader)?, "owned_work_stopped"), true);
                stage(id, "78")
            }
        };
        // Queue GO behind the invalid frame. No following command may start.
        send(&mut writer, &frame)?;
        if let Err(error) = send(&mut writer, &go) {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        let rejected = writer.wait_with_output()?;
        assert!(!rejected.status.success());
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains(reason),
            "{rejected:?}"
        );
        assert_eq!(reader.read_line(&mut String::new())?, 0);
        assert!(!marker.exists());
        prior = Some(scratch);
    }
    let (mut writer, mut reader, locked) = fixture.open(LIMIT, false)?;
    assert!(!prior.expect("previous scratch").exists());
    let id = turn(&locked);
    send(&mut writer, &stage(id, &"78".repeat(LIMIT)))?;
    assert_eq!(field(&reply(&mut reader)?, "state"), "handoff_staged");
    send(&mut writer, &json!({"action":"close"}))?;
    assert!(writer.wait()?.success());
    assert!(!fixture.registry.join(".writer-storage-v1").join(id).exists());
    assert_eq!(fs::read_dir(&fixture.scratch)?.count(), 0);
    Ok(())
}
