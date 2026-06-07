//! End-to-end tests for the `LeanHost` / `LeanCapabilities` /
//! `LeanSession` cascade.
//!
//! Each test bootstraps the runtime, opens the fixture Lake project,
//! loads the `LeanRsFixture` capability dylib plus the checked bundled host
//! shim capability, starts a session over an import list, and
//! exercises the typed query methods.

#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use lean_rs::LeanRuntime;
use lean_rs::error::{HostStage, LeanError};
use lean_toolchain::LEAN_VERSION;

use crate::host::meta::{
    LeanMetaOptions, LeanMetaResponse, LeanMetaService, LeanMetaTransparency, MetaCallStatus, heartbeat_burn,
    infer_type, is_def_eq, pp_expr, whnf,
};
use crate::{
    DeclarationInspectionRequest, DeclarationInspectionResult, EvidenceStatus, LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT,
    LEAN_PROOF_SUMMARY_BYTE_LIMIT, LeanBracketedImportRequest, LeanBracketedImportResult, LeanCancellationToken,
    LeanDeclarationFilter, LeanElabOptions, LeanHost, LeanKernelOutcome, LeanSession, LeanSessionImportProfile,
    LeanSeverity,
};

// -- fixture setup -------------------------------------------------------

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
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
        let root = std::env::temp_dir().join(format!("lean-rs-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).expect("create temporary Lake project");
        fs::write(
            root.join("lean-toolchain"),
            format!("leanprover/lean4:v{LEAN_VERSION}\n"),
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

    fn lake_build(&self, target: &str) -> std::process::Output {
        Command::new("lake")
            .arg("build")
            .arg(target)
            .current_dir(&self.root)
            .output()
            .expect("lake command starts")
    }

    fn lake_build_ok(&self, target: &str) {
        let output = self.lake_build(target);
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

fn write_transitive_dependency_fixture(project: &TempLakeProject) {
    let toolchain = fs::read_to_string(project.path().join("lean-toolchain")).expect("read temp project toolchain");
    project.write(".lake/packages/dep/lean-toolchain", &toolchain);
    project.write(
        ".lake/packages/dep/lakefile.lean",
        "import Lake\nopen Lake DSL\npackage dep\nlean_lib Dep\n",
    );
    project.write(".lake/packages/dep/Dep/Hello.lean", "def Dep.hello : Nat := 41\n");
    let dep_root = project.path().join(".lake").join("packages").join("dep");
    let output = Command::new("lake")
        .arg("build")
        .arg("Dep.Hello")
        .current_dir(&dep_root)
        .output()
        .expect("lake command starts for dependency");
    assert!(
        output.status.success(),
        "`lake build Dep.Hello` failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    project.write(
        "lakefile.lean",
        "import Lake\nopen Lake DSL\npackage consumer\nrequire dep from \"./.lake/packages/dep\"\nlean_lib Consumer\n",
    );
    project.write(
        "Consumer.lean",
        "import Dep.Hello\ndef Consumer.value : Nat := Dep.hello + 1\n",
    );
    project.write(
        "lake-manifest.json",
        r#"{"version":"1.2.0","packagesDir":".lake/packages","packages":[{"type":"path","scope":"","name":"dep","manifestFile":"lake-manifest.json","inherited":false,"dir":"./.lake/packages/dep","configFile":"lakefile.lean"}],"name":"consumer","lakeDir":".lake"}"#,
    );
    project.lake_build_ok("Consumer");
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

// -- from_lake_project ---------------------------------------------------

#[test]
fn from_lake_project_missing_path_is_load_error() {
    let err = LeanHost::from_lake_project(runtime(), "/does/not/exist/lean-rs-fixture")
        .expect_err("opening a nonexistent project root must fail");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Load);
            assert!(
                failure.message().contains("lean-rs-fixture"),
                "diagnostic must name the requested path, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Load) failure, got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected Host(Load) failure, got cancellation {cancelled:?}"),
    }
}

// -- load_capabilities ---------------------------------------------------

#[test]
fn load_capabilities_prepares_checked_host_shims() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("capability dylib loads + symbols resolve");
    // Sanity: caps is move-constructed, no public observable state to
    // assert against. The follow-on tests prove checked shim bindings
    // actually dispatch correctly.
    drop(caps);
}

#[test]
fn load_capabilities_missing_dylib_is_load_error() {
    let host = fixture_host();
    let err = host
        .load_capabilities("does_not_exist", "NoSuchLib")
        .expect_err("missing dylib must fail");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Load);
            assert!(
                failure.message().contains("NoSuchLib"),
                "diagnostic must name the requested library, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Load) failure, got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected Host(Load) failure, got cancellation {cancelled:?}"),
    }
}

#[test]
fn load_shims_only_opens_session_against_user_oleans() {
    let project = TempLakeProject::new("shims-only-good");
    project.write(
        "lakefile.lean",
        "import Lake\nopen Lake DSL\npackage demo_shims\nlean_lib Good\n",
    );
    project.write("Good.lean", "def goodValue : Nat := 41\n");
    project.lake_build_ok("Good");

    let host = LeanHost::from_lake_project(runtime(), project.path()).expect("host opens temp project");
    let caps = host.load_shims_only().expect("shim-only capabilities load");
    let mut session = caps
        .session(&["Good", "LeanRsHostShims.Meta"], None, None)
        .expect("session imports user and shim oleans");

    let declarations = session
        .list_declarations_strings(&LeanDeclarationFilter::default(), None, None)
        .expect("list declarations works");
    assert!(
        declarations.iter().any(|name| name == "goodValue"),
        "user declaration should be visible in shim-only session"
    );

    let expr = session
        .declaration_type("goodValue", None)
        .expect("type query succeeds")
        .expect("goodValue has a type");
    let inferred = session
        .run_meta(&infer_type(), expr, &LeanMetaOptions::new(), None)
        .expect("infer_type dispatch succeeds");
    assert_eq!(inferred.status(), MetaCallStatus::Ok);

    let checked = session
        .kernel_check(
            "theorem good_kernel_check : 1 + 1 = 2 := rfl",
            &LeanElabOptions::new(),
            None,
            None,
        )
        .expect("kernel_check dispatch succeeds");
    assert_eq!(checked.status(), EvidenceStatus::Checked);
}

