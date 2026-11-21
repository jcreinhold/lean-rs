//! Exercise the operator decision with real registry charges and both EX locks.
#![allow(
    clippy::indexing_slicing,
    reason = "the fixture asserts exact JSON fields and owns two lock descriptors"
)]
use super::*;
use lean_toolchain::native_guardian::writer_storage::StorageRoot;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;

fn root(path: &Path) -> io::Result<StorageRoot> {
    let stat = path.metadata()?;
    Ok(StorageRoot {
        path: path.to_owned(),
        device: stat.dev(),
        inode: stat.ino(),
    })
}

#[test]
fn exact_turn_checks_each_binding_and_settles_once() -> io::Result<()> {
    const CHILD: &str = "LEAN_RS_TEST_EXACT_TURN_DIRECTORY";
    let Some(fixture) = std::env::var_os(CHILD) else {
        let temp = tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))?;
        let fixture = temp.path().canonicalize()?;
        let registry = fixture.join("registry");
        fs::create_dir(&registry)?;
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "exact_turn::exact_turn_checks_each_binding_and_settles_once",
                "--nocapture",
            ])
            .env(CHILD, &fixture)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", registry)
            .status()?;
        assert!(status.success());
        return Ok(());
    };
    let fixture = PathBuf::from(fixture);
    let registry = root(&fixture.join("registry"))?;
    let project = fixture.join("project");
    let dependency = fixture.join("dependency");
    let sibling = fixture.join("other-root");
    let scratch_parent = fixture.join("scratch");
    for p in [&project, &dependency, &sibling, &scratch_parent] {
        fs::create_dir(p)?;
    }
    let primary = lean_toolchain::shared_writer::project_lock_path(&project)?;
    let secondary = lean_toolchain::shared_writer::project_lock_path(&dependency)?;
    // SHA-256 of the single byte 'x'.
    let digest = "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881";
    let generation = approve(&registry.path)?;
    let request = json!({"version":1,"command_only":true,"handoff_sha256":digest,"approval_generation":generation,"floor_bytes":1,"budget_bytes":1,
        "roots":[super::root("project",&project),super::root("registry",&registry.path),
            super::root("scratch_parent",&scratch_parent),super::root("outputs",&sibling)]});
    for (field, replacement) in [
        ("handoff_sha256", Some(Value::Null)),
        ("handoff_sha256", Some(json!("bad digest"))),
        ("command_only", Some(json!(false))),
        ("approval_generation", None),
        ("approval_generation", Some(Value::Null)),
        ("approval_generation", Some(json!("bad generation"))),
        ("handoff_sha256", None),
    ] {
        let mut bad = request.clone();
        if let Some(value) = replacement {
            bad[field] = value;
        } else {
            bad.as_object_mut().expect("storage request").remove(field);
        }
        let out = Command::new(env!("CARGO_BIN_EXE_lean-rs-writer"))
            .arg("writer")
            .arg(&project)
            .arg(&primary)
            .args(["3", "--storage-request"])
            .arg(bad.to_string())
            .output()?;
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(!registry.path.join(".writer-storage-v1").exists());
    }
    let mut writer = Command::new(env!("CARGO_BIN_EXE_lean-rs-writer"))
        .arg("writer")
        .arg(&project)
        .arg(&primary)
        .args(["3", "--storage-request"])
        .arg(request.to_string())
        .env(
            "LEAN_HOST_MCP_WRITER_WORKSPACES",
            json!([project, dependency]).to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut reader = BufReader::new(writer.stdout.take().expect("writer replies"));
    let ready = reply(&mut reader)?;
    let receipt = &ready["handoff"];
    let turn = receipt["turn"].as_str().expect("turn").to_owned();
    let scratch = PathBuf::from(ready["asset_dir"].as_str().expect("bound scratch"));
    assert_eq!(receipt["handoff_sha256"], digest);
    let identities = [&primary, &secondary]
        .into_iter()
        .map(|path| {
            let m = path.metadata()?;
            Ok(json!({"path":path,"device":m.dev(),"inode":m.ino()}))
        })
        .collect::<io::Result<Vec<_>>>()?;
    assert_eq!(receipt["locks"], json!(identities));
    let operator = |action: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lean-rs-writer"));
        command
            .args(["writer-storage", action, "--registry"])
            .arg(&registry.path)
            .arg("--registry-device")
            .arg(registry.device.to_string())
            .arg("--registry-inode")
            .arg(registry.inode.to_string())
            .arg("--turn")
            .arg(&turn);
        command
    };
    let inspect = |recover| {
        let mut command = operator(if recover { "recover" } else { "inspect" });
        if recover {
            command.arg("--acknowledge-unknown-result");
        }
        let output = command.output().expect("operator command");
        let report: Value = serde_json::from_slice(&output.stdout).expect("operator JSON report");
        assert_eq!(report["old_result"], "unknown");
        assert_eq!(
            output.status.success(),
            matches!(report["state"].as_str(), Some("eligible" | "settled"))
        );
        report
    };
    assert_eq!(inspect(true)["state"], "refused");
    send(
        &mut writer,
        &json!({"action":"stage_handoff","turn":turn,"data_hex":"78"}),
    )?;
    assert_eq!(
        reply(&mut reader)?,
        json!({"state":"handoff_staged","turn":turn,
        "handoff_sha256":digest,"bytes":1,"name":"handoff.json"})
    );
    send(
        &mut writer,
        &json!({"action":"run","command":["/bin/cat",scratch.join("handoff.json")],"timeout":3}),
    )?;
    assert_eq!(reply(&mut reader)?["state"], "started");
    let stopped = reply(&mut reader)?;
    assert_eq!(stopped["owned_work_stopped"], true);
    assert_eq!(stopped["exit_code"], 0);
    assert_eq!(stopped["stdout"], "x");
    let deep = (0..35).fold(scratch.clone(), |p, _| p.join("child"));
    send(
        &mut writer,
        &json!({"action":"run","command":["/bin/mkdir","-p",deep],"timeout":3}),
    )?;
    assert_eq!(reply(&mut reader)?["state"], "started");
    let stopped = reply(&mut reader)?;
    assert_eq!(stopped["owned_work_stopped"], true);
    assert_eq!(stopped["exit_code"], 0);
    drop(writer.stdin.take());
    assert!(!writer.wait()?.success());
    let preview = inspect(false);
    assert_eq!(preview["state"], "eligible", "{preview}");
    assert_eq!(preview["old_result"], "unknown");
    let entry = registry.path.join(".writer-storage-v1").join(&turn);
    let saved_charge = fs::read(&entry)?;
    let mut pre_staging: Value = serde_json::from_slice(&saved_charge)?;
    pre_staging
        .as_object_mut()
        .expect("charge")
        .remove("helper_stages_handoff");
    let pre_staging = serde_json::to_vec(&pre_staging)?;
    fs::write(&entry, &pre_staging)?;
    let refused = inspect(true);
    assert_eq!(refused["state"], "refused");
    assert!(
        refused["reason"]
            .as_str()
            .is_some_and(|s| s.contains("staging") || s.contains("scope"))
    );
    assert_eq!(fs::read(&entry)?, pre_staging);
    assert!(scratch.is_dir());
    fs::write(&entry, &saved_charge)?;
    let unacknowledged = operator("recover").output()?;
    assert_eq!(unacknowledged.status.code(), Some(2));
    assert!(unacknowledged.stdout.is_empty());
    assert!(String::from_utf8_lossy(&unacknowledged.stderr).contains("--acknowledge-unknown-result"));
    assert_eq!(fs::read(&entry)?, saved_charge);
    assert!(scratch.is_dir());
    let observed = lean_toolchain::native_guardian::writer_storage::WriterStorageTurn::inspect(&registry, &turn)?;
    let original_entry = fixture.join("original-charge");
    fs::rename(&entry, &original_entry)?;
    fs::write(&entry, &saved_charge)?;
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o600))?;
    let refusal = observed.try_locks(
        std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(1))
            .expect("deadline"),
    );
    assert!(refusal.is_err_and(|e| e.to_string().contains("changed since inspection")));
    fs::rename(original_entry, &entry)?;
    let admitted: Value = serde_json::from_slice(&saved_charge)?;
    assert_eq!(admitted["handoff_sha256"], receipt["handoff_sha256"]);
    assert_eq!(admitted["primary"], receipt["locks"][0]["path"]);
    assert_eq!(admitted["device"], receipt["locks"][0]["device"]);
    assert_eq!(admitted["inode"], receipt["locks"][0]["inode"]);
    let secondary_id = &receipt["locks"][1];
    assert_eq!(
        admitted["secondary_locks"],
        json!([[secondary_id["path"], secondary_id["device"], secondary_id["inode"]]])
    );
    assert!(admitted["origin"]["host"].as_str().is_some_and(|id| id.len() == 64));
    assert!(admitted["origin"]["boot"].as_str().is_some_and(|id| id.len() == 36));
    assert!(scratch.is_dir());
    let original = fs::read(&secondary)?;
    let mut starting: Value = serde_json::from_slice(&original)?;
    let created = scratch.metadata()?;
    assert!(
        starting["scratch_identity"]
            .as_str()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    );
    assert_eq!(fs::read(&primary)?, original);
    // Earlier source-bound turns lack the independent identity on their locks.
    let mut earlier = starting.clone();
    earlier.as_object_mut().expect("owner").remove("scratch_identity");
    for lock in [&primary, &secondary] {
        fs::write(lock, format!("{earlier}\n"))?;
    }
    for recover in [false, true] {
        let report = inspect(recover);
        assert_eq!(report["state"], "refused", "{report}");
        assert!(
            report["reason"]
                .as_str()
                .is_some_and(|s| s.contains("scratch identity"))
        );
        assert_eq!(fs::read(&entry)?, saved_charge);
    }
    // The unpublished inode-only pair cannot establish the original parent.
    earlier["scratch_identity"] = json!([created.dev(), created.ino()]);
    for lock in [&primary, &secondary] {
        fs::write(lock, format!("{earlier}\n"))?;
    }
    for recover in [false, true] {
        assert_eq!(inspect(recover)["state"], "refused");
        assert_eq!(fs::read(&entry)?, saved_charge);
    }
    fs::write(&primary, &original)?;
    fs::write(&secondary, &original)?;
    starting["command_phase"] = json!("starting");
    let starting = format!("{starting}\n");
    fs::write(&secondary, &starting)?;
    assert_eq!(inspect(true)["state"], "refused");
    fs::write(&primary, &starting)?;
    let refused = inspect(true);
    assert_eq!(refused["command_phase"], "starting");
    assert_eq!(refused["state"], "refused");
    fs::write(&primary, &original)?;
    fs::write(&secondary, &original)?;
    fs::rename(&sibling, fixture.join("original-root"))?;
    fs::create_dir(&sibling)?;
    assert_eq!(inspect(true)["state"], "refused");
    fs::remove_dir(&sibling)?;
    fs::rename(fixture.join("original-root"), &sibling)?;
    fs::rename(&secondary, fixture.join("original-lock"))?;
    fs::write(&secondary, &original)?;
    assert_eq!(inspect(true)["state"], "refused");
    fs::remove_file(&secondary)?;
    fs::rename(fixture.join("original-lock"), &secondary)?;
    let mut legacy: Value = serde_json::from_slice(&saved_charge)?;
    legacy
        .as_object_mut()
        .ok_or_else(|| io::Error::other("fixture charge"))?
        .remove("command_only");
    fs::write(&entry, serde_json::to_vec(&legacy)?)?;
    assert!(
        inspect(true)["reason"]
            .as_str()
            .is_some_and(|s| s.contains("caller-owned"))
    );
    fs::write(&entry, &saved_charge)?;
    // Cut after removal: absent scratch alone proves nothing.
    let displaced = fixture.join("original-scratch");
    fs::rename(&scratch, &displaced)?;
    assert_eq!(inspect(true)["state"], "refused");
    assert_eq!(fs::read(&entry)?, saved_charge);
    // A fresh inspection sees a valid charge naming the replacement inode.
    // Only the independent record on the original EX union can disprove it.
    fs::create_dir(&scratch)?;
    let replacement = root(&scratch)?;
    let check_rewritten_charge = |substitute: &StorageRoot| -> io::Result<()> {
        let original = fs::read(&entry)?;
        let mut changed: Value = serde_json::from_slice(&original)?;
        changed["scratch"] = serde_json::to_value(substitute)?;
        let rewritten = serde_json::to_vec(&changed)?;
        fs::write(&entry, &rewritten)?;
        for recover in [false, true] {
            let report = inspect(recover);
            assert_eq!(report["reservation"]["scratch"], changed["scratch"]);
            assert_eq!(report["state"], "refused", "{report}");
            assert!(
                report["reason"]
                    .as_str()
                    .is_some_and(|s| s.contains("scratch identity")),
                "{report}"
            );
            assert_eq!(fs::read(&entry)?, rewritten);
        }
        fs::write(&entry, original)
    };
    fs::write(scratch.join("sentinel"), b"keep")?;
    check_rewritten_charge(&replacement)?;
    assert_eq!(fs::read(scratch.join("sentinel"))?, b"keep");
    fs::rename(&scratch, fixture.join("replacement"))?;
    fs::rename(&displaced, &scratch)?;
    // A disjoint admission attempts cleanup but leaves the deep turn charged.
    let other = fixture.join("disjoint-project");
    fs::create_dir(&other)?;
    let mut disjoint = start(&other, &registry.path, &scratch_parent, 1)?;
    let mut replies = BufReader::new(disjoint.stdout.take().expect("disjoint writer replies"));
    assert_eq!(reply(&mut replies)?["state"], "locked");
    assert!(deep.is_dir());
    let marked: Value = serde_json::from_slice(&fs::read(&entry)?)?;
    assert_eq!(marked["scratch_cleanup_started"], true);
    send(&mut disjoint, &json!({"action":"close"}))?;
    assert!(disjoint.wait()?.success());

    // Keep the real scratch and its inode pair. Only redirect its name under
    // another admitted root, where a marked cleanup would see absence.
    let mut redirected = root(&scratch)?;
    redirected.path = sibling.join(scratch.file_name().expect("turn name"));
    assert!(!redirected.path.try_exists()?);
    check_rewritten_charge(&redirected)?;
    assert!(deep.is_dir());
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(&entry)?)?, marked);

    // With the name absent, the cleanup mark cannot authenticate an edited charge.
    fs::rename(&scratch, &displaced)?;
    check_rewritten_charge(&replacement)?;
    assert_eq!(inspect(false)["state"], "eligible");
    fs::rename(&displaced, &scratch)?;

    // Interrupt the actual operator walk with a kernel descriptor limit.
    // The normal retry resumes using the original directory and custody.
    let recover = operator("recover");
    let interrupted = Command::new("/bin/sh")
        .args(["-c", "ulimit -n 32 && exec \"$@\"", "limited-recovery"])
        .arg(recover.get_program())
        .args(recover.get_args())
        .arg("--acknowledge-unknown-result")
        .output()?;
    assert!(!interrupted.status.success());
    let report: Value = serde_json::from_slice(&interrupted.stdout)?;
    assert_eq!(report["state"], "refused");
    assert_eq!(report["old_result"], "unknown");
    let fd_limit = io::Error::from(rustix::io::Errno::MFILE).to_string();
    assert!(
        report["reason"].as_str().is_some_and(|s| s.contains(&fd_limit)),
        "{report}"
    );
    assert!(deep.is_dir());
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(&entry)?)?, marked);
    let recovered = inspect(true);
    assert_eq!(recovered["state"], "settled", "{recovered}");
    assert_eq!(recovered["old_result"], "unknown");
    assert!(!scratch.exists());
    assert!(!entry.exists());
    assert_eq!(inspect(true)["state"], "refused");
    Ok(())
}
