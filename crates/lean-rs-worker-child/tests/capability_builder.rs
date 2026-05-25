#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lean_rs::LeanBuiltCapability;
use lean_rs_worker_parent::{
    LeanWorkerBootstrapDiagnosticCode, LeanWorkerCapabilityBuilder, LeanWorkerCapabilityFact,
    LeanWorkerCapabilityMetadata, LeanWorkerChild, LeanWorkerCommandMetadata, LeanWorkerError, LeanWorkerRestartPolicy,
};
use serde_json::json;

fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn interop_root() -> PathBuf {
    workspace_root().join("fixtures").join("interop-shims")
}

fn shipped_template_root() -> PathBuf {
    workspace_root().join("templates").join("shipped-lean-crate")
}

fn shipped_template_manifest() -> PathBuf {
    shipped_template_root().join("Cargo.toml")
}

fn build_shipped_template() {
    // Memoize: see the matching note in `loader_regressions::build_template`.
    // Each invocation of `cargo build` spawns its own rustc swarm; with
    // nextest's parallel test threads and many tests calling this
    // helper, the unbounded fan-out can exhaust developer-machine RAM.
    // Cap the inner cargo's jobs and run the build at most once per
    // test process.
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .args(["build", "--jobs", "2", "--manifest-path"])
            .arg(shipped_template_manifest())
            .args(["--bins", "--examples"])
            .output()
            .expect("template cargo build starts");
        assert!(
            output.status.success(),
            "template build failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    });
}

fn shipped_manifest_path() -> PathBuf {
    build_shipped_template();
    let build_dir = shipped_template_root().join("target").join("debug").join("build");
    let mut candidates = std::fs::read_dir(&build_dir)
        .expect("template build directory exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("out").join("ShipLeanDemo.lean-rs-capability.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.pop().expect("template build emitted a capability manifest")
}

fn env_exe_name(name: &str) -> String {
    let mut name = name.to_owned();
    if !std::env::consts::EXE_SUFFIX.is_empty() {
        name.push_str(std::env::consts::EXE_SUFFIX);
    }
    name
}

fn shipped_worker_binary() -> PathBuf {
    build_shipped_template();
    shipped_template_root()
        .join("target")
        .join("debug")
        .join(env_exe_name("shipped-lean-crate-worker"))
}

fn builder() -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .worker_executable(worker_binary())
}

#[test]
fn built_capability_builder_infers_lake_root_from_dylib_path() {
    let dylib = interop_root()
        .join(".lake")
        .join("build")
        .join("lib")
        .join(if cfg!(target_os = "macos") {
            "liblean__rs__interop__consumer_LeanRsInteropConsumer.dylib"
        } else {
            "liblean__rs__interop__consumer_LeanRsInteropConsumer.so"
        });
    let spec = LeanBuiltCapability::path(&dylib)
        .package("lean_rs_interop_consumer")
        .module("LeanRsInteropConsumer");

    let builder = LeanWorkerCapabilityBuilder::from_built_capability(&spec, ["LeanRsInteropConsumer.Callback"])
        .expect("standard Lake dylib path is accepted")
        .worker_executable(worker_binary());

    assert_eq!(
        builder.session_key(),
        LeanWorkerCapabilityBuilder::new(
            interop_root(),
            "lean_rs_interop_consumer",
            "LeanRsInteropConsumer",
            ["LeanRsInteropConsumer.Callback"],
        )
        .worker_executable(worker_binary())
        .session_key(),
    );
}

#[test]
fn app_owned_worker_child_locator_accepts_explicit_binary() {
    let child = LeanWorkerChild::path(worker_binary());
    let mut capability = LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .worker_child(child)
    .open()
    .expect("explicit app-owned worker child opens");

    capability.open_session(None, None).expect("session opens");
}

#[test]
fn builder_opens_worker_capability_and_validates_metadata() {
    let mut capability = builder()
        .validate_metadata(
            "lean_rs_interop_consumer_worker_metadata",
            json!({"caller": "builder-test"}),
        )
        .open()
        .expect("builder opens capability");

    assert!(
        capability.dylib_path().is_file(),
        "builder should return the built Lake dylib path"
    );
    assert_eq!(
        capability.validated_metadata().map(|metadata| &metadata.commands),
        Some(&vec![
            LeanWorkerCommandMetadata {
                name: "version".to_owned(),
                version: "fixture-1".to_owned(),
            },
            LeanWorkerCommandMetadata {
                name: "scan".to_owned(),
                version: "fixture-2".to_owned(),
            },
        ]),
    );
    assert_eq!(capability.runtime_metadata().worker_version, env!("CARGO_PKG_VERSION"));

    let mut session = capability
        .open_session(None, None)
        .expect("session opens after builder");
    let metadata = session
        .capability_metadata(
            "lean_rs_interop_consumer_worker_metadata",
            &json!({"caller": "builder-test-second-session"}),
            None,
            None,
        )
        .expect("metadata call succeeds through reopened session");
    assert_eq!(metadata.commands.len(), 2);
}

#[test]
fn missing_lake_target_is_a_build_error() {
    let err = LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "MissingTarget",
        ["LeanRsInteropConsumer.Callback"],
    )
    .worker_executable(worker_binary())
    .open()
    .expect_err("missing Lake target should fail before worker startup");

    match err {
        LeanWorkerError::CapabilityBuild { diagnostic } => {
            let rendered = diagnostic.to_string();
            assert!(
                rendered.contains("MissingTarget"),
                "diagnostic should name the missing target: {rendered}"
            );
        }
        other => panic!("expected capability build error, got {other:?}"),
    }
}

