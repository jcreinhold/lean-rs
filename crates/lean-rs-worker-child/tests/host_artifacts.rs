//! The worker's `--lean-rs-prepare-host-artifacts DEST` mode, which an installer
//! uses to give a worker its own built copy of the bundled shims.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts;

#[test]
#[ignore = "builds both shim packages from source with Lake; run it by name"]
fn prepare_writes_and_builds_the_shims_in_a_new_directory() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .ok_or("no workspace root")?;
    let scratch = tempfile::Builder::new()
        .prefix("prep.")
        .tempdir_in(workspace.join("target"))?;
    let root = scratch.path().canonicalize()?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let destination = root.join("shims");
    let prepare = || {
        Command::new(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
            .arg("--lean-rs-prepare-host-artifacts")
            .arg(&destination)
            .output()
    };

    let output = prepare()?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let descriptor: LeanWorkerPreparedHostArtifacts = serde_json::from_slice(&output.stdout)?;
    let host = destination.join("lean-rs-host-shims");
    let interop = destination.join("lean-rs-interop-shims");
    assert!(
        host.join("LeanRsHostShims/InfoTree.lean").is_file(),
        "sources were written"
    );
    assert!(
        Path::new(&descriptor.shim_manifest_path).starts_with(&host),
        "{}",
        descriptor.shim_manifest_path
    );
    let [first, second] = descriptor.shim_search_paths.as_slice() else {
        return Err(format!(
            "expected interop and host search paths: {:?}",
            descriptor.shim_search_paths
        )
        .into());
    };
    assert!(Path::new(first).starts_with(&interop), "{first}");
    assert!(Path::new(second).starts_with(&host), "{second}");

    let again = prepare()?;
    assert!(!again.status.success(), "an existing destination must be refused");
    Ok(())
}
