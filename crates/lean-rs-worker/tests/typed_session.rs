//! Per-method coverage for the typed `LeanWorkerSession` surface added in
//! 0.1.6: `infer_type`, `whnf`, `is_def_eq`, `describe`,
//! `list_declarations_strings`, `describe_bulk`, `process_file`, and
//! `process_module`. Each test exercises the full worker → child → host
//! dispatch path and asserts response shape against the fixture.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm
)]

use std::path::{Path, PathBuf};

use lean_rs_worker::{
    LeanWorker, LeanWorkerConfig, LeanWorkerDeclarationFilter, LeanWorkerElabOptions, LeanWorkerMetaResult,
    LeanWorkerMetaTransparency, LeanWorkerProcessFileOutcome, LeanWorkerProcessModuleOutcome, LeanWorkerRendering,
    LeanWorkerSessionConfig,
};

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

fn fixture_root() -> PathBuf {
    workspace_root().join("fixtures").join("lean")
}

fn ensure_fixture_built() {
    lean_toolchain::build_lake_target_quiet(&fixture_root(), "LeanRsFixture").expect("fixture Lake target builds");
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
}

fn handles_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsFixture.Handles"],
    )
}

fn elaboration_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsHostShims.Elaboration"],
    )
}

fn source_ranges_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsFixture.SourceRanges"],
    )
}

#[test]
fn infer_type_returns_rendered_type_for_known_term() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .infer_type("(1 + 1 : Nat)", &opts, None, None)
        .expect("worker infer_type dispatch succeeds");

    match result {
        LeanWorkerMetaResult::Ok { value: rendered } => {
            assert!(
                rendered.value.contains("Nat"),
                "rendered type should mention Nat, got {rendered:?}"
            );
            assert_eq!(
                rendered.rendering,
                LeanWorkerRendering::Pretty,
                "fixture loads the meta_pp_expr shim, so notation-aware rendering should fire"
            );
        }
        other => panic!("expected Ok meta result, got {other:?}"),
    }
}

#[test]
fn whnf_reduces_known_term() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .whnf("(1 + 1 : Nat)", &opts, None, None)
        .expect("worker whnf dispatch succeeds");

    match result {
        LeanWorkerMetaResult::Ok { value: _rendered } => {}
        other => panic!("expected Ok meta result, got {other:?}"),
    }
}

#[test]
fn is_def_eq_recognises_equal_terms() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .is_def_eq(
            "(2 : Nat)",
            "(1 + 1 : Nat)",
            LeanWorkerMetaTransparency::Default,
            &opts,
            None,
            None,
        )
        .expect("worker is_def_eq dispatch succeeds");

    match result {
        LeanWorkerMetaResult::Ok { value } => assert!(value, "2 ≡ 1 + 1 should hold definitionally"),
        other => panic!("expected Ok meta result, got {other:?}"),
    }
}

#[test]
fn describe_returns_row_for_known_declaration() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let row = session
        .describe("Nat.add", None, None)
        .expect("worker describe dispatch succeeds")
        .expect("Nat.add is present in the open environment");

    assert_eq!(row.name, "Nat.add");
    assert!(
        !row.kind.is_empty() && row.kind != "missing",
        "Nat.add should have a non-missing kind, got {:?}",
        row.kind
    );
    assert!(
        row.type_signature.as_deref().is_some_and(|s| !s.is_empty()),
        "Nat.add should have a rendered type signature, got {:?}",
        row.type_signature
    );
}

#[test]
fn describe_returns_none_for_unknown_declaration() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let row = session
        .describe("This.Name.Does.Not.Exist", None, None)
        .expect("worker describe dispatch succeeds");

    assert!(row.is_none(), "absent name should project to None");
}

#[test]
fn list_declarations_strings_streams_full_env_without_frame_cap() {
    ensure_fixture_built();
    let filter = LeanWorkerDeclarationFilter::default();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let names = session
        .list_declarations_strings(&filter, None, None)
        .expect("worker list_declarations_strings dispatch succeeds");

    assert!(
        names.len() > 1000,
        "the fixture env imports Lean stdlib; expected many names, got {}",
        names.len()
    );
    assert!(
        names.iter().any(|n| n == "Nat.add"),
        "expected Nat.add in enumerated names"
    );
}

#[test]
fn describe_bulk_preserves_input_length_with_missing_slots() {
    ensure_fixture_built();
    let names = ["Nat.add", "This.Name.Does.Not.Exist", "Nat.succ"];
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let rows = session
        .describe_bulk(&names, None, None)
        .expect("worker describe_bulk dispatch succeeds");

    assert_eq!(rows.len(), names.len());
    assert_eq!(rows[0].name, "Nat.add");
    assert!(rows[0].kind != "missing");
    assert!(rows[0].type_signature.is_some());
    assert_eq!(rows[1].kind, "missing");
    assert!(rows[1].type_signature.is_none());
    assert!(rows[1].source.is_none());
    assert_eq!(rows[2].name, "Nat.succ");
    assert!(rows[2].kind != "missing");
}

#[test]
fn process_file_projects_inline_source_into_info_tree() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let outcome = session
        .process_file("example : Nat := 1\n", &opts, None, None)
        .expect("worker process_file dispatch succeeds");

    match outcome {
        LeanWorkerProcessFileOutcome::Processed { file } => {
            assert!(!file.commands.is_empty(), "single example produces one command");
            assert!(!file.terms.is_empty(), "elaborator records at least one term node");
            assert!(
                file.diagnostics.diagnostics.iter().all(|d| d.severity != "error"),
                "valid example should not produce error diagnostics"
            );
        }
        LeanWorkerProcessFileOutcome::Unsupported => panic!("expected Processed outcome, got Unsupported"),
    }
}

#[test]
fn process_module_projects_fixture_module_into_info_tree() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let source = std::fs::read_to_string(fixture_root().join("LeanRsFixture").join("SourceRanges.lean"))
        .expect("fixture SourceRanges.lean reads");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&source_ranges_session_config(), None, None)
        .expect("worker session opens");

    let outcome = session
        .process_module(&source, &opts, None, None)
        .expect("worker process_module dispatch succeeds");

    match outcome {
        LeanWorkerProcessModuleOutcome::Ok { file, imports } => {
            assert!(!file.commands.is_empty(), "fixture body produces commands");
            assert!(imports.iter().any(|m| m == "Lean"), "fixture imports Lean");
        }
        LeanWorkerProcessModuleOutcome::MissingImports { missing, .. } => {
            panic!("expected Ok module outcome, got MissingImports({missing:?})")
        }
        LeanWorkerProcessModuleOutcome::HeaderParseFailed { diagnostics } => {
            panic!("expected Ok module outcome, got HeaderParseFailed({diagnostics:?})")
        }
        LeanWorkerProcessModuleOutcome::Unsupported => {
            panic!("expected Ok module outcome, got Unsupported")
        }
    }
}
