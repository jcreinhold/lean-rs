//! Per-method coverage for the typed `LeanWorkerSession` surface added in
//! 0.1.6: `infer_type`, `whnf`, `is_def_eq`, `describe`,
//! `list_declarations_strings`, `describe_bulk`, and module queries.
//! Each test exercises the full worker → child → host
//! dispatch path and asserts response shape against the fixture.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerConfig, LeanWorkerDeclarationFilter, LeanWorkerDeclarationInspectionFields,
    LeanWorkerDeclarationInspectionRequest, LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationNameMatch,
    LeanWorkerDeclarationSearch, LeanWorkerDeclarationSearchBias, LeanWorkerDeclarationSearchScope,
    LeanWorkerDeclarationVerificationRequest, LeanWorkerDeclarationVerificationResult,
    LeanWorkerDeclarationVerificationStatus, LeanWorkerDeclarationVerificationTarget, LeanWorkerElabOptions,
    LeanWorkerError, LeanWorkerMetaResult, LeanWorkerMetaTransparency, LeanWorkerModuleCacheStatus,
    LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome,
    LeanWorkerModuleQueryBatchResult, LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQueryOutcome,
    LeanWorkerModuleQueryResult, LeanWorkerModuleQuerySelector, LeanWorkerOutputBudgets, LeanWorkerProofAttemptRequest,
    LeanWorkerProofAttemptResult, LeanWorkerProofAttemptStatus, LeanWorkerProofCandidate, LeanWorkerProofEditTarget,
    LeanWorkerProofPositionSelector, LeanWorkerProofStateResult, LeanWorkerRendering, LeanWorkerSessionConfig,
    LeanWorkerSorryPolicy,
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

fn cache_worker_config() -> LeanWorkerConfig {
    worker_config().env("LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB", "0")
}

fn handles_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsFixture.Handles", "LeanRsFixture.SourceRanges"],
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

fn cache_probe_selectors() -> Vec<LeanWorkerModuleQuerySelector> {
    vec![LeanWorkerModuleQuerySelector::ProofState {
        id: "state".to_owned(),
        line: 2,
        column: 4,
    }]
}

fn batch_facts(outcome: &LeanWorkerModuleQueryBatchOutcome) -> &LeanWorkerModuleQueryCacheFacts {
    match outcome {
        LeanWorkerModuleQueryBatchOutcome::Ok { facts, .. }
        | LeanWorkerModuleQueryBatchOutcome::MissingImports { facts, .. }
        | LeanWorkerModuleQueryBatchOutcome::HeaderParseFailed { facts, .. } => facts,
        LeanWorkerModuleQueryBatchOutcome::Unsupported => {
            panic!("batch cache facts unavailable on unsupported outcome")
        }
        _ => panic!("unexpected batch outcome variant"),
    }
}

fn assert_batch_has_state(outcome: &LeanWorkerModuleQueryBatchOutcome) {
    let LeanWorkerModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    assert!(
        result.items.iter().any(|item| {
            matches!(
                item,
                LeanWorkerModuleQueryBatchItem::Ok { id, result }
                    if id == "state"
                        && matches!(result.as_ref(), LeanWorkerModuleQueryBatchResult::ProofState(_))
            )
        }),
        "expected proof-state selector item, got {:?}",
        result.items,
    );
}

struct TempLakeProject {
    root: PathBuf,
}

impl TempLakeProject {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lean-rs-worker-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).expect("create temporary Lake project");
        fs::write(
            root.join("lean-toolchain"),
            fs::read_to_string(workspace_root().join("lean-toolchain")).expect("read workspace Lean toolchain"),
        )
        .expect("write temporary Lean toolchain pin");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, content).expect("write temporary Lake project file");
    }

    fn lake_build_ok(&self, target: &str) {
        let output = Command::new("lake")
            .arg("build")
            .arg(target)
            .current_dir(&self.root)
            .output()
            .expect("lake command starts");
        assert!(
            output.status.success(),
            "`lake build {target}` failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

impl Drop for TempLakeProject {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.root));
    }
}

fn write_module_syntax_fixture(project: &TempLakeProject) {
    project.write(
        "lakefile.lean",
        "import Lake\nopen Lake DSL\npackage module_syntax_fixture\nlean_lib Fixture\n",
    );
    project.write("Fixture/Imported.lean", "module\n\npublic def imported : Nat := 2\n");
    project.write("Fixture/Internal.lean", "module\n\ndef internalSecret : Nat := 40\n");
    project.write("Fixture/PrivateScope.lean", "module\n\ndef privateOnly : Nat := 7\n");
    project.lake_build_ok("Fixture.Imported");
    project.lake_build_ok("Fixture.Internal");
    project.lake_build_ok("Fixture.PrivateScope");
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
fn search_declarations_returns_bounded_metadata_without_types() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .search_declarations(
            &LeanWorkerDeclarationSearch {
                name_fragment: Some("add".to_owned()),
                name_match: LeanWorkerDeclarationNameMatch::Contains,
                kind: Some("theorem".to_owned()),
                required_constants: Vec::new(),
                conclusion_head: None,
                scope_biases: Vec::new(),
                limit: 5,
                filter: LeanWorkerDeclarationFilter {
                    include_private: false,
                    include_generated: false,
                    include_internal: false,
                },
                include_source: false,
            },
            None,
            None,
        )
        .expect("worker search_declarations dispatch succeeds");

    assert!(result.declarations.len() <= 5);
    assert!(
        result.declarations.iter().all(|row| row.kind == "theorem"),
        "kind filter should be applied before rows return: {:?}",
        result.declarations
    );
    assert!(
        result.declarations.iter().all(|row| row.source.is_none()),
        "metadata search should be able to omit source lookups"
    );
    assert!(
        result.declarations.iter().all(|row| row.rank >= 1),
        "search rows should carry deterministic ranks: {:?}",
        result.declarations
    );
    assert_eq!(result.facts.source_lookups, 0);
    assert!(result.facts.declarations_scanned >= result.facts.after_name_filter);
}

#[test]
fn search_declarations_supports_structural_filters_and_deterministic_order() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let search = LeanWorkerDeclarationSearch {
        name_fragment: Some("Anonymous".to_owned()),
        name_match: LeanWorkerDeclarationNameMatch::Suffix,
        kind: Some("definition".to_owned()),
        required_constants: vec!["Unit".to_owned()],
        conclusion_head: Some("Lean.Name".to_owned()),
        scope_biases: vec![LeanWorkerDeclarationSearchBias {
            scope: LeanWorkerDeclarationSearchScope::Namespace,
            prefix: "LeanRsFixture.Handles".to_owned(),
            strict: true,
            weight: 25,
        }],
        limit: 10,
        filter: LeanWorkerDeclarationFilter {
            include_private: false,
            include_generated: false,
            include_internal: false,
        },
        include_source: false,
    };

    let first = session
        .search_declarations(&search, None, None)
        .expect("first search_declarations dispatch succeeds");
    let second = session
        .search_declarations(&search, None, None)
        .expect("second search_declarations dispatch succeeds");

    assert_eq!(
        first.declarations, second.declarations,
        "search order must be deterministic"
    );
    assert!(
        first
            .declarations
            .iter()
            .any(|row| row.name == "LeanRsFixture.Handles.nameAnonymous"),
        "required-constant and conclusion-head filters should find the fixture declaration: {:?}",
        first.declarations
    );
    assert!(first.declarations.iter().all(|row| row.kind == "definition"));
    assert!(first.declarations.iter().all(|row| row.name.ends_with("Anonymous")));
    assert_eq!(first.facts.source_lookups, 0);
    assert!(first.facts.after_scope_filter <= first.facts.after_conclusion_filter);
}