#[test]
fn load_shims_only_succeeds_when_user_shared_facet_does_not_build() {
    let project = TempLakeProject::new("shims-only-broken");
    project.write(
        "lakefile.lean",
        "import Lake\nopen Lake DSL\npackage demo_broken\nlean_lib Good\nlean_lib Broken\n",
    );
    project.write("Good.lean", "def goodValue : Nat := 41\n");
    project.write("Broken.lean", "theorem broken : True := sorry_that_doesnt_exist\n");
    project.lake_build_ok("Good");
    let broken_build = project.lake_build("Broken:shared");
    assert!(
        !broken_build.status.success(),
        "`lake build Broken:shared` should fail for the broken fixture"
    );

    let host = LeanHost::from_lake_project(runtime(), project.path()).expect("host opens temp project");
    let caps = host.load_shims_only().expect("shim-only capabilities load");
    let mut good_session = caps.session(&["Good"], None, None).expect("unrelated module imports");
    let kind = good_session
        .declaration_kind("goodValue", None)
        .expect("declaration query works in unrelated module");
    assert_eq!(kind, "definition");

    let Err(broken_err) = caps.session(&["Broken"], None, None) else {
        panic!("broken module import unexpectedly succeeded");
    };
    match broken_err {
        LeanError::LeanException(_) => {}
        LeanError::Host(failure) => panic!("expected LeanException for broken import, got Host {failure:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected LeanException for broken import, got {cancelled:?}"),
    }
}

#[test]
fn import_finds_transitive_lake_package_oleans() {
    let project = TempLakeProject::new("transitive-oleans");
    write_transitive_dependency_fixture(&project);

    let host = LeanHost::from_lake_project(runtime(), project.path()).expect("host opens temp project");
    let caps = host.load_shims_only().expect("shim-only capabilities load");
    let mut session = caps
        .session(&["Consumer"], None, None)
        .expect("consumer imports dependency");
    let declarations = session
        .list_declarations_strings(&LeanDeclarationFilter::default(), None, None)
        .expect("list declarations works");
    assert!(
        declarations.iter().any(|name| name.starts_with("Dep.")),
        "dependency declaration should be visible when transitive oleans are on the search path"
    );

    let Err(missing_err) = caps.session(&["Dep.NonExistent"], None, None) else {
        panic!("missing dependency module unexpectedly imported");
    };
    match missing_err {
        LeanError::LeanException(exc) => {
            assert!(
                exc.message().contains("Dep.NonExistent"),
                "missing module should be reported by Lean import, got: {}",
                exc.message(),
            );
        }
        LeanError::Host(failure) => panic!("expected LeanException for missing import, got Host {failure:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected LeanException for missing import, got {cancelled:?}"),
    }
}

// -- session import + query ---------------------------------------------

fn session_over_handles<'lean, 'c>(caps: &'c crate::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.Handles"], None, None)
        .expect("session imports cleanly")
}

#[test]
fn session_import_then_query_fixture_definition() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    // `LeanRsFixture.Handles.nameAnonymous` is the first fixture export
    // in Handles.lean and is reachable through the imported environment.
    let decl = session
        .query_declaration("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("query existing fixture declaration");
    // Returned LeanDeclaration is opaque; the test passes if no error
    // surfaced. Render-checks happen via declaration_name.
    drop(decl);
}

#[test]
fn session_declaration_kind_discriminates() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let fixture_def_kind = session
        .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("kind for fixture def");
    assert_eq!(
        fixture_def_kind, "definition",
        "fixture `def` must classify as definition"
    );

    let nat_kind = session.declaration_kind("Nat", None).expect("kind for Nat");
    assert_eq!(nat_kind, "inductive", "prelude `Nat` must classify as inductive");

    let missing_kind = session
        .declaration_kind("This.Name.Does.Not.Exist", None)
        .expect("kind query for absent name");
    assert_eq!(missing_kind, "missing", "absent name must classify as missing");
}

#[test]
fn session_declaration_type_round_trips_as_expr() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let type_handle = session
        .declaration_type("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("type query for fixture def")
        .expect("fixture def has a type");
    // Returned LeanExpr is opaque; passing it through any fixture export
    // that accepts LeanExpr would prove structural soundness. Here we
    // just confirm the handle exists and drops without panic.
    drop(type_handle);
}

#[test]
fn session_import_profiles_pass_full_host_gates() {
    use crate::host::process::{
        ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult, ModuleQueryOutputBudgets,
        ModuleQuerySelector, ProofStateResult,
    };

    const IMPORTS: &[&str] = &["LeanRsFixture.Handles", "LeanRsHostShims.Elaboration"];
    const DECL: &str = "LeanRsFixture.Handles.nameAnonymous";
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");

    for profile in [
        LeanSessionImportProfile::ExportedPublic,
        LeanSessionImportProfile::Server,
    ] {
        let err = match caps.session_with_profile(IMPORTS, profile, None, None) {
            Ok(_) => panic!("{} should report the non-module fixture blocker", profile.label()),
            Err(err) => err,
        };
        let message = format!("{err:?}");
        assert!(
            message.contains("cannot import non-`module`"),
            "{} must report the Lean module-system blocker, got {message}",
            profile.label(),
        );
    }

    for profile in [
        LeanSessionImportProfile::Private,
        LeanSessionImportProfile::FullPrivateCompat,
    ] {
        let mut session = caps
            .session_with_profile(IMPORTS, profile, None, None)
            .unwrap_or_else(|err| panic!("{} session imports cleanly: {err:?}", profile.label()));
        let stats = session.import_stats().clone();
        assert_eq!(stats.import_all, profile.import_all(), "{} importAll", profile.label());
        assert_eq!(
            stats.import_level,
            profile.import_level().as_str(),
            "{} import level",
            profile.label()
        );
        assert!(stats.load_exts, "{} loadExts", profile.label());

        drop(
            session
                .query_declaration(DECL, None)
                .unwrap_or_else(|err| panic!("{} declaration lookup succeeds: {err:?}", profile.label())),
        );

        let range = session
            .declaration_source_range(DECL, None)
            .unwrap_or_else(|err| panic!("{} source range query succeeds: {err:?}", profile.label()))
            .unwrap_or_else(|| panic!("{} source range exists for {DECL}", profile.label()));
        assert!(
            range.end_line >= range.start_line,
            "{} source range is ordered",
            profile.label()
        );

        match session
            .inspect_declaration(&DeclarationInspectionRequest::new(DECL), None)
            .unwrap_or_else(|err| panic!("{} inspection succeeds: {err:?}", profile.label()))
        {
            DeclarationInspectionResult::Found { declaration } => {
                let statement = declaration
                    .statement
                    .as_ref()
                    .unwrap_or_else(|| panic!("{} inspection includes a statement", profile.label()));
                assert!(
                    !statement.value.is_empty(),
                    "{} pretty statement is non-empty",
                    profile.label()
                );
            }
            other => panic!("{} expected found inspection, got {other:?}", profile.label()),
        }

        let elaborated = session
            .elaborate("(1 + 2 : Nat)", None, &LeanElabOptions::new(), None)
            .unwrap_or_else(|err| panic!("{} elaboration dispatch succeeds: {err:?}", profile.label()));
        assert!(
            elaborated.is_ok(),
            "{} elaboration succeeds: {elaborated:?}",
            profile.label()
        );

        let outcome = session
            .process_module_query_batch(
                "theorem t (h : True) : True := by\n  exact h\n",
                &[ModuleQuerySelector::ProofState {
                    id: "state".to_owned(),
                    line: 2,
                    column: 4,
                }],
                &ModuleQueryOutputBudgets::default(),
                &LeanElabOptions::new(),
                None,
            )
            .unwrap_or_else(|err| panic!("{} proof-state dispatch succeeds: {err:?}", profile.label()));
        let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
            panic!("{} expected proof-state Ok outcome, got {outcome:?}", profile.label());
        };
        let Some(ModuleQueryBatchItem::Ok { result, .. }) = result.items.first() else {
            panic!("{} expected first proof-state item to be Ok", profile.label());
        };
        let ModuleQueryBatchResult::ProofState(ProofStateResult::State(info)) = result.as_ref() else {
            panic!("{} expected proof-state result, got {result:?}", profile.label());
        };
        assert!(
            info.locals.iter().any(|local| local.name == "h"),
            "{} proof-state locals include h",
            profile.label()
        );
    }
}

