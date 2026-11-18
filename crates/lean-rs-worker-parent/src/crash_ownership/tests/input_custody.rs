//! Generation input retention across cancellation or server death.

use super::{LeanWorkerNativeScope, LeanWorkerOwnedProcess, directory, wait_for};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

#[test]
fn preparing_scope_owns_one_private_input_directory_and_cleans_it_on_cancel() {
    use std::os::unix::fs::MetadataExt as _;
    let root = directory().canonicalize().unwrap();
    let scope = LeanWorkerNativeScope::prepare(&root.join("owners"), Duration::from_secs(1), None).unwrap();
    let inputs = scope.input_directory().unwrap();
    assert_eq!(inputs, scope.input_directory().unwrap());
    assert_eq!(fs::metadata(&inputs).unwrap().mode() & 0o777, 0o700);
    fs::create_dir(inputs.join("lake-cache")).unwrap();
    fs::write(inputs.join("lake-cache").join("source.lean"), b"owned source").unwrap();
    let generation = inputs.parent().unwrap().to_path_buf();
    assert!(generation.join("owner.json").exists());
    drop(scope);
    assert!(!generation.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelled_preparation_budget_failure_can_recover_while_parent_stays_alive() {
    let root = directory().canonicalize().unwrap();
    let registry = root.join("owners");
    let slot = lean_toolchain::native_guardian::NativeCapacitySlot {
        key: "utility".to_owned(),
        scope: "preparation".to_owned(),
        limit: 1,
    };
    let scope = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(1), Some(&slot)).unwrap();
    let inputs = scope.input_directory().unwrap();
    let record = inputs.parent().unwrap().join("owner.json");
    for index in 0..4097 {
        fs::write(inputs.join(format!("{index:04}")), []).unwrap();
    }
    drop(scope);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(
        value.get("phase").and_then(serde_json::Value::as_str),
        Some("preparing_closed")
    );
    assert!(value.get("process").is_some_and(serde_json::Value::is_null));
    assert!(
        LeanWorkerOwnedProcess::capture(std::process::id())
            .unwrap()
            .unwrap()
            .is_live()
            .unwrap()
    );
    let next = lean_toolchain::native_guardian::NativeCapacitySlot {
        key: "next".to_owned(),
        ..slot
    };
    let replacement = LeanWorkerNativeScope::prepare(&registry, Duration::from_secs(1), Some(&next)).unwrap();
    assert!(!record.exists());
    assert!(!inputs.exists());
    drop(replacement);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn actual_server_death_keeps_inputs_for_live_descendant_then_next_gc_removes_them() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = directory().canonicalize().unwrap();
    let registry = root.join("owners");
    fs::create_dir(&registry).unwrap();
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("input-custody-mode"), []).unwrap();
    let mut server = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_ownership::lifecycle_tests::fixture_broker_entry",
            "--nocapture",
        ])
        .env("PROOFS_LIFELINE_BROKER", &root)
        .env_remove("PROOFS_LIFELINE_CHILD")
        .spawn()
        .unwrap();
    wait_for(|| {
        root.join("broker-ready").exists()
            && fs::metadata(root.join("input-reading")).is_ok_and(|metadata| metadata.len() > 0)
    });
    let group: u32 = fs::read_to_string(root.join("broker-ready")).unwrap().parse().unwrap();
    let inputs = PathBuf::from(fs::read_to_string(root.join("input-path")).unwrap());
    let record = inputs.parent().unwrap().join("owner.json");
    assert!(lean_toolchain::process_group::group_present(group).unwrap());
    assert!(inputs.join("source.lean").exists());
    server.kill().unwrap();
    server.wait().unwrap(); // signal only our actual owned C.
    lean_toolchain::native_guardian::reserve_owner_record(&registry, &format!("{:048x}", 1), Duration::from_secs(1))
        .unwrap();
    if lean_toolchain::process_group::group_present(group).unwrap() {
        assert!(record.exists());
        assert!(inputs.join("source.lean").exists());
        let before = fs::metadata(root.join("input-reading")).unwrap().len();
        wait_for(|| fs::metadata(root.join("input-reading")).unwrap().len() > before);
    }
    wait_for(|| !lean_toolchain::process_group::group_present(group).unwrap());
    lean_toolchain::native_guardian::reserve_owner_record(&registry, &format!("{:048x}", 2), Duration::from_secs(1))
        .unwrap();
    assert!(!record.exists());
    assert!(!inputs.exists());
    fs::remove_dir_all(root).unwrap();
}
