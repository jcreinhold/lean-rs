//! Downstream-shape proof for bundled host shims.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use lean_rs::LeanRuntime;
use lean_rs_host::LeanHost;

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

#[test]
fn host_loads_with_no_consumer_shim_require() {
    let lake_root = fixture_lake_root();
    let manifest = std::fs::read_to_string(lake_root.join("lake-manifest.json")).expect("fixture manifest is readable");
    assert!(
        !manifest.contains("lean_rs_host_shims") && !manifest.contains("lean_rs_interop_shims"),
        "fixture manifest should not require lean-rs shim packages: {manifest}",
    );

    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let host = LeanHost::from_lake_project(runtime, &lake_root).expect("host opens cleanly");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("capabilities load with bundled shims");
    let mut session = caps
        .session(&["LeanRsFixture.Handles", "LeanRsHostShims.Meta"], None, None)
        .expect("session imports consumer and bundled shim modules");

    let kind = session
        .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("declaration kind query succeeds");
    assert_eq!(kind, "definition");
}
