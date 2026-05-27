//! Per-method coverage for the typed `LeanWorkerSession` surface added in
//! 0.1.6: `infer_type`, `whnf`, `is_def_eq`, `describe`,
//! `list_declarations_strings`, `describe_bulk`, and `process_module_query`.
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
use std::time::{SystemTime, UNIX_EPOCH};

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerConfig, LeanWorkerDeclarationFilter, LeanWorkerDeclarationSearch, LeanWorkerElabOptions,
    LeanWorkerError, LeanWorkerMetaResult, LeanWorkerMetaTransparency, LeanWorkerModuleQuery,
    LeanWorkerModuleQueryOutcome, LeanWorkerModuleQueryResult, LeanWorkerRendering, LeanWorkerSessionConfig,
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
                query: "add".to_owned(),
                kind: Some("theorem".to_owned()),
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