#[test]
fn search_declarations_caps_broad_queries_and_reports_pruning() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .search_declarations(
            &LeanWorkerDeclarationSearch {
                name_fragment: None,
                name_match: LeanWorkerDeclarationNameMatch::Contains,
                kind: None,
                required_constants: Vec::new(),
                conclusion_head: None,
                scope_biases: Vec::new(),
                limit: 1,
                filter: LeanWorkerDeclarationFilter {
                    include_private: false,
                    include_generated: false,
                    include_internal: false,
                },
                include_source: false,
            },
            None,
            None,
        )
        .expect("broad search_declarations dispatch succeeds");

    assert_eq!(result.declarations.len(), 1);
    assert!(result.truncated);
    assert!(result.facts.truncated);
    assert!(
        result
            .facts
            .broad_pruning
            .iter()
            .any(|pruning| pruning.reason == "broad_search_limit"),
        "broad search should report limit pruning: {:?}",
        result.facts.broad_pruning
    );
}

#[test]
fn declaration_type_truncates_single_rendered_type() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let row = session
        .declaration_type("Nat.rec", 16, None, None)
        .expect("worker declaration_type dispatch succeeds")
        .expect("Nat.rec is present");

    let rendered = row.type_signature.expect("Nat.rec has a type");
    assert!(rendered.truncated, "cap should truncate Nat.rec's recursor type");
    assert!(rendered.value.len() <= 16);
}

#[test]
fn declaration_type_zero_cap_returns_empty_truncated_type() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let row = session
        .declaration_type("Nat.rec", 0, None, None)
        .expect("worker declaration_type dispatch succeeds")
        .expect("Nat.rec is present");

    let rendered = row.type_signature.expect("Nat.rec has a type");
    assert!(rendered.truncated, "zero cap should still report omitted type text");
    assert!(rendered.value.is_empty());
}

#[test]
fn inspect_declaration_returns_known_theorem() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .inspect_declaration(
            &LeanWorkerDeclarationInspectionRequest::new("LeanRsFixture.SourceRanges.knownTheorem"),
            None,
            None,
        )
        .expect("worker inspect_declaration dispatch succeeds");

    let LeanWorkerDeclarationInspectionResult::Found { declaration } = result else {
        panic!("known theorem should be found, got {result:?}");
    };
    assert_eq!(declaration.name, "LeanRsFixture.SourceRanges.knownTheorem");
    assert_eq!(declaration.kind, "theorem");
    assert!(declaration.source.is_some());
    let statement = declaration.statement.expect("default inspection renders statement");
    assert_eq!(statement.value, "True");
    assert!(!statement.truncated);
}

#[test]
fn inspect_declaration_returns_known_definition() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .inspect_declaration(
            &LeanWorkerDeclarationInspectionRequest::new("LeanRsFixture.Handles.nameAnonymous"),
            None,
            None,
        )
        .expect("worker inspect_declaration dispatch succeeds");

    let LeanWorkerDeclarationInspectionResult::Found { declaration } = result else {
        panic!("known definition should be found, got {result:?}");
    };
    assert_eq!(declaration.kind, "definition");
    assert!(
        declaration
            .statement
            .as_ref()
            .is_some_and(|statement| statement.value.contains("Lean.Name")),
        "definition statement should be rendered: {:?}",
        declaration.statement
    );
}

#[test]
fn inspect_declaration_returns_not_found_for_unknown_name() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let result = session
        .inspect_declaration(
            &LeanWorkerDeclarationInspectionRequest::new("This.Name.Does.Not.Exist"),
            None,
            None,
        )
        .expect("worker inspect_declaration dispatch succeeds");

    assert_eq!(
        result,
        LeanWorkerDeclarationInspectionResult::NotFound {
            name: "This.Name.Does.Not.Exist".to_owned()
        }
    );
}

#[test]
fn inspect_declaration_bounds_docstring_and_reports_attributes() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");
    let request = LeanWorkerDeclarationInspectionRequest {
        name: "LeanRsFixture.SourceRanges.documentedSimpTheorem".to_owned(),
        fields: LeanWorkerDeclarationInspectionFields::default(),
        budgets: LeanWorkerOutputBudgets {
            per_field_bytes: 24,
            total_bytes: 96,
        },
    };

    let result = session
        .inspect_declaration(&request, None, None)
        .expect("worker inspect_declaration dispatch succeeds");

    let LeanWorkerDeclarationInspectionResult::Found { declaration } = result else {
        panic!("documented theorem should be found, got {result:?}");
    };
    assert!(
        declaration.attributes.iter().any(|attr| attr == "simp"),
        "simp attribute should be reported: {:?}",
        declaration.attributes
    );
    assert!(declaration.proof_search.is_simp);
    assert!(declaration.proof_search.is_rw_candidate);
    let docstring = declaration.docstring.expect("docstring should be present");
    assert!(docstring.truncated);
    assert!(docstring.value.len() <= 24);
}

#[test]
fn inspect_declaration_truncates_large_type() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");
    let request = LeanWorkerDeclarationInspectionRequest {
        name: "Nat.rec".to_owned(),
        fields: LeanWorkerDeclarationInspectionFields {
            docstring: false,
            ..LeanWorkerDeclarationInspectionFields::default()
        },
        budgets: LeanWorkerOutputBudgets {
            per_field_bytes: 16,
            total_bytes: 16,
        },
    };

    let result = session
        .inspect_declaration(&request, None, None)
        .expect("worker inspect_declaration dispatch succeeds");

    let LeanWorkerDeclarationInspectionResult::Found { declaration } = result else {
        panic!("Nat.rec should be found, got {result:?}");
    };
    let statement = declaration.statement.expect("statement should be rendered");
    assert!(statement.truncated);
    assert!(statement.value.len() <= 16);
}

#[test]
fn inspect_declaration_default_stays_under_total_budget() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");
    let request = LeanWorkerDeclarationInspectionRequest {
        name: "LeanRsFixture.SourceRanges.documentedSimpTheorem".to_owned(),
        fields: LeanWorkerDeclarationInspectionFields::default(),
        budgets: LeanWorkerOutputBudgets {
            per_field_bytes: 32,
            total_bytes: 48,
        },
    };

    let result = session
        .inspect_declaration(&request, None, None)
        .expect("worker inspect_declaration dispatch succeeds");

    let LeanWorkerDeclarationInspectionResult::Found { declaration } = result else {
        panic!("documented theorem should be found, got {result:?}");
    };
    let rendered_bytes = declaration
        .statement
        .as_ref()
        .map_or(0, |statement| statement.value.len())
        + declaration
            .docstring
            .as_ref()
            .map_or(0, |docstring| docstring.value.len());
    assert!(rendered_bytes <= 48, "rendered text exceeded budget: {rendered_bytes}");
}