#[test]
fn bracketed_import_query_returns_serialized_declaration_metadata() {
    const IMPORTS: &[&str] = &["LeanRsFixture.Handles"];
    const DECL: &str = "LeanRsFixture.Handles.nameAnonymous";
    const MISSING: &str = "LeanRsFixture.Handles.noSuchDeclaration";
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");

    let result = caps
        .bracketed_import_query(IMPORTS, LeanBracketedImportRequest::new([DECL, MISSING]), None)
        .expect("bracketed import query succeeds");

    assert!(result.free_regions_ran, "bracketed query reports freeRegions ran");
    assert_eq!(result.import_stats.direct_import_names, IMPORTS);
    assert_eq!(result.import_stats.import_level, "private");
    assert!(!result.import_stats.import_all);
    assert!(!result.import_stats.load_exts);
    assert!(
        result.import_stats.imported_bytes > 0,
        "bracketed import reports imported bytes"
    );
    assert!(
        result.import_stats.compacted_region_count > 0,
        "bracketed import reports compacted regions"
    );

    let found = result
        .declarations
        .iter()
        .find(|info| info.name == DECL)
        .expect("found declaration row");
    assert!(found.exists);
    assert_eq!(found.kind.as_deref(), Some("definition"));
    assert_eq!(found.module.as_deref(), Some("LeanRsFixture.Handles"));
    assert!(
        found.raw_type.as_ref().is_some_and(|raw| !raw.is_empty()),
        "found declaration has raw type text"
    );

    let missing = result
        .declarations
        .iter()
        .find(|info| info.name == MISSING)
        .expect("missing declaration row");
    assert!(!missing.exists);
    assert_eq!(missing.kind, None);
    assert_eq!(missing.module, None);
    assert_eq!(missing.raw_type, None);
}

#[test]
fn bracketed_import_query_result_surface_cannot_carry_lean_handles() {
    fn assert_owned<T: Send + Sync + 'static>() {}

    assert_owned::<LeanBracketedImportRequest>();
    assert_owned::<LeanBracketedImportResult>();
}

#[test]
fn bracketed_import_query_reports_rejected_full_session_operations() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let result = caps
        .bracketed_import_query(
            &["LeanRsFixture.Handles"],
            LeanBracketedImportRequest::new(["LeanRsFixture.Handles.nameAnonymous"]),
            None,
        )
        .expect("bracketed import query succeeds");

    for operation in [
        "elaboration",
        "source-ranges",
        "proof-state",
        "pretty-printing",
        "capability-session",
    ] {
        let rejected = result
            .rejected_operations
            .iter()
            .find(|rejected| rejected.operation == operation)
            .unwrap_or_else(|| panic!("expected rejection for {operation}"));
        assert!(
            rejected.reason.contains("requires") || rejected.reason.contains("loaded"),
            "{operation} rejection explains the full-session dependency"
        );
    }
}

#[test]
fn session_declaration_type_returns_none_for_missing() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let absent = session
        .declaration_type("This.Name.Does.Not.Exist", None)
        .expect("type query for absent name");
    assert!(absent.is_none(), "missing name must yield None");
}

#[test]
fn session_declaration_name_renders_dotted_form() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let rendered = session
        .declaration_name("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("render name");
    assert!(
        rendered.contains("nameAnonymous"),
        "rendered name must contain the leaf component, got {rendered:?}",
    );
}

#[test]
fn session_query_missing_declaration_is_host_error() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let err = session
        .query_declaration("This.Name.Does.Not.Exist", None)
        .expect_err("missing declaration must surface a host error");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Conversion);
            assert!(
                failure.message().contains("This.Name.Does.Not.Exist"),
                "diagnostic must name the missing declaration, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Conversion) failure, got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected Host(Conversion) failure, got cancellation {cancelled:?}"),
    }
}

#[test]
fn session_list_declarations_includes_prelude_and_fixture() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let names = session.list_declarations(None).expect("list declarations");
    // The Lean prelude alone contributes thousands; the fixture import
    // is a thin slice on top. Just assert the result is non-empty.
    assert!(
        !names.is_empty(),
        "imported environment must contain at least one declaration"
    );
}