#[test]
fn missing_worker_child_is_a_bootstrap_error() {
    let missing = workspace_root().join("target").join("definitely-not-a-worker-child");
    let err = builder()
        .worker_executable(missing)
        .open()
        .expect_err("missing worker child should fail before spawn");

    match err {
        LeanWorkerError::Bootstrap { code, message } => {
            assert_eq!(code, LeanWorkerBootstrapDiagnosticCode::WorkerChildNotExecutable);
            assert!(message.contains("path does not point to a file"));
        }
        other => panic!("expected bootstrap child error, got {other:?}"),
    }
}

#[test]
fn bootstrap_report_distinguishes_missing_child_without_spawning() {
    let report = builder()
        .worker_executable(workspace_root().join("target").join("missing-bootstrap-child"))
        .check();
    let first = report.first_error().expect("missing child is reported");
    assert_eq!(
        first.code(),
        LeanWorkerBootstrapDiagnosticCode::WorkerChildNotExecutable
    );
    assert!(first.message().contains("path does not point to a file"));
}

#[test]
fn manifest_backed_builder_uses_manifest_capability_identity() {
    // The manifest, primary dylib, and worker binary are all produced by
    // the template's `cargo build`; without this the test relied on a
    // sibling having built the template earlier under nextest's scheduler.
    build_shipped_template();

    let spec = LeanBuiltCapability::manifest_path(shipped_manifest_path());
    let report = LeanWorkerCapabilityBuilder::from_built_capability(&spec, ["ShipLeanDemo"])
        .expect("manifest-backed descriptor creates worker builder")
        .worker_child(LeanWorkerChild::path(shipped_worker_binary()))
        .check();

    assert!(
        report.is_ok(),
        "manifest-backed shipped worker bootstrap should pass: {report:?}",
    );
}

#[test]
fn missing_manifest_is_typed_at_manifest_backed_builder_boundary() {
    let missing = std::env::temp_dir()
        .join(format!("lean-rs-worker-missing-manifest-{}", std::process::id()))
        .join("missing.json");
    let err = LeanWorkerCapabilityBuilder::from_built_capability(
        &LeanBuiltCapability::manifest_path(missing),
        ["ShipLeanDemo"],
    )
    .expect_err("missing manifest should be typed");

    match err {
        LeanWorkerError::Bootstrap { code, message } => {
            assert_eq!(code.as_str(), "lean_rs.loader.missing_manifest");
            assert!(message.contains("could not read Lean capability manifest"));
        }
        other => panic!("expected missing-manifest bootstrap error, got {other:?}"),
    }
}