#[test]
fn search_result_name_can_be_inspected() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");
    let search = LeanWorkerDeclarationSearch {
        name_fragment: Some("knownTheorem".to_owned()),
        name_match: LeanWorkerDeclarationNameMatch::Suffix,
        kind: Some("theorem".to_owned()),
        required_constants: Vec::new(),
        conclusion_head: None,
        scope_biases: Vec::new(),
        limit: 1,
        filter: LeanWorkerDeclarationFilter {
            include_private: false,
            include_generated: false,
            include_internal: false,
        },
        include_source: false,
    };
    let result = session
        .search_declarations(&search, None, None)
        .expect("worker search_declarations dispatch succeeds");
    let name = result
        .declarations
        .first()
        .expect("search should return known theorem")
        .name
        .clone();

    let inspected = session
        .inspect_declaration(&LeanWorkerDeclarationInspectionRequest::new(name.clone()), None, None)
        .expect("worker inspect_declaration dispatch succeeds");

    let LeanWorkerDeclarationInspectionResult::Found { declaration } = inspected else {
        panic!("search result should inspect successfully, got {inspected:?}");
    };
    assert_eq!(declaration.name, name);
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
fn oversized_terminal_response_is_request_error_not_child_death() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config().max_frame_bytes(64 * 1024)).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");
    let names = vec!["Nat.rec"; 512];

    let err = session
        .describe_bulk(&names, None, None)
        .expect_err("oversized describe_bulk should return a structured worker error");
    match err {
        LeanWorkerError::Worker { code, .. } => {
            assert_eq!(code, "lean_rs.worker.output_frame_too_large");
        }
        other => panic!("expected structured output-frame error, got {other:?}"),
    }

    let rendered = session
        .infer_type("(Nat.succ 0 : Nat)", &LeanWorkerElabOptions::new(), None, None)
        .expect("worker should still accept later requests");
    assert!(
        matches!(rendered, LeanWorkerMetaResult::Ok { .. }),
        "worker should survive oversized output: {rendered:?}"
    );
}

#[test]
fn process_module_query_returns_diagnostics_through_worker() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let outcome = session
        .process_module_query(
            "import Lean\n\ntheorem ok : True := by trivial\n",
            LeanWorkerModuleQuery::Diagnostics,
            &opts,
            None,
            None,
        )
        .expect("worker process_module_query dispatch succeeds");

    match outcome {
        LeanWorkerModuleQueryOutcome::Ok {
            result: LeanWorkerModuleQueryResult::Diagnostics(diagnostics),
            imports,
        } => {
            assert_eq!(imports, vec!["Lean".to_string()]);
            assert!(
                diagnostics.diagnostics.iter().all(|d| d.severity != "error"),
                "valid body should not produce error diagnostics, got {diagnostics:?}",
            );
        }
        other => panic!("expected Ok diagnostics outcome, got {other:?}"),
    }
}

#[test]
fn process_module_query_returns_cursor_type_and_goal_through_worker() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let source = "def x := 1\ntheorem t : x = 1 := by rfl\n";
    let type_outcome = session
        .process_module_query(
            source,
            LeanWorkerModuleQuery::TypeAt { line: 2, column: 13 },
            &opts,
            None,
            None,
        )
        .expect("worker type-at dispatch succeeds");
    match type_outcome {
        LeanWorkerModuleQueryOutcome::Ok {
            result: LeanWorkerModuleQueryResult::TypeAt(result),
            ..
        } => match result {
            lean_rs_worker_parent::LeanWorkerTypeAtResult::Term {
                span,
                expr,
                type_str,
                expected_type: _,
            } => {
                assert_eq!(span.start_line, 2);
                assert!(!expr.value.is_empty(), "selected expression should render");
                assert!(!type_str.value.is_empty(), "selected inferred type should render");
                assert!(expr.value.len() <= 64 * 1024);
                assert!(type_str.value.len() <= 64 * 1024);
            }
            other => panic!("expected selected term, got {other:?}"),
        },
        other => panic!("expected Ok type-at outcome, got {other:?}"),
    }

    let goal_outcome = session
        .process_module_query(
            "theorem t : True := by\n  trivial\n",
            LeanWorkerModuleQuery::GoalAt { line: 2, column: 4 },
            &opts,
            None,
            None,
        )
        .expect("worker goal-at dispatch succeeds");
    match goal_outcome {
        LeanWorkerModuleQueryOutcome::Ok {
            result: LeanWorkerModuleQueryResult::GoalAt(result),
            ..
        } => match result {
            lean_rs_worker_parent::LeanWorkerGoalAtResult::Goal {
                span,
                goals_before,
                goals_after: _,
                truncated: _,
            } => {
                assert_eq!(span.start_line, 2);
                assert!(
                    goals_before.iter().any(|goal| goal.contains("True")),
                    "goal before `trivial` should mention True, got {goals_before:?}",
                );
            }
            other => panic!("expected selected tactic goals, got {other:?}"),
        },
        other => panic!("expected Ok goal-at outcome, got {other:?}"),
    }
}

#[test]
fn process_module_query_references_through_worker() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let source = "def x := 1\n#check x\n";

    let outcome = session
        .process_module_query(
            source,
            LeanWorkerModuleQuery::References { name: "x".to_string() },
            &opts,
            None,
            None,
        )
        .expect("worker references dispatch succeeds");

    match outcome {
        LeanWorkerModuleQueryOutcome::Ok {
            result: LeanWorkerModuleQueryResult::References(result),
            imports,
        } => {
            assert!(imports.is_empty(), "body-only source should not report imports");
            assert!(!result.references.is_empty(), "expected references for local x");
            assert!(
                result.references.iter().all(|r| r.name.ends_with('x')),
                "reference projection should include matching names only, got {:?}",
                result.references,
            );
        }
        other => panic!("expected Ok references outcome, got {other:?}"),
    }
}

