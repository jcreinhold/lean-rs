#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm
)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use lean_rs_worker_parent::{
    LeanWorkerCapabilityBuilder, LeanWorkerImportPlanConfig, LeanWorkerImportPlanError, LeanWorkerImportPlanner,
    LeanWorkerJsonCommand, LeanWorkerModuleWork, LeanWorkerPool, LeanWorkerPoolConfig,
};
use lean_toolchain::{LeanModuleDiscoveryOptions, discover_lake_modules};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Serialize)]
struct FixtureRequest {
    source: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct FixtureResponse {
    accepted: bool,
    kind: String,
}

fn json_command() -> LeanWorkerJsonCommand<FixtureRequest, FixtureResponse> {
    LeanWorkerJsonCommand::new("lean_rs_interop_consumer_worker_json_command")
}

fn planner() -> LeanWorkerImportPlanner {
    LeanWorkerImportPlanner::new(
        LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer")
            .source_roots(["LeanRsInteropConsumer"])
            .base_imports(["LeanRsInteropConsumer.Callback"]),
    )
}

#[test]
fn planner_output_is_deterministic_and_grouped_by_session_key() {
    let first = planner().plan_lake_project().expect("planner succeeds");
    let second = planner().plan_lake_project().expect("planner succeeds again");

    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    let batch = &first[0];
    assert_eq!(batch.source_root, "LeanRsInteropConsumer");
    assert_eq!(batch.imports, vec!["LeanRsInteropConsumer.Callback"]);
    assert_eq!(batch.modules.len(), 2);
    assert_eq!(batch.modules[0].module, "LeanRsInteropConsumer");
    assert_eq!(batch.modules[1].module, "LeanRsInteropConsumer.Callback");
    assert_eq!(batch.session_key.imports(), batch.imports.as_slice());
    assert_eq!(batch.session_key.project_root(), interop_root().as_path());
}

#[test]
fn manual_work_items_group_by_import_set() {
    let discovered =
        discover_lake_modules(LeanModuleDiscoveryOptions::new(interop_root())).expect("module discovery succeeds");
    let config = LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer");
    let planner = LeanWorkerImportPlanner::new(config);
    let source_set = discovered.fingerprint;
    let modules = vec![
        LeanWorkerModuleWork::new(
            "LeanRsInteropConsumer",
            interop_root().join("LeanRsInteropConsumer.lean"),
            "LeanRsInteropConsumer",
            ["LeanRsInteropConsumer"],
        ),
        LeanWorkerModuleWork::new(
            "LeanRsInteropConsumer.Callback",
            interop_root().join("LeanRsInteropConsumer").join("Callback.lean"),
            "LeanRsInteropConsumer",
            ["LeanRsInteropConsumer.Callback"],
        ),
    ];

    let batches = planner
        .plan_work_items(modules, &source_set)
        .expect("manual modules plan");
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].modules[0].module, "LeanRsInteropConsumer");
    assert_eq!(batches[1].modules[0].module, "LeanRsInteropConsumer.Callback");
    assert_ne!(batches[0].session_key, batches[1].session_key);
}

#[test]
fn invalid_module_name_is_typed() {
    let discovered =
        discover_lake_modules(LeanModuleDiscoveryOptions::new(interop_root())).expect("module discovery succeeds");
    let config = LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer");
    let err = LeanWorkerImportPlanner::new(config)
        .plan_work_items(
            [LeanWorkerModuleWork::new(
                "Bad-Module",
                interop_root().join("Bad-Module.lean"),
                "LeanRsInteropConsumer",
                ["LeanRsInteropConsumer.Callback"],
            )],
            &discovered.fingerprint,
        )
        .expect_err("invalid module name should be typed");

    match err {
        LeanWorkerImportPlanError::InvalidModuleName { module, .. } => assert_eq!(module, "Bad-Module"),
        other => panic!("expected invalid module name, got {other:?}"),
    }
}

#[test]
fn missing_selected_root_and_capability_target_are_typed() {
    let missing_root = LeanWorkerImportPlanner::new(
        LeanWorkerImportPlanConfig::new(interop_root(), "lean_rs_interop_consumer", "LeanRsInteropConsumer")
            .source_roots(["MissingRoot"]),
    )
    .plan_lake_project()
    .expect_err("missing source root should be typed");
    match missing_root {
        LeanWorkerImportPlanError::ModuleDiscovery { diagnostic } => {
            assert!(diagnostic.to_string().contains("MissingRoot"));
        }
        other => panic!("expected module discovery error, got {other:?}"),
    }

    let missing_target = LeanWorkerImportPlanner::new(LeanWorkerImportPlanConfig::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "MissingTarget",
    ))
    .plan_lake_project()
    .expect_err("missing capability target should be typed");
    match missing_target {
        LeanWorkerImportPlanError::UnresolvedCapabilityTarget { target_name, .. } => {
            assert_eq!(target_name, "MissingTarget");
        }
        other => panic!("expected unresolved capability target, got {other:?}"),
    }
}

#[test]
fn planned_batches_execute_through_pool_with_one_import_group() {
    let batches = planner().plan_lake_project().expect("planner succeeds");
    assert_eq!(batches.len(), 1);
    let modules = batches[0].modules.len();
    assert!(modules > 1, "fixture should provide more than one module");

    let naive_start = Instant::now();
    for module in &batches[0].modules {
        let mut capability = LeanWorkerCapabilityBuilder::new(
            interop_root(),
            "lean_rs_interop_consumer",
            "LeanRsInteropConsumer",
            ["LeanRsInteropConsumer.Callback"],
        )
        .json_command_export("lean_rs_interop_consumer_worker_json_command")
        .worker_executable(worker_binary())
        .open()
        .expect("naive capability opens");
        let mut session = capability.open_session(None, None).expect("naive session opens");
        let response = session
            .run_json_command(
                &json_command(),
                &FixtureRequest {
                    source: module.module.clone(),
                },
                None,
                None,
            )
            .expect("naive command succeeds");
        assert!(response.accepted);
    }
    let naive = naive_start.elapsed();

    let planned_start = Instant::now();
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let mut lease = pool
        .acquire_lease(
            batches[0]
                .capability_builder()
                .metadata_export("lean_rs_interop_consumer_worker_metadata")
                .json_command_export("lean_rs_interop_consumer_worker_json_command")
                .worker_executable(worker_binary()),
        )
        .expect("planned batch lease opens");
    for module in &batches[0].modules {
        let response = lease
            .run_json_command(
                &json_command(),
                &FixtureRequest {
                    source: module.module.clone(),
                },
                None,
                None,
            )
            .expect("planned command succeeds");
        assert!(response.accepted);
    }
    let planned = planned_start.elapsed();
    drop(lease);

    println!(
        "import_planner_fixture modules={modules} batches={} naive_ms={} planned_ms={} planned_workers={}",
        batches.len(),
        naive.as_millis(),
        planned.as_millis(),
        pool.snapshot().workers,
    );
    assert_eq!(pool.snapshot().workers, 1);
}