#[test]
fn session_name_to_string_renders_prelude_name() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let names = session.list_declarations(None).expect("list declarations");
    let mut found_nat = false;
    let mut found_fixture = false;
    for name in &names {
        let rendered = session.name_to_string(name, None).expect("render name");
        if rendered == "Nat" {
            found_nat = true;
        }
        if rendered == "LeanRsFixture.Handles.nameAnonymous" {
            found_fixture = true;
        }
        if found_nat && found_fixture {
            break;
        }
    }
    assert!(found_nat, "prelude `Nat` must round-trip through name_to_string");
    assert!(
        found_fixture,
        "fixture `LeanRsFixture.Handles.nameAnonymous` must round-trip through name_to_string"
    );
}

#[test]
fn session_name_to_string_renders_names_with_numeric_components() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    // Generated names (compiler-introduced numeric components) are
    // suppressed by the default filter; flip the flag on so the listing
    // contains them.
    let filter = LeanDeclarationFilter {
        include_generated: true,
        ..LeanDeclarationFilter::default()
    };
    let names = session
        .list_declarations_filtered(&filter, None, None)
        .expect("list with generated names");
    let mut saw_numeric_component = false;
    for name in &names {
        let rendered = session.name_to_string(name, None).expect("render name");
        assert!(!rendered.is_empty(), "every rendered name must be non-empty");
        if rendered.split('.').any(|part| part.chars().all(|c| c.is_ascii_digit())) {
            saw_numeric_component = true;
        }
    }
    assert!(
        saw_numeric_component,
        "enabling include_generated must surface at least one numeric-component name"
    );
}

#[test]
fn session_name_to_string_bulk_renders_listed_declarations() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let mut names = session.list_declarations(None).expect("list declarations");
    let take = names.len().min(1000);
    assert!(take >= 100, "fixture + prelude must yield at least 100 names");
    names.truncate(take);

    let rendered = session.name_to_string_bulk(&names, None, None).expect("bulk render");
    assert_eq!(rendered.len(), take, "bulk render must preserve length");
    assert!(rendered.iter().all(|s| !s.is_empty()), "no rendered name may be empty");
    assert!(
        rendered.iter().all(|s| s != "missing"),
        "bulk render is a pure projection — `missing` is not a valid output"
    );
}

#[test]
fn session_list_declarations_strings_matches_filtered_count() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let filter = LeanDeclarationFilter::default();
    let names = session
        .list_declarations_filtered(&filter, None, None)
        .expect("list filtered");
    let rendered = session
        .list_declarations_strings(&filter, None, None)
        .expect("list strings");
    assert_eq!(
        rendered.len(),
        names.len(),
        "list_declarations_strings must agree on length with list_declarations_filtered"
    );
}

// -- elaborate + kernel_check -------------------------------------------

fn session_over_elaboration<'lean, 'c>(caps: &'c crate::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsHostShims.Elaboration"], None, None)
        .expect("session imports cleanly")
}

#[test]
fn elaborate_success_returns_expr() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .elaborate("(1 + 2 : Nat)", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let expr = outcome.expect("elaboration succeeds for a well-typed Nat term");
    // Returned LeanExpr is opaque; success path is asserted by Ok.
    drop(expr);
}

#[test]
fn session_expr_to_string_raw_renders_elaborated_expr() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let expr = session
        .elaborate("(Nat.succ 0 : Nat)", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception")
        .expect("`(Nat.succ 0 : Nat)` elaborates against the prelude");
    let rendered = session.expr_to_string_raw(&expr, None).expect("raw render");
    assert!(!rendered.is_empty(), "raw projection must be non-empty");
    assert!(
        rendered.contains("Nat.succ"),
        "raw projection of `Nat.succ 0` must mention `Nat.succ`, got {rendered:?}",
    );
}

#[test]
fn elaborate_syntax_failure_reports_diagnostic() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .elaborate("1 +", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let failure = outcome.expect_err("trailing operator must fail to parse");
    let first = failure
        .diagnostics()
        .first()
        .expect("parse failure must report at least one diagnostic");
    assert_eq!(
        first.severity(),
        LeanSeverity::Error,
        "parse failure diagnostic must be error-severity"
    );
}

#[test]
fn elaborate_type_failure_reports_position() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // Mixing `String` with arithmetic against `Nat` triggers an
    // elaborator type error that carries a position.
    let outcome = session
        .elaborate("(1 + \"hi\" : Nat)", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let failure = outcome.expect_err("type-mismatched term must fail to elaborate");
    let diag = failure
        .diagnostics()
        .first()
        .expect("type failure must report at least one diagnostic");
    assert_eq!(
        diag.severity(),
        LeanSeverity::Error,
        "first diagnostic must be error-severity"
    );
    let pos = diag.position().expect("elaborator attached a position");
    assert!(
        pos.line() >= 1 && pos.column() >= 1,
        "position is 1-indexed: line={} column={}",
        pos.line(),
        pos.column(),
    );
    assert!(
        diag.message().len() <= LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT,
        "single diagnostic must fit the per-message byte bound"
    );
}

#[test]
fn kernel_check_small_theorem_returns_evidence() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem lean_rs_smoke : 1 + 1 = 2 := rfl";
    let outcome = session
        .kernel_check(src, &LeanElabOptions::new(), None, None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        EvidenceStatus::Checked,
        "well-typed theorem must classify as Checked, got {outcome:?}"
    );
    match outcome {
        LeanKernelOutcome::Checked(evidence) => {
            let _cloned = evidence.clone();
            drop(evidence);
        }
        LeanKernelOutcome::Rejected(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Checked variant");
        }
    }
}

#[test]
fn kernel_check_rejects_bad_proof() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem lean_rs_bad : 1 = 2 := rfl";
    let outcome = session
        .kernel_check(src, &LeanElabOptions::new(), None, None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        EvidenceStatus::Rejected,
        "kernel must reject a false proof, got {outcome:?}"
    );
    match outcome {
        LeanKernelOutcome::Rejected(failure) => {
            assert!(
                !failure.diagnostics().is_empty(),
                "rejected proof must carry at least one diagnostic"
            );
        }
        LeanKernelOutcome::Checked(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Rejected variant");
        }
    }
}