#[test]
fn process_module_query_batch_returns_diagnostics_and_proof_state_through_worker() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let source = "theorem t (h : True) : True := by\n  exact h\n";
    let outcome = session
        .process_module_query_batch(
            source,
            &[
                LeanWorkerModuleQuerySelector::Diagnostics {
                    id: "diagnostics".to_owned(),
                },
                LeanWorkerModuleQuerySelector::ProofState {
                    id: "state".to_owned(),
                    line: 2,
                    column: 4,
                },
                LeanWorkerModuleQuerySelector::DeclarationTarget {
                    id: "target".to_owned(),
                    name: Some("t".to_owned()),
                    line: None,
                    column: None,
                },
            ],
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("worker process_module_query_batch dispatch succeeds");

    let LeanWorkerModuleQueryBatchOutcome::Ok { result, imports, .. } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    assert!(imports.is_empty(), "body-only source should not report imports");
    assert_eq!(result.items.len(), 3);
    assert!(!result.total_truncated, "small fixture should fit default budget");

    let diagnostics = result
        .items
        .iter()
        .find(|item| matches!(item, LeanWorkerModuleQueryBatchItem::Ok { id, .. } if id == "diagnostics"))
        .expect("diagnostics item present");
    match diagnostics {
        LeanWorkerModuleQueryBatchItem::Ok { result, .. } => match result.as_ref() {
            LeanWorkerModuleQueryBatchResult::Diagnostics(failure) => assert!(
                failure.diagnostics.iter().all(|d| d.severity != "error"),
                "valid proof should not produce error diagnostics, got {failure:?}",
            ),
            other => panic!("expected diagnostics result, got {other:?}"),
        },
        other => panic!("expected diagnostics result, got {other:?}"),
    }

    let proof_state = result
        .items
        .iter()
        .find(|item| matches!(item, LeanWorkerModuleQueryBatchItem::Ok { id, .. } if id == "state"))
        .expect("proof-state item present");
    match proof_state {
        LeanWorkerModuleQueryBatchItem::Ok { result, .. } => match result.as_ref() {
            LeanWorkerModuleQueryBatchResult::ProofState(LeanWorkerProofStateResult::State { info }) => {
                assert!(
                    info.goals_before.iter().any(|goal| goal.contains("True")),
                    "goal before `exact h` should mention True, got {:?}",
                    info.goals_before,
                );
                assert!(
                    info.locals.iter().any(|local| local.name == "h"),
                    "local context should include hypothesis h, got {:?}",
                    info.locals,
                );
                assert!(
                    info.safe_edit.is_some(),
                    "proof state should include a safe edit declaration span"
                );
            }
            other => panic!("expected proof-state result, got {other:?}"),
        },
        other => panic!("expected proof-state result, got {other:?}"),
    }
}

#[test]
fn process_module_query_batch_obeys_total_budget_without_killing_worker() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let source = "theorem t (h : True) : True := by\n  exact h\n";
    let outcome = session
        .process_module_query_batch(
            source,
            &[LeanWorkerModuleQuerySelector::ProofState {
                id: "state".to_owned(),
                line: 2,
                column: 4,
            }],
            &LeanWorkerOutputBudgets {
                per_field_bytes: 128,
                total_bytes: 0,
            },
            &opts,
            None,
            None,
        )
        .expect("budget exhaustion is a normal batch outcome");

    let LeanWorkerModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok batch outcome with budgeted item, got {outcome:?}");
    };
    assert!(result.total_truncated, "batch should report total truncation");
    assert!(
        matches!(
            result.items.as_slice(),
            [LeanWorkerModuleQueryBatchItem::BudgetExceeded { id, .. }] if id == "state"
        ),
        "expected budget-exceeded selector item, got {:?}",
        result.items,
    );

    let rendered = session
        .infer_type("(Nat.succ 0 : Nat)", &LeanWorkerElabOptions::new(), None, None)
        .expect("worker should still accept later requests");
    assert!(
        matches!(rendered, LeanWorkerMetaResult::Ok { .. }),
        "worker should survive budgeted batch output: {rendered:?}"
    );
}

#[test]
fn attempt_proof_successful_candidate_closes_simple_goal() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/attempt/success.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let request = LeanWorkerProofAttemptRequest {
        source:
            "import Lean\n\ntheorem t : True := by\n  skip\n/-- following docstring must remain parseable -/\ntheorem u : True := by\n  trivial\n"
                .to_owned(),
        edit: LeanWorkerProofEditTarget::Declaration {
            name: "t".to_owned(),
            position: LeanWorkerProofPositionSelector::default(),
        },
        candidates: vec![LeanWorkerProofCandidate {
            id: "trivial".to_owned(),
            text: "trivial".to_owned(),
        }],
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .attempt_proof(&request, &opts, None, None)
        .expect("attempt_proof dispatch succeeds");

    let LeanWorkerProofAttemptResult::Ok { result, .. } = result else {
        panic!("expected Ok proof attempt, got {result:?}");
    };
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].status, LeanWorkerProofAttemptStatus::Closed);
    assert_eq!(result.candidates[0].candidate_text.value, "trivial");
    assert!(
        result.candidates[0].goals.is_empty(),
        "closed proof should have no goals"
    );
    assert!(
        result.candidates[0].declaration.is_some(),
        "declaration target should resolve"
    );
    assert!(
        result.candidates[0].proof_position.is_some(),
        "proof attempts should report the selected proof position"
    );
    assert!(
        result.candidates[0]
            .downstream_diagnostics
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("unexpected token '/--'")),
        "candidate overlay must not corrupt the following docstring"
    );
}

#[test]
fn attempt_proof_bad_candidate_on_unicode_signature_does_not_false_close() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/attempt/unicode.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let request = LeanWorkerProofAttemptRequest {
        source: "theorem unicodeSig (hα : True) : True := by\n  skip\n/-- following docstring must remain parseable -/\ntheorem after : True := by\n  trivial\n".to_owned(),
        edit: LeanWorkerProofEditTarget::Declaration {
            name: "unicodeSig".to_owned(),
            position: LeanWorkerProofPositionSelector::default(),
        },
        candidates: vec![LeanWorkerProofCandidate {
            id: "bad".to_owned(),
            text: "exact definitely_missing_identifier".to_owned(),
        }],
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .attempt_proof(&request, &opts, None, None)
        .expect("unicode signature proof attempt dispatch succeeds");
    let LeanWorkerProofAttemptResult::Ok { result, .. } = result else {
        panic!("expected Ok proof attempt, got {result:?}");
    };
    let row = &result.candidates[0];
    assert_eq!(row.candidate_text.value, "exact definitely_missing_identifier");
    assert_eq!(
        row.status,
        LeanWorkerProofAttemptStatus::Failed,
        "invalid candidate must not be reported as closed: {row:?}",
    );
    assert!(
        row.diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error"
                && diagnostic.message.contains("definitely_missing_identifier")),
        "bad proof should return candidate-local diagnostics: {:?}",
        row.diagnostics,
    );
    assert!(
        row.downstream_diagnostics
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("unexpected token '/--'")),
        "candidate overlay must not corrupt the following docstring: {:?}",
        row.downstream_diagnostics,
    );
}

#[test]
fn attempt_proof_bad_candidate_returns_diagnostics_and_session_survives() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/attempt/bad.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let request = LeanWorkerProofAttemptRequest {
        source: "theorem t : True := by\n  skip\n/-- following docstring must remain parseable -/\ntheorem u : True := by\n  trivial\n".to_owned(),
        edit: LeanWorkerProofEditTarget::Declaration {
            name: "t".to_owned(),
            position: LeanWorkerProofPositionSelector::default(),
        },
        candidates: vec![LeanWorkerProofCandidate {
            id: "bad".to_owned(),
            text: "exact missingIdentifier".to_owned(),
        }],
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .attempt_proof(&request, &opts, None, None)
        .expect("bad candidate is a normal proof attempt result");
    let LeanWorkerProofAttemptResult::Ok { result, .. } = result else {
        panic!("expected Ok proof attempt, got {result:?}");
    };
    assert_eq!(result.candidates[0].status, LeanWorkerProofAttemptStatus::Failed);
    assert!(
        result.candidates[0]
            .diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error" && diagnostic.message.contains("missingIdentifier")),
        "bad proof should return error diagnostics: {:?}",
        result.candidates[0].diagnostics,
    );

    let later = session
        .process_module_query(
            "theorem u : True := by\n  trivial\n",
            LeanWorkerModuleQuery::Diagnostics,
            &LeanWorkerElabOptions::new(),
            None,
            None,
        )
        .expect("later module query still succeeds");
    assert!(matches!(later, LeanWorkerModuleQueryOutcome::Ok { .. }));
}

