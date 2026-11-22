use super::*;
use lean_toolchain::native_guardian::guardian_deadline_ticks;
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
        .env("LEAN_HOST_MCP_CAPTURE_MODULE_SETUP", "1");
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