#[test]
fn kernel_check_classifies_unavailable_or_rejected_on_pathological_input() {
    // `Lean.Elab.Frontend.process` usually turns malformed
    // source into error diagnostics in the `MessageLog` (the
    // shim's `Rejected` path), not an `IO`-level exception (the
    // shim's `Unavailable` path). The Unavailable branch fires only
    // when `process` itself raises through `IO`—for example on
    // resource exhaustion, internal panic, or runtime failure during
    // task scheduling. Driving any of those from user input alone is
    // not contract: a given Lean release can move the boundary
    // between which inputs surface as diagnostics versus exceptions.
    //
    // This test pins what the Rust mapping *guarantees*: a deeply
    // pathological input must classify as either `Rejected` or
    // `Unavailable`—never `Checked` and never `Unsupported` (those
    // would mean the shim treated broken input as a valid command).
    // It also confirms the four-tag `EvidenceStatus` discriminator is
    // wired correctly for the two failure branches the shim can pick
    // here. The Lean-side classification logic (which decides between
    // `Rejected` and `Unavailable`) is exercised by the fixture's own
    // tests, not by this Rust integration suite.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .kernel_check("theorem :=", &LeanElabOptions::new(), None, None)
        .expect("host stack reports no exception");
    assert!(
        matches!(outcome.status(), EvidenceStatus::Rejected | EvidenceStatus::Unavailable),
        "malformed source must classify as Rejected or Unavailable, got {outcome:?}"
    );
    match outcome {
        LeanKernelOutcome::Rejected(failure) | LeanKernelOutcome::Unavailable(failure) => {
            assert!(
                !failure.diagnostics().is_empty(),
                "failure outcome must carry at least one diagnostic"
            );
        }
        LeanKernelOutcome::Checked(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("malformed source must not classify as Checked or Unsupported");
        }
    }
}

#[test]
fn kernel_check_unsupported_on_non_declaration() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // `#check` is a command that elaborates cleanly but adds no
    // constant to the environment, so the classifier returns
    // `Unsupported` (no new theorem/definition).
    let outcome = session
        .kernel_check("#check Nat", &LeanElabOptions::new(), None, None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        EvidenceStatus::Unsupported,
        "non-declaration command must classify as Unsupported, got {outcome:?}"
    );
}

#[test]
fn check_evidence_revalidates_checked_evidence() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .kernel_check(
            "theorem lean_rs_recheck : 1 + 1 = 2 := rfl",
            &LeanElabOptions::new(),
            None,
            None,
        )
        .expect("host stack reports no exception");
    let evidence = match outcome {
        LeanKernelOutcome::Checked(evidence) => evidence,
        LeanKernelOutcome::Rejected(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Checked variant");
        }
    };

    // Round-trip the cloned handle: re-validation must read the
    // bumped refcount cleanly.
    let cloned = evidence.clone();
    let status = session
        .check_evidence(&cloned, None)
        .expect("re-validation routes through the host stack cleanly");
    assert_eq!(
        status,
        EvidenceStatus::Checked,
        "re-validating a fresh evidence handle against the same environment must succeed"
    );

    // Original handle also re-validates; addDecl does not consume it.
    let status_again = session
        .check_evidence(&evidence, None)
        .expect("re-validation is idempotent");
    assert_eq!(
        status_again,
        EvidenceStatus::Checked,
        "re-validation is idempotent against an unchanged environment"
    );
}

#[test]
fn summarize_evidence_exposes_declaration_name() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .kernel_check(
            "theorem lean_rs_summary : 1 + 1 = 2 := rfl",
            &LeanElabOptions::new(),
            None,
            None,
        )
        .expect("host stack reports no exception");
    let evidence = match outcome {
        LeanKernelOutcome::Checked(evidence) => evidence,
        LeanKernelOutcome::Rejected(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Checked variant");
        }
    };

    let summary = session
        .summarize_evidence(&evidence, None)
        .expect("summary routes through the host stack cleanly");
    assert_eq!(
        summary.declaration_name(),
        "lean_rs_summary",
        "summary must expose the declared name verbatim"
    );
    assert_eq!(summary.kind(), "theorem", "summary must classify the kind as `theorem`");
    let signature = summary.type_signature();
    // The Lean fixture renders types via the default `ToString Expr`
    // instance, which emits the elaborated `Eq.{...} Nat ...` form
    // rather than the surface `=` notation. Either spelling proves the
    // proposition crossed the boundary as text; check for both so the
    // assertion survives a future switch to a pretty-printer.
    assert!(
        signature.contains("Eq") || signature.contains('='),
        "type signature must mention equality on the proposition, got: {signature:?}",
    );
    assert!(
        signature.contains("Nat"),
        "type signature must mention the underlying `Nat` carrier, got: {signature:?}",
    );
    assert!(
        !signature.contains("rfl"),
        "type signature must not leak the proof term `rfl`, got: {signature:?}",
    );
    for field in [summary.declaration_name(), summary.kind(), summary.type_signature()] {
        assert!(
            field.len() <= LEAN_PROOF_SUMMARY_BYTE_LIMIT,
            "ProofSummary field exceeds the documented byte bound: {} bytes",
            field.len()
        );
    }
}

#[test]
fn diagnostic_byte_limit_truncates() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // Multiple unbound identifiers produce multiple diagnostics; a
    // single-byte budget cannot fit them all and must be reported as
    // truncated.
    let opts = LeanElabOptions::new().diagnostic_byte_limit(1);
    let outcome = session
        .elaborate("(foo + bar + baz : Nat)", None, &opts, None)
        .expect("host stack reports no exception");
    let failure = outcome.expect_err("unbound identifiers must fail to elaborate");
    assert!(
        failure.truncated(),
        "tiny diagnostic budget must surface as truncated; diagnostics returned = {}",
        failure.diagnostics().len(),
    );
}

// -- timing note: amortised import across many queries -------------------
//
// Informational only. This test prints the numbers and does not assert
// thresholds. Run with `cargo test session_reuse_amortises_import -- --nocapture`.

#[test]
#[ignore = "informational import-amortization timing test; too memory-heavy for the default suite"]
fn session_reuse_amortises_import() {
    // Re-importing the Lean prelude is multi-second per call; 4 queries
    // is plenty to make the amortisation observable without dragging
    // the suite into the multi-minute range.
    const QUERIES: usize = 4;
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");

    // (a) One session, many queries.
    let start_reuse = Instant::now();
    {
        let mut session = caps
            .session(&["LeanRsFixture.Handles"], None, None)
            .expect("session imports cleanly");
        for _ in 0..QUERIES {
            let kind = session
                .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
                .expect("query");
            assert_eq!(kind, "definition");
        }
    }
    let reuse_elapsed = start_reuse.elapsed();

    // (b) Fresh session per query.
    let start_per_query = Instant::now();
    for _ in 0..QUERIES {
        let mut session = caps
            .session(&["LeanRsFixture.Handles"], None, None)
            .expect("session imports cleanly");
        let kind = session
            .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
            .expect("query");
        assert_eq!(kind, "definition");
    }
    let per_query_elapsed = start_per_query.elapsed();

    println!(
        "session_reuse_amortises_import: \
         {QUERIES} queries reusing one session took {reuse_elapsed:?}; \
         re-importing per query took {per_query_elapsed:?}",
    );
}