/// Regression for the type-safe proof-splice fix: when the proof's tactic sits
/// on the same line as `by` (not at the line's start), the `.default` selector
/// must resolve the real tactic, not the bare `by` keyword atom, and the
/// candidate must be indented to that tactic's column. Before the fix the `by`
/// atom became `positions[0]` and the candidate was indented to the `by` line's
/// leading whitespace (column 0) — which Lean >= 4.31 rejects with
/// `unexpected identifier; expected command`. This is the smallest case that
/// catches a regression of either degree of freedom (atom selection, or
/// indentation derived from the wrong line).
#[test]
fn attempt_proof_single_line_by_tactic_aligns_and_closes() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/attempt/single-line.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let request = LeanWorkerProofAttemptRequest {
        source: "theorem t : True := by skip\n".to_owned(),
        edit: LeanWorkerProofEditTarget::Declaration {
            name: "t".to_owned(),
            position: LeanWorkerProofPositionSelector::default(),
        },
        candidates: vec![LeanWorkerProofCandidate {
            id: "trivial".to_owned(),
            text: "trivial".to_owned(),
        }],
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .attempt_proof(&request, &opts, None, None)
        .expect("single-line proof attempt dispatch succeeds");
    let LeanWorkerProofAttemptResult::Ok { result, .. } = result else {
        panic!("expected Ok proof attempt, got {result:?}");
    };
    let row = &result.candidates[0];

    // The selected position must be the real `skip` tactic, never the `by` atom.
    let position = row
        .proof_position
        .as_ref()
        .expect("single-line proof attempt reports a proof position");
    assert_eq!(
        position.tactic.value.trim(),
        "skip",
        "default selector must resolve the real tactic, not the `by` keyword: {position:?}",
    );

    // No parse error from a dedented (column-0) splice, on any Lean version.
    for diagnostic in row
        .diagnostics
        .diagnostics
        .iter()
        .chain(row.downstream_diagnostics.diagnostics.iter())
    {
        assert!(
            !diagnostic.message.contains("unexpected identifier; expected command"),
            "candidate must not be spliced as a dedented top-level command: {diagnostic:?}",
        );
    }

    // Aligned to `skip`'s column, `trivial` runs after `skip` and closes `True`.
    assert_eq!(
        row.status,
        LeanWorkerProofAttemptStatus::Closed,
        "aligned candidate should close the goal: {row:?}",
    );
}