#[test]
fn stale_fingerprint_is_reported_by_bootstrap_preflight() {
    let manifest = shipped_manifest_path();
    let dir = std::env::temp_dir().join(format!("lean-rs-worker-stale-fingerprint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let stale = dir.join("ShipLeanDemo.lean-rs-capability.json");
    let mut contents: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).expect("read template manifest"))
            .expect("template manifest is JSON");
    contents
        .get_mut("toolchain_fingerprint")
        .and_then(serde_json::Value::as_object_mut)
        .expect("manifest has fingerprint object")
        .insert(
            "header_sha256".to_owned(),
            serde_json::Value::String("0000000000000000000000000000000000000000000000000000000000000000".to_owned()),
        );
    std::fs::write(
        &stale,
        serde_json::to_vec_pretty(&contents).expect("encode stale manifest"),
    )
    .expect("write stale manifest");

    let spec = LeanBuiltCapability::manifest_path(stale);
    let report = LeanWorkerCapabilityBuilder::from_built_capability(&spec, ["ShipLeanDemo"])
        .expect("manifest still contains capability identity")
        .worker_child(LeanWorkerChild::path(shipped_worker_binary()))
        .check();

    let first = report.first_error().expect("stale fingerprint should be reported");
    assert_eq!(
        first.code().as_str(),
        "lean_rs.loader.unsupported_toolchain_fingerprint"
    );
}

#[test]
fn metadata_mismatch_is_reported_by_bootstrap_check() {
    let wrong_metadata = LeanWorkerCapabilityMetadata {
        commands: vec![LeanWorkerCommandMetadata {
            name: "wrong".to_owned(),
            version: "0".to_owned(),
        }],
        capabilities: vec![LeanWorkerCapabilityFact {
            name: "wrong-capability".to_owned(),
            version: "0".to_owned(),
        }],
        lean_version: None,
        extra: None,
    };
    let report = builder()
        .expect_metadata(
            "lean_rs_interop_consumer_worker_metadata",
            json!({"caller": "builder-metadata-check"}),
            wrong_metadata,
        )
        .check();

    let first = report.first_error().expect("metadata mismatch is reported");
    assert_eq!(
        first.code(),
        LeanWorkerBootstrapDiagnosticCode::CapabilityMetadataMismatch
    );
}

#[test]
fn handshake_failure_is_a_bootstrap_diagnostic() {
    let shell = PathBuf::from("/bin/sh");
    if !shell.is_file() {
        return;
    }
    let report = builder()
        .worker_child(LeanWorkerChild::path(shell))
        .startup_timeout(Duration::from_millis(50))
        .check();
    let first = report.first_error().expect("non-worker child fails bootstrap");
    assert_eq!(first.code(), LeanWorkerBootstrapDiagnosticCode::WorkerHandshakeFailed);
}

#[test]
fn metadata_validation_failure_is_typed() {
    let err = builder()
        .validate_metadata(
            "lean_rs_interop_consumer_worker_metadata_missing",
            json!({"caller": "builder-test"}),
        )
        .open()
        .expect_err("metadata validation should fail when export is missing");

    match err {
        LeanWorkerError::Worker { code, message } => {
            assert_eq!(code, "lean_rs.symbol_lookup");
            assert!(
                message.contains("lean_rs_interop_consumer_worker_metadata_missing"),
                "message should name missing export, got {message}",
            );
        }
        other => panic!("expected worker symbol lookup error, got {other:?}"),
    }
}

#[test]
fn restart_policy_override_is_applied_during_builder_startup() {
    let capability = builder()
        .restart_policy(LeanWorkerRestartPolicy::default().max_requests(1))
        .open()
        .expect("builder opens capability with restart policy");

    let stats = capability.worker().stats();
    assert!(
        stats.max_request_restarts >= 1,
        "health then session-open should trigger the max-request restart policy, stats={stats:?}",
    );
}

#[test]
fn open_session_with_imports_overrides_builder_imports() {
    let mut capability = builder().open().expect("builder opens capability");

    capability
        .open_session(None, None)
        .expect("baseline open_session still works after adding the override sibling");

    capability
        .open_session_with_imports(["LeanRsInteropConsumer"], None, None)
        .expect("override with a different real import set opens a session");

    match capability.open_session_with_imports(["LeanRsInteropConsumer.DefinitelyDoesNotExist"], None, None) {
        Ok(_) => panic!("override with a bogus import should be rejected by the child"),
        Err(LeanWorkerError::Worker { .. } | LeanWorkerError::CapabilityBuild { .. }) => {}
        Err(other) => panic!("expected the child to reject the bad import, got {other:?}"),
    }
}