// -- run_meta -----------------------------------------------------------
//
// Each test imports `LeanRsHostShims.Meta` (which also pulls in
// `LeanRsHostShims.Elaboration` via the dependency edge). The fixture
// dylib exports the optional meta symbols, so checked binding resolution
// finds them and `run_meta` dispatches through cached typed handles.

fn session_over_meta<'lean, 'c>(caps: &'c crate::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.Meta", "LeanRsHostShims.Meta"], None, None)
        .expect("session imports cleanly")
}

fn meta_expr<'lean>(session: &mut LeanSession<'lean, '_>, fixture: &str) -> lean_rs::LeanExpr<'lean> {
    let source = match fixture {
        "lean_rs_fixture_meta_expr_nat" => "Nat".to_owned(),
        "lean_rs_fixture_meta_expr_bool" => "Bool".to_owned(),
        "lean_rs_fixture_meta_expr_reducible_nat_alias" => "LeanRsFixture.Meta.ReducibleNatAlias".to_owned(),
        "lean_rs_fixture_meta_expr_irreducible_nat_alias" => "LeanRsFixture.Meta.IrreducibleNatAlias".to_owned(),
        other => panic!("unknown meta fixture expression export {other}"),
    };
    session
        .elaborate(&source, None, &LeanElabOptions::new(), None)
        .expect("fixture expression elaboration reports no exception")
        .expect("fixture expression elaborates")
}

fn assert_is_def_eq_response(response: &LeanMetaResponse<bool>, expected: bool) {
    assert_eq!(
        response.status(),
        MetaCallStatus::Ok,
        "isDefEq must return Ok({expected}), got {response:?}",
    );
    match response {
        LeanMetaResponse::Ok(actual) => assert_eq!(*actual, expected),
        LeanMetaResponse::Failed(_) | LeanMetaResponse::TimeoutOrHeartbeat(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected Ok({expected}) variant");
        }
    }
}

#[test]
fn meta_registry_exposes_five_pinned_services() {
    let services = [
        infer_type().name(),
        whnf().name(),
        heartbeat_burn().name(),
        is_def_eq().name(),
        pp_expr().name(),
    ];
    assert_eq!(
        services,
        [
            "lean_rs_host_meta_infer_type",
            "lean_rs_host_meta_whnf",
            "lean_rs_host_meta_heartbeat_burn",
            "lean_rs_host_meta_is_def_eq",
            "lean_rs_host_meta_pp_expr",
        ],
    );
    assert_eq!(
        is_def_eq().required_imports(),
        ["LeanRsHostShims.Meta"],
        "new service must use the existing meta shim module",
    );
}

#[test]
fn meta_infer_type_returns_ok_for_nat_type() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    // The type of `Nat.zero` is `Nat`; inferring its type yields `Type`.
    // Using a Lean-produced Expr keeps the test honest—Rust never
    // constructs an Expr directly.
    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query for Nat.zero")
        .expect("Nat.zero has a type");
    let outcome = session
        .run_meta(&infer_type(), expr, &LeanMetaOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Ok,
        "Meta.inferType on `Nat` must succeed, got {outcome:?}",
    );
    match outcome {
        LeanMetaResponse::Ok(payload) => {
            // Opaque LeanExpr; the success path is asserted by status().
            drop(payload);
        }
        LeanMetaResponse::Failed(_) | LeanMetaResponse::TimeoutOrHeartbeat(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected Ok variant");
        }
    }
}

#[test]
fn meta_whnf_returns_ok_for_nat_type() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query for Nat.zero")
        .expect("Nat.zero has a type");
    let outcome = session
        .run_meta(&whnf(), expr, &LeanMetaOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Ok,
        "Meta.whnf on a constant Expr must succeed, got {outcome:?}",
    );
    match outcome {
        LeanMetaResponse::Ok(payload) => drop(payload),
        LeanMetaResponse::Failed(_) | LeanMetaResponse::TimeoutOrHeartbeat(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected Ok variant");
        }
    }
}

#[test]
fn meta_heartbeat_burn_yields_timeout_status() {
    // timing note: with heartbeat_limit(1), Core.checkMaxHeartbeats
    // trips on the very first iteration; the test completes in well
    // under a millisecond. No threshold asserted (per the project's
    // "no performance claim without numbers" rule).
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    // Any Expr will do—heartbeat_burn ignores its argument.
    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query for Nat.zero")
        .expect("Nat.zero has a type");
    let opts = LeanMetaOptions::new().heartbeat_limit(1);
    let outcome = session
        .run_meta(&heartbeat_burn(), expr, &opts, None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::TimeoutOrHeartbeat,
        "heartbeat budget = 1 must surface as TimeoutOrHeartbeat, got {outcome:?}",
    );
    match outcome {
        LeanMetaResponse::TimeoutOrHeartbeat(failure) => {
            let first = failure
                .diagnostics()
                .first()
                .expect("heartbeat failure must carry at least one diagnostic");
            assert_eq!(first.severity(), LeanSeverity::Error);
            assert!(
                !first.message().is_empty(),
                "heartbeat diagnostic message must be non-empty",
            );
        }
        LeanMetaResponse::Ok(_) | LeanMetaResponse::Failed(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected TimeoutOrHeartbeat variant");
        }
    }
}

#[test]
fn meta_pp_expr_renders_elaborated_expr() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let expr = session
        .elaborate("(Nat.succ 0 : Nat)", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception")
        .expect("`(Nat.succ 0 : Nat)` elaborates against the prelude");
    let outcome = session
        .run_meta(&pp_expr(), expr, &LeanMetaOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Ok,
        "pp_expr on a well-typed Expr must succeed, got {outcome:?}",
    );
    match outcome {
        LeanMetaResponse::Ok(rendered) => {
            assert!(!rendered.is_empty(), "pretty-printed form must be non-empty");
            assert!(
                rendered.contains("Nat.succ"),
                "pretty-printed `Nat.succ 0` must mention `Nat.succ`, got {rendered:?}",
            );
        }
        LeanMetaResponse::Failed(_) | LeanMetaResponse::TimeoutOrHeartbeat(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected Ok variant");
        }
    }
}