#[test]
fn attempt_proof_candidate_list_is_capped_and_does_not_write_files() {
    ensure_fixture_built();
    let fixture = workspace_root().join("fixtures/lean/LeanRsFixture/SourceRanges.lean");
    let before = fs::read_to_string(&fixture).expect("fixture reads before proof attempt");
    let opts = LeanWorkerElabOptions::new().file_label("/attempt/cap.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let candidates = (0..10)
        .map(|idx| LeanWorkerProofCandidate {
            id: format!("c{idx}"),
            text: "trivial".to_owned(),
        })
        .collect();
    let request = LeanWorkerProofAttemptRequest {
        source: "theorem t : True := by\n  skip\n".to_owned(),
        edit: LeanWorkerProofEditTarget::Declaration {
            name: "t".to_owned(),
            position: LeanWorkerProofPositionSelector::default(),
        },
        candidates,
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .attempt_proof(&request, &opts, None, None)
        .expect("capped attempt succeeds");
    let LeanWorkerProofAttemptResult::Ok { result, .. } = result else {
        panic!("expected Ok proof attempt, got {result:?}");
    };
    assert_eq!(result.candidate_limit, 8);
    assert!(result.candidates.len() <= 8);

    let after = fs::read_to_string(&fixture).expect("fixture reads after proof attempt");
    assert_eq!(before, after, "proof attempts must not mutate source files");
}

#[test]
fn verify_declaration_accepts_closed_theorem_and_rejects_sorry() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/verify/theorem.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let closed = LeanWorkerDeclarationVerificationRequest {
        source: "theorem t : True := by\n  trivial\n".to_owned(),
        target: LeanWorkerDeclarationVerificationTarget::Name { name: "t".to_owned() },
        sorry_policy: LeanWorkerSorryPolicy::Deny,
        report_axioms: true,
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .verify_declaration(&closed, &opts, None, None)
        .expect("closed theorem verification succeeds");
    match result {
        LeanWorkerDeclarationVerificationResult::Ok {
            verification_status,
            facts,
            ..
        } => {
            assert_eq!(verification_status, LeanWorkerDeclarationVerificationStatus::Accepted);
            assert!(!facts.contains_sorry);
            assert!(facts.unresolved_goals.is_empty());
            assert!(
                facts.axioms_available,
                "a resolved declaration with report_axioms should run the axiom walk (axioms_available=true)"
            );
        }
        other => panic!("expected accepted verification, got {other:?}"),
    }

    let sorry = LeanWorkerDeclarationVerificationRequest {
        source: "theorem t : True := by\n  sorry\n".to_owned(),
        target: LeanWorkerDeclarationVerificationTarget::Name { name: "t".to_owned() },
        sorry_policy: LeanWorkerSorryPolicy::Deny,
        report_axioms: true,
        budgets: LeanWorkerOutputBudgets::default(),
    };
    let result = session
        .verify_declaration(&sorry, &opts, None, None)
        .expect("sorry theorem verification is a normal result");
    match result {
        LeanWorkerDeclarationVerificationResult::Ok {
            verification_status,
            facts,
            ..
        } => {
            assert_eq!(verification_status, LeanWorkerDeclarationVerificationStatus::Rejected);
            assert!(facts.contains_sorry);
            assert!(facts.axioms.iter().any(|axiom| axiom == "sorryAx"));
        }
        other => panic!("expected rejected verification, got {other:?}"),
    }
}

#[test]
fn process_module_query_batch_reuses_snapshot_for_same_file_content() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/cache/reuse.lean");
    let mut worker = LeanWorker::spawn(&cache_worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let source = "theorem t (h : True) : True := by\n  exact h\n";
    let selectors = cache_probe_selectors();

    let first = session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("first batch succeeds");
    assert_batch_has_state(&first);
    assert_eq!(batch_facts(&first).cache_status, LeanWorkerModuleCacheStatus::Miss);

    let second = session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("second batch succeeds");
    assert_batch_has_state(&second);
    let facts = batch_facts(&second);
    assert_eq!(facts.cache_status, LeanWorkerModuleCacheStatus::Hit);
    assert!(facts.cache_entry_count.is_some());
    assert!(facts.cache_approx_bytes.is_some());
}

#[test]
fn process_module_query_batch_rebuilds_when_content_changes() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/cache/rebuild.lean");
    let mut worker = LeanWorker::spawn(&cache_worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let selectors = cache_probe_selectors();

    let first = session
        .process_module_query_batch(
            "theorem t (h : True) : True := by\n  exact h\n",
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("first batch succeeds");
    assert_eq!(batch_facts(&first).cache_status, LeanWorkerModuleCacheStatus::Miss);

    let changed = session
        .process_module_query_batch(
            "theorem t (h : True) : True := by\n  trivial\n",
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("changed-content batch succeeds");
    assert_batch_has_state(&changed);
    assert_eq!(batch_facts(&changed).cache_status, LeanWorkerModuleCacheStatus::Rebuilt);
}

#[test]
fn process_module_query_batch_clear_evicts_snapshot() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/cache/clear.lean");
    let mut worker = LeanWorker::spawn(&cache_worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let source = "theorem t (h : True) : True := by\n  exact h\n";
    let selectors = cache_probe_selectors();

    session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("populate cache");
    let cleared = session
        .clear_module_snapshot_cache(None, None)
        .expect("manual clear succeeds");
    assert!(
        cleared.entries_cleared >= 1,
        "expected at least one cache entry cleared"
    );

    let after_clear = session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("batch after clear succeeds");
    assert_ne!(batch_facts(&after_clear).cache_status, LeanWorkerModuleCacheStatus::Hit);
}

#[test]
fn process_module_query_batch_changed_session_imports_miss() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/cache/imports.lean");
    let mut worker = LeanWorker::spawn(&cache_worker_config()).expect("worker starts");
    let source = "theorem t (h : True) : True := by\n  exact h\n";
    let selectors = cache_probe_selectors();
    {
        let mut session = worker
            .open_session(&elaboration_session_config(), None, None)
            .expect("worker session opens");
        session
            .process_module_query_batch(
                source,
                &selectors,
                &LeanWorkerOutputBudgets::default(),
                &opts,
                None,
                None,
            )
            .expect("first session batch succeeds");
    }

    let changed_imports = LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsHostShims.Elaboration", "Lean"],
    );
    let mut session = worker
        .open_session(&changed_imports, None, None)
        .expect("worker session with changed imports opens");
    let outcome = session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("changed-import batch succeeds");
    assert_eq!(batch_facts(&outcome).cache_status, LeanWorkerModuleCacheStatus::Miss);
}

#[test]
fn process_module_query_batch_ttl_evicts_idle_snapshot() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/cache/ttl.lean");
    let mut worker =
        LeanWorker::spawn(&cache_worker_config().env("LEAN_RS_MODULE_CACHE_TTL_MILLIS", "1")).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let source = "theorem t (h : True) : True := by\n  exact h\n";
    let selectors = cache_probe_selectors();

    session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("populate cache");
    std::thread::sleep(Duration::from_millis(5));
    let after_ttl = session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("batch after ttl succeeds");
    assert_eq!(
        batch_facts(&after_ttl).cache_status,
        LeanWorkerModuleCacheStatus::Evicted
    );
}

#[test]
fn process_module_query_batch_entry_limit_evicts_old_entries() {
    ensure_fixture_built();
    let mut worker =
        LeanWorker::spawn(&cache_worker_config().env("LEAN_RS_MODULE_CACHE_MAX_ENTRIES", "1")).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let selectors = cache_probe_selectors();
    let source = "theorem t (h : True) : True := by\n  exact h\n";

    let first_opts = LeanWorkerElabOptions::new().file_label("/cache/entry-a.lean");
    session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &first_opts,
            None,
            None,
        )
        .expect("first file populates cache");
    let second_opts = LeanWorkerElabOptions::new().file_label("/cache/entry-b.lean");
    let second = session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &second_opts,
            None,
            None,
        )
        .expect("second file populates cache");
    assert!(batch_facts(&second).cache_entry_count <= Some(1));

    let first_again = session
        .process_module_query_batch(
            source,
            &selectors,
            &LeanWorkerOutputBudgets::default(),
            &first_opts,
            None,
            None,
        )
        .expect("first file after entry eviction succeeds");
    assert_ne!(batch_facts(&first_again).cache_status, LeanWorkerModuleCacheStatus::Hit);
}

#[test]
fn process_module_query_batch_tiny_cache_still_returns_correct_query() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/cache/tiny.lean");
    let mut worker =
        LeanWorker::spawn(&cache_worker_config().env("LEAN_RS_MODULE_CACHE_MAX_BYTES", "1")).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let outcome = session
        .process_module_query_batch(
            "theorem t (h : True) : True := by\n  exact h\n",
            &cache_probe_selectors(),
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("tiny-cache batch succeeds");
    assert_batch_has_state(&outcome);
    let facts = batch_facts(&outcome);
    assert_eq!(facts.cache_status, LeanWorkerModuleCacheStatus::Miss);
    assert!(facts.output_bytes > 0);
}

#[test]
fn process_module_query_handles_module_system_header_through_worker() {
    let project = TempLakeProject::new("module-system-header-typed-session");
    write_module_syntax_fixture(&project);
    let opts = LeanWorkerElabOptions::new();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let config = LeanWorkerSessionConfig::shims_only(
        project.path().to_path_buf(),
        ["Fixture.Imported", "Fixture.Internal", "Fixture.PrivateScope"],
    );
    let mut session = worker
        .open_session(&config, None, None)
        .expect("worker shims-only session opens");

    let source = "\
module

public import Fixture.Imported
import all Fixture.Internal
import Fixture.PrivateScope

def moduleSyntaxFoo : Nat := imported + internalSecret
";
    let outcome = session
        .process_module_query(source, LeanWorkerModuleQuery::Diagnostics, &opts, None, None)
        .expect("worker process_module_query dispatch succeeds");

    match outcome {
        LeanWorkerModuleQueryOutcome::Ok {
            result: LeanWorkerModuleQueryResult::Diagnostics(diagnostics),
            imports,
        } => {
            assert_eq!(
                imports,
                vec![
                    "Fixture.Imported".to_string(),
                    "Fixture.Internal".to_string(),
                    "Fixture.PrivateScope".to_string(),
                ],
                "imports must be bare module names, without `public` or `all` modifiers",
            );
            let diagnostics = &diagnostics.diagnostics;
            assert!(
                !diagnostics
                    .iter()
                    .any(|d| d.severity == "error" && d.message.contains("internalSecret")),
                "`import all` under `module` must expose private declarations from the imported module, got {diagnostics:?}",
            );
        }
        LeanWorkerModuleQueryOutcome::MissingImports { missing, .. } => {
            panic!("expected Ok module outcome, got MissingImports({missing:?})")
        }
        LeanWorkerModuleQueryOutcome::HeaderParseFailed { diagnostics } => {
            panic!("expected Ok module outcome, got HeaderParseFailed({diagnostics:?})")
        }
        LeanWorkerModuleQueryOutcome::Unsupported => {
            panic!("expected Ok module outcome, got Unsupported")
        }
        other => panic!("expected Ok module outcome, got {other:?}"),
    }
}

