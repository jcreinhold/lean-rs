//! Panic-containment contract for prompt 33.
//!
//! A Lean internal panic is process-scoped, not a typed `LeanError`.
//! This test proves the contract without killing the main test process:
//! the parent re-runs this same test binary as a child with
//! `LEAN_ABORT_ON_PANIC=1`, then asserts that the child terminates.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

use lean_rs::LeanRuntime;
use lean_rs_host::LeanHost;

const CHILD_ENV: &str = "LEAN_RS_PANIC_CHILD";
const TEST_NAME: &str = "lean_internal_panic_terminates_child_process";

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn run_child_workload() {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens cleanly");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = caps
        .session(&["LeanRsFixture.Effects"])
        .expect("session imports cleanly");

    let returned = session.call_capability::<(u8,), ()>("lean_rs_fixture_panic_unit", (0,));
    panic!("Lean panic export returned instead of terminating: {returned:?}");
}

#[test]
fn lean_internal_panic_terminates_child_process() {
    if std::env::var_os(CHILD_ENV).is_some() {
        run_child_workload();
        return;
    }

    let current_exe = std::env::current_exe().expect("test binary path is available");
    let output = Command::new(current_exe)
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("LEAN_ABORT_ON_PANIC", "1")
        .env("RUST_BACKTRACE", "0")
        .output()
        .expect("child test process starts");

    assert!(
        !output.status.success(),
        "Lean panic child unexpectedly succeeded; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        assert!(
            stderr.contains("lean_rs_fixture: deliberate Lean panic"),
            "child stderr should contain the Lean panic message when stderr is available, got:\n{stderr}",
        );
    }
}