#[test]
fn meta_pp_expr_honours_heartbeat_budget() {
    // pp_expr runs `Lean.PrettyPrinter.ppExpr` inside MetaM, which
    // consults `checkMaxHeartbeats` on every reduction step. With
    // `heartbeat_limit(1)` the first internal check trips and the
    // outcome must classify as `TimeoutOrHeartbeat`—never `Ok`,
    // never a panic.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let expr = session
        .elaborate("(Nat.succ 0 : Nat)", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception")
        .expect("`(Nat.succ 0 : Nat)` elaborates against the prelude");
    let opts = LeanMetaOptions::new().heartbeat_limit(1);
    let outcome = session
        .run_meta(&pp_expr(), expr, &opts, None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::TimeoutOrHeartbeat,
        "heartbeat budget = 1 must surface as TimeoutOrHeartbeat, got {outcome:?}",
    );
}

#[test]
fn meta_is_def_eq_reducible_alias_matches_nat() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_reducible_nat_alias");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Reducible),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, true);
}

#[test]
fn meta_is_def_eq_distinguishes_nat_and_bool() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_bool");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Reducible),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, false);
}

#[test]
fn meta_is_def_eq_default_does_not_unfold_irreducible_alias() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_irreducible_nat_alias");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Default),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, false);
}

#[test]
fn meta_is_def_eq_all_unfolds_irreducible_alias() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_irreducible_nat_alias");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::All),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, true);
}

#[test]
fn meta_is_def_eq_pre_cancelled_token_returns_cancelled() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let before = session.stats();
    let token = LeanCancellationToken::new();
    token.cancel();
    let err = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Reducible),
            &LeanMetaOptions::new(),
            Some(&token),
        )
        .expect_err("pre-cancelled token must return Cancelled");
    match err {
        LeanError::Cancelled(_) => {}
        LeanError::LeanException(exc) => panic!("expected Cancelled, got LeanException {exc:?}"),
        LeanError::Host(failure) => panic!("expected Cancelled, got Host {failure:?}"),
    }
    assert_eq!(
        session.stats().ffi_calls,
        before.ffi_calls,
        "pre-cancelled run_meta must not enter another FFI call",
    );
}

#[test]
fn meta_missing_optional_symbol_returns_unsupported() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let expr = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let missing: LeanMetaService<lean_rs::LeanExpr<'_>, lean_rs::LeanExpr<'_>> =
        LeanMetaService::new("lean_rs_host_meta_missing_for_test", &["LeanRsHostShims.Meta"]);
    let outcome = session
        .run_meta(&missing, expr, &LeanMetaOptions::new(), None)
        .expect("missing optional service is classified, not a load failure");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Unsupported,
        "missing optional meta symbol must return Unsupported, got {outcome:?}",
    );
}

// -- process_module_query (bounded module projections) -------------------