// Regression for prompt 12: a `module` + `@[expose] public section` overlay
// with a single namespaced theorem resolves to a unique declaration against a
// complete (green) session env — for both the short name and the
// fully-qualified name. The field report's spurious `ambiguous` verdict is
// degraded-environment-specific, NOT a `module`/`public section`
// double-counting artifact, so the unique-resolution path must stay clean.
#[test]
fn module_public_section_resolves_to_unique_declaration() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/verify/module.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let source = "\
module

@[expose] public section
namespace SSet.boundary

theorem isPushout : True := by
  trivial

end SSet.boundary
end
";
    for name in ["isPushout", "SSet.boundary.isPushout"] {
        let request = LeanWorkerDeclarationVerificationRequest {
            source: source.to_owned(),
            target: LeanWorkerDeclarationVerificationTarget::Name { name: name.to_owned() },
            sorry_policy: LeanWorkerSorryPolicy::Deny,
            report_axioms: true,
            budgets: LeanWorkerOutputBudgets::default(),
        };
        let result = session
            .verify_declaration(&request, &opts, None, None)
            .expect("verification dispatch succeeds");
        match result {
            LeanWorkerDeclarationVerificationResult::Ok {
                verification_status,
                facts,
                ..
            } => {
                assert_eq!(
                    verification_status,
                    LeanWorkerDeclarationVerificationStatus::Accepted,
                    "module/public-section overlay must resolve uniquely for target {name:?}, not report Ambiguous",
                );
                assert_eq!(
                    facts.target.as_ref().map(|t| t.declaration_name.as_str()),
                    Some("SSet.boundary.isPushout"),
                    "resolved target should be the fully-qualified namespaced theorem for {name:?}",
                );
            }
            other => panic!("expected accepted verification for {name:?}, got {other:?}"),
        }
    }
}

// Prompt 12 target B: a short name that genuinely matches declarations in two
// different namespaces resolves to `Ambiguous` carrying the competing
// declarations (post-dedup, these survive because they have distinct
// fully-qualified `declaration_name`s). `target` is `None` for an ambiguous
// verdict.
#[test]
fn verify_declaration_reports_ambiguous_with_candidates() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/verify/ambiguous.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let source = "\
namespace A
theorem dup : True := by trivial
end A

namespace B
theorem dup : True := by trivial
end B
";
    let request = LeanWorkerDeclarationVerificationRequest {
        source: source.to_owned(),
        target: LeanWorkerDeclarationVerificationTarget::Name { name: "dup".to_owned() },
        sorry_policy: LeanWorkerSorryPolicy::Deny,
        report_axioms: true,
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .verify_declaration(&request, &opts, None, None)
        .expect("verification dispatch succeeds");
    match result {
        LeanWorkerDeclarationVerificationResult::Ok {
            verification_status,
            facts,
            ..
        } => {
            assert_eq!(
                verification_status,
                LeanWorkerDeclarationVerificationStatus::Ambiguous,
                "a short name defined in two namespaces must resolve to Ambiguous",
            );
            assert!(
                facts.candidates.len() >= 2,
                "Ambiguous must carry the competing declarations, got {:?}",
                facts.candidates,
            );
            assert!(
                facts.target.is_none(),
                "an ambiguous verdict has no single resolved target, got {:?}",
                facts.target,
            );
            let names: Vec<&str> = facts.candidates.iter().map(|c| c.declaration_name.as_str()).collect();
            assert!(
                names.contains(&"A.dup") && names.contains(&"B.dup"),
                "candidates should name both namespaced declarations, got {names:?}",
            );
        }
        other => panic!("expected ambiguous verification, got {other:?}"),
    }
}

// Prompt 13 Part 1: faithful reproduction of the field `child_abort` scenario.
// Unlike the test above (which only defines two same-short-name declarations and
// never elaborates an ambiguous *reference*), this source `open`s both namespaces
// and uses the bare name, forcing Lean to emit an "ambiguous, possible
// interpretations" elaboration error. Rendering that message
// (`serializeMessages` -> `MessageData.toString`) is the one metavar-touching
// step on the `Ambiguous` verdict path, so it is where a
// `Lean.MetavarContext.getDecl … unknown metavariable` panic + SIGABRT would
// surface (report 61 §3). The invariant under test: a read-only resolution query
// must resolve correctly *and* leave the child alive — the supervisor must not
// have to restart it (`retry_count` stayed 0 in the field's healthy case).
#[test]
fn verify_declaration_ambiguous_open_reference_does_not_restart_child() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/verify/ambiguous-open.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    // Scope the session so its mutable borrow of `worker` ends before we read
    // lifecycle stats.
    {
        let mut session = worker
            .open_session(&elaboration_session_config(), None, None)
            .expect("worker session opens");

        let source = "\
namespace AmbigA
def collide : Nat := 0
end AmbigA

namespace AmbigB
def collide : Nat := 1
end AmbigB

open AmbigA AmbigB

example : Nat := collide
";
        let request = LeanWorkerDeclarationVerificationRequest {
            source: source.to_owned(),
            target: LeanWorkerDeclarationVerificationTarget::Name {
                name: "collide".to_owned(),
            },
            sorry_policy: LeanWorkerSorryPolicy::Deny,
            report_axioms: true,
            budgets: LeanWorkerOutputBudgets::default(),
        };

        let result = session
            .verify_declaration(&request, &opts, None, None)
            .expect("verification dispatch succeeds");
        match result {
            LeanWorkerDeclarationVerificationResult::Ok {
                verification_status,
                facts,
                ..
            } => {
                assert_eq!(
                    verification_status,
                    LeanWorkerDeclarationVerificationStatus::Ambiguous,
                    "a short name defined in two open namespaces must resolve to Ambiguous",
                );
                let names: Vec<&str> = facts.candidates.iter().map(|c| c.declaration_name.as_str()).collect();
                assert!(
                    names.contains(&"AmbigA.collide") && names.contains(&"AmbigB.collide"),
                    "candidates should name both competing declarations, got {names:?}",
                );
                // Prove the suspect path actually ran: the bare `collide`
                // reference must have produced an ambiguity diagnostic that
                // `serializeMessages` rendered. Without this, a future change
                // that stops elaborating the reference would silently turn this
                // into a degenerate clean-elaboration test that no longer guards
                // the metavar-rendering boundary.
                assert!(
                    facts
                        .diagnostics
                        .diagnostics
                        .iter()
                        .any(|d| d.message.to_lowercase().contains("ambiguous")),
                    "the open-namespace reference must yield a rendered ambiguity diagnostic, got {:?}",
                    facts.diagnostics.diagnostics,
                );
            }
            other => panic!("expected ambiguous verification, got {other:?}"),
        }
    }

    // The field defect was `call_restart.cause = "child_abort"` with
    // `retry_count = 1`: a kernel panic aborted the child on this read-only query
    // and the supervisor silently restarted it. Assert that did not happen.
    assert_eq!(
        worker.stats().restarts,
        0,
        "an ambiguous-name resolution query must not crash the child (no supervisor restart)",
    );
}

