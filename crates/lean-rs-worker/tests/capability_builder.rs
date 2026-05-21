#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};

use lean_rs::LeanBuiltCapability;
use lean_rs_worker::{
    LeanWorkerCapabilityBuilder, LeanWorkerChild, LeanWorkerCommandMetadata, LeanWorkerError, LeanWorkerRestartPolicy,
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
fn missing_worker_child_is_a_spawn_error() {
    let missing = workspace_root().join("target").join("definitely-not-a-worker-child");
    let err = builder()
        .worker_executable(missing.clone())
        .open()
        .expect_err("missing worker child should fail at spawn");

    match err {
        LeanWorkerError::Spawn { executable, .. } => assert_eq!(executable, missing),
        other => panic!("expected spawn error, got {other:?}"),
    }
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