#[test]
fn session_process_module_query_returns_diagnostics_without_info_tree_payload() {
    use crate::host::process::{ModuleQuery, ModuleQueryOutcome, ModuleQueryResult};

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "import Lean\n\ntheorem bad : True := 0\n";
    let outcome = session
        .process_module_query(src, &ModuleQuery::Diagnostics, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");

    let ModuleQueryOutcome::Ok {
        result: ModuleQueryResult::Diagnostics(diagnostics),
        imports,
    } = outcome
    else {
        panic!("expected diagnostics Ok outcome, got {outcome:?}");
    };
    assert_eq!(imports, vec!["Lean".to_string()]);
    assert!(
        diagnostics
            .diagnostics()
            .iter()
            .any(|d| d.severity() == LeanSeverity::Error),
        "type-mismatched body must record at least one error diagnostic, got {:?}",
        diagnostics.diagnostics(),
    );
}

#[test]
fn session_process_module_query_type_at_returns_one_selected_term() {
    use crate::host::process::{ModuleQuery, ModuleQueryOutcome, ModuleQueryResult, TypeAtResult};

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "def x := 1\ntheorem t : x = 1 := by rfl\n";
    let outcome = session
        .process_module_query(
            src,
            &ModuleQuery::TypeAt { line: 2, column: 13 },
            &LeanElabOptions::new(),
            None,
        )
        .expect("host stack reports no exception");

    let ModuleQueryOutcome::Ok {
        result:
            ModuleQueryResult::TypeAt(TypeAtResult::Term {
                span,
                expr,
                type_str,
                expected_type: _,
            }),
        imports,
    } = outcome
    else {
        panic!("expected selected term, got {outcome:?}");
    };
    assert!(
        imports.is_empty(),
        "body-only input should have no imports, got {imports:?}"
    );
    assert_eq!(span.start_line, 2);
    assert!(!expr.value.is_empty(), "selected term must render its expression");
    assert!(
        !type_str.value.is_empty(),
        "selected term must render its inferred type"
    );
    assert!(
        expr.value.len() <= 64 * 1024 && type_str.value.len() <= 64 * 1024,
        "type query fields must be bounded, got expr={} type={}",
        expr.value.len(),
        type_str.value.len(),
    );
}

#[test]
fn session_process_module_query_goal_at_returns_selected_tactic_goals() {
    use crate::host::process::{GoalAtResult, ModuleQuery, ModuleQueryOutcome, ModuleQueryResult};

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem t : True := by\n  trivial\n";
    let outcome = session
        .process_module_query(
            src,
            &ModuleQuery::GoalAt { line: 2, column: 4 },
            &LeanElabOptions::new().diagnostic_byte_limit(256),
            None,
        )
        .expect("host stack reports no exception");

    let ModuleQueryOutcome::Ok {
        result:
            ModuleQueryResult::GoalAt(GoalAtResult::Goal {
                span,
                goals_before,
                goals_after: _,
                truncated: _,
            }),
        imports,
    } = outcome
    else {
        panic!("expected selected tactic goals, got {outcome:?}");
    };
    assert!(
        imports.is_empty(),
        "body-only input should have no imports, got {imports:?}"
    );
    assert_eq!(span.start_line, 2);
    assert!(
        goals_before.iter().any(|goal| goal.contains("True")),
        "goal before `trivial` should mention True, got {goals_before:?}",
    );
}

#[test]
fn session_process_module_query_batch_returns_proof_context_in_one_dispatch() {
    use crate::host::process::{
        ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult, ModuleQueryOutputBudgets,
        ModuleQuerySelector, ProofStateResult,
    };

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem t (h : True) : True := by\n  exact h\n";
    let before = session.stats();
    let outcome = session
        .process_module_query_batch(
            src,
            &[
                ModuleQuerySelector::Diagnostics {
                    id: "diagnostics".to_owned(),
                },
                ModuleQuerySelector::ProofState {
                    id: "state".to_owned(),
                    line: 2,
                    column: 4,
                },
            ],
            &ModuleQueryOutputBudgets::default(),
            &LeanElabOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    let after = session.stats();

    assert_eq!(after.ffi_calls, before.ffi_calls + 1);
    assert_eq!(after.batch_items, before.batch_items + 2);

    let ModuleQueryBatchOutcome::Ok { result, imports } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    assert!(imports.is_empty(), "body-only input should have no imports");
    assert_eq!(result.items.len(), 2);
    assert!(!result.total_truncated);

    let state = result
        .items
        .iter()
        .find(|item| item.id() == "state")
        .expect("proof-state selector result present");
    match state {
        ModuleQueryBatchItem::Ok { result, .. } => match result.as_ref() {
            ModuleQueryBatchResult::ProofState(ProofStateResult::State(info)) => {
                assert!(
                    info.goals_before.iter().any(|goal| goal.contains("True")),
                    "goal before `exact h` should mention True, got {:?}",
                    info.goals_before,
                );
                assert!(
                    info.locals.iter().any(|local| local.name == "h"),
                    "local context should include h, got {:?}",
                    info.locals,
                );
                assert!(
                    info.safe_edit.is_some(),
                    "proof context should identify a replace-body span"
                );
            }
            other => panic!("expected proof-state result, got {other:?}"),
        },
        other => panic!("expected ok proof-state selector, got {other:?}"),
    }
}

#[test]
fn session_process_module_query_references_returns_name_locations_only() {
    use crate::host::process::{ModuleQuery, ModuleQueryOutcome, ModuleQueryResult};

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "def x := 1\ntheorem t : x = 1 := by rfl\n#check x\n";
    let outcome = session
        .process_module_query(
            src,
            &ModuleQuery::References { name: "x".to_string() },
            &LeanElabOptions::new(),
            None,
        )
        .expect("host stack reports no exception");

    let ModuleQueryOutcome::Ok {
        result: ModuleQueryResult::References(result),
        imports,
    } = outcome
    else {
        panic!("expected references result, got {outcome:?}");
    };
    assert!(
        imports.is_empty(),
        "body-only input should have no imports, got {imports:?}"
    );
    assert!(!result.references.is_empty(), "expected at least one `x` reference");
    assert!(
        result.references.iter().all(|r| r.name.ends_with('x')),
        "references query should return only matching names, got {:?}",
        result.references,
    );
}

#[test]
fn session_process_module_query_reports_missing_imports_with_result() {
    use crate::host::process::{ModuleQuery, ModuleQueryOutcome, ModuleQueryResult};

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "import Foo.Bar.Baz\n\ndef x := 1\n";
    let outcome = session
        .process_module_query(src, &ModuleQuery::Diagnostics, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let ModuleQueryOutcome::MissingImports {
        result: ModuleQueryResult::Diagnostics(_),
        imports,
        missing,
    } = outcome
    else {
        panic!("expected MissingImports with diagnostics, got {outcome:?}");
    };
    assert_eq!(imports, vec!["Foo.Bar.Baz".to_string()]);
    assert_eq!(missing, vec!["Foo.Bar.Baz".to_string()]);
}

#[test]
fn session_process_module_query_surfaces_header_parse_error() {
    use crate::host::process::{ModuleQuery, ModuleQueryOutcome};

    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "import 123\n\ntheorem t : True := by trivial\n";
    let outcome = session
        .process_module_query(src, &ModuleQuery::Diagnostics, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let ModuleQueryOutcome::HeaderParseFailed { diagnostics } = outcome else {
        panic!("expected HeaderParseFailed, got {outcome:?}");
    };
    assert!(
        diagnostics
            .diagnostics()
            .iter()
            .any(|d| d.severity() == LeanSeverity::Error),
        "malformed header must record at least one error diagnostic, got {:?}",
        diagnostics.diagnostics(),
    );
}

#[test]
fn session_process_module_query_handles_module_system_header() {
    use crate::host::process::{ModuleQuery, ModuleQueryOutcome, ModuleQueryResult};

    let project = TempLakeProject::new("module-query-header-host");
    write_module_syntax_fixture(&project);
    let host = LeanHost::from_lake_project(runtime(), project.path()).expect("host opens temp project");
    let caps = host.load_shims_only().expect("shim-only capabilities load");
    let mut session = caps
        .session(
            &["Fixture.Imported", "Fixture.Internal", "Fixture.PrivateScope"],
            None,
            None,
        )
        .expect("session imports module-system fixture dependencies");

    let src = "\
module

public import Fixture.Imported
import all Fixture.Internal
import Fixture.PrivateScope

def moduleSyntaxFoo : Nat := imported + internalSecret
#check privateOnly
";
    let outcome = session
        .process_module_query(src, &ModuleQuery::Diagnostics, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let ModuleQueryOutcome::Ok {
        result: ModuleQueryResult::Diagnostics(diagnostics),
        imports,
    } = outcome
    else {
        panic!("expected Ok diagnostics, got {outcome:?}");
    };

    assert_eq!(
        imports,
        vec![
            "Fixture.Imported".to_string(),
            "Fixture.Internal".to_string(),
            "Fixture.PrivateScope".to_string(),
        ],
        "imports must be bare module names, without `public` or `all` modifiers",
    );
    let diagnostics = diagnostics.diagnostics();
    let messages: Vec<&str> = diagnostics.iter().map(|diagnostic| diagnostic.message()).collect();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.severity() == LeanSeverity::Error && d.message().contains("privateOnly")),
        "ordinary imports under `module` must not expose private declarations, got {diagnostics:?}",
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.severity() == LeanSeverity::Error && d.message().contains("internalSecret")),
        "`import all` under `module` must expose private declarations from the imported module, got {diagnostics:?}",
    );
    assert!(
        !messages.iter().any(|m| m.contains("unknown module prefix 'all'")),
        "`import all` must resolve the named module, got {messages:?}",
    );
}