// Prompt 12 target A2: a source whose header imports a module absent from the
// open session environment, queried for a name it does not define, yields the
// typed `NeedsBuild` verdict inside the `MissingImports` outcome that names the
// absent module — not a bare `NotFound` or an infrastructure error string.
#[test]
fn verify_declaration_reports_needs_build_for_unbuilt_import() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/verify/needs-build.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let source = "\
import LeanRsFixture.DoesNotExist

theorem present : True := by trivial
";
    let request = LeanWorkerDeclarationVerificationRequest {
        source: source.to_owned(),
        target: LeanWorkerDeclarationVerificationTarget::Name {
            name: "notDefinedHere".to_owned(),
        },
        sorry_policy: LeanWorkerSorryPolicy::Deny,
        report_axioms: true,
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let result = session
        .verify_declaration(&request, &opts, None, None)
        .expect("verification dispatch succeeds");
    match result {
        LeanWorkerDeclarationVerificationResult::MissingImports {
            verification_status,
            missing,
            ..
        } => {
            assert_eq!(
                verification_status,
                LeanWorkerDeclarationVerificationStatus::NeedsBuild,
                "an unresolved name under incomplete imports must report NeedsBuild",
            );
            assert!(
                missing.iter().any(|m| m == "LeanRsFixture.DoesNotExist"),
                "the absent import should be named in `missing`, got {missing:?}",
            );
        }
        other => panic!("expected MissingImports/NeedsBuild outcome, got {other:?}"),
    }
}

// Prompt 13 Part 2: a module query against a file with an incomplete import
// closure must degrade in O(parse-header), not pay a full failing body
// elaboration whose output the parent discards. The body here references an
// undefined symbol: if it were elaborated against the import-incomplete
// environment it would emit an unknown-identifier error and `elaboration_micros`
// would be non-zero. The short-circuit skips `processCommands` entirely, so the
// `Diagnostics` selector is empty and `elaboration_micros` stays 0 — the per-file
// cost attribution that bounds a project-scope scan's worst case.
#[test]
fn module_query_on_incomplete_closure_skips_body_elaboration() {
    ensure_fixture_built();
    let opts = LeanWorkerElabOptions::new().file_label("/scan/incomplete-closure.lean");
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");

    let source = "\
import LeanRsFixture.DoesNotExist

theorem present : True := totallyUndefinedSymbol
";
    let outcome = session
        .process_module_query_batch(
            source,
            &[
                LeanWorkerModuleQuerySelector::Diagnostics {
                    id: "diagnostics".to_owned(),
                },
                LeanWorkerModuleQuerySelector::References {
                    id: "refs".to_owned(),
                    name: "present".to_owned(),
                },
            ],
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("worker process_module_query_batch dispatch succeeds");

    let LeanWorkerModuleQueryBatchOutcome::MissingImports { result, missing, .. } = &outcome else {
        panic!("expected MissingImports batch outcome, got {outcome:?}");
    };
    assert!(
        missing.iter().any(|m| m == "LeanRsFixture.DoesNotExist"),
        "the absent import should be named in `missing`, got {missing:?}",
    );

    // The skip is exact: `elaboration_micros` is hard-set to 0 because
    // `processCommands` never ran, not merely measured-fast.
    assert_eq!(
        batch_facts(&outcome).timings.elaboration_micros,
        0,
        "an incomplete-closure query must not elaborate the body",
    );

    // Behavioural proof of the skip: had the body been elaborated, the undefined
    // `totallyUndefinedSymbol` reference would surface as an error diagnostic.
    let diagnostics = result
        .items
        .iter()
        .find(|item| matches!(item, LeanWorkerModuleQueryBatchItem::Ok { id, .. } if id == "diagnostics"))
        .expect("diagnostics item present");
    match diagnostics {
        LeanWorkerModuleQueryBatchItem::Ok { result, .. } => match result.as_ref() {
            LeanWorkerModuleQueryBatchResult::Diagnostics(failure) => assert!(
                failure.diagnostics.is_empty(),
                "skipped elaboration must not surface body diagnostics, got {failure:?}",
            ),
            other => panic!("expected diagnostics result, got {other:?}"),
        },
        other => panic!("expected diagnostics Ok item, got {other:?}"),
    }

    let references = result
        .items
        .iter()
        .find(|item| matches!(item, LeanWorkerModuleQueryBatchItem::Ok { id, .. } if id == "refs"))
        .expect("references item present");
    match references {
        LeanWorkerModuleQueryBatchItem::Ok { result, .. } => match result.as_ref() {
            LeanWorkerModuleQueryBatchResult::References(refs) => assert!(
                refs.references.is_empty(),
                "an incomplete-closure scan returns no references, got {refs:?}",
            ),
            other => panic!("expected references result, got {other:?}"),
        },
        other => panic!("expected references Ok item, got {other:?}"),
    }
}

// Prompt 12 target C: inspection renders the statement notation-aware under
// `Pretty` (the default) and falls back to the raw `Expr.toString` form under
// `Raw`. Each path reports the rendering it actually used, and for a
// universe-polymorphic constant the two forms differ (raw carries `@`/universe
// annotations that pretty printing with `pp.universes false` suppresses).
#[test]
fn inspect_declaration_pretty_and_raw_rendering_differ() {
    ensure_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let request_for = |rendering| LeanWorkerDeclarationInspectionRequest {
        name: "Nat.rec".to_owned(),
        fields: LeanWorkerDeclarationInspectionFields {
            docstring: false,
            rendering,
            ..LeanWorkerDeclarationInspectionFields::default()
        },
        budgets: LeanWorkerOutputBudgets {
            per_field_bytes: 4096,
            total_bytes: 8192,
        },
    };

    let pretty = match session
        .inspect_declaration(&request_for(LeanWorkerRendering::Pretty), None, None)
        .expect("pretty inspect dispatch succeeds")
    {
        LeanWorkerDeclarationInspectionResult::Found { declaration } => declaration,
        other => panic!("Nat.rec should be found, got {other:?}"),
    };
    let raw = match session
        .inspect_declaration(&request_for(LeanWorkerRendering::Raw), None, None)
        .expect("raw inspect dispatch succeeds")
    {
        LeanWorkerDeclarationInspectionResult::Found { declaration } => declaration,
        other => panic!("Nat.rec should be found, got {other:?}"),
    };

    assert_eq!(
        pretty.statement_rendering,
        Some(LeanWorkerRendering::Pretty),
        "the pretty-printer is available, so a Pretty request must report Pretty",
    );
    assert_eq!(
        raw.statement_rendering,
        Some(LeanWorkerRendering::Raw),
        "a Raw request must report Raw",
    );
    let pretty_statement = pretty.statement.expect("pretty statement renders").value;
    let raw_statement = raw.statement.expect("raw statement renders").value;
    assert_ne!(
        pretty_statement, raw_statement,
        "pretty (notation, no universes) and raw (Expr.toString) forms of Nat.rec should differ",
    );
}
