//! End-to-end tests for the `LeanHost` / `LeanCapabilities` /
//! `LeanSession` cascade.
//!
//! Each test bootstraps the runtime, opens the fixture Lake project,
//! loads the `LeanRsFixture` capability dylib plus the checked bundled host
//! shim capability, starts a session over an import list, and
//! exercises the typed query methods.

#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

#[path = "support/fixed_fixture.rs"]
mod fixture;

#[path = "support/service_requests.rs"]
mod service_requests;

#[path = "support/transitive_assertions.rs"]
mod transitive_assertions;

#[path = "support/good_shims_assertions.rs"]
mod good_shims_assertions;

// The marked regression calls fix their sources, selectors and options at
// the call site. All other calls still need the independent policy record.
macro_rules! service_paths {
    ($caps:ident, $session:ident, $operation:literal, $payload:expr, $options:ident) => {
        service_requests::approved_paths(&$caps, &$session, $operation, $payload, &$options)
    };
    ($caps:ident, $session:ident, $operation:literal, $payload:expr, $options:ident, option_regression) => {
        if fixture::native_fixture_selected() {
            fixture::native_elaboration_paths(&$caps, &$session)
        } else {
            service_paths!($caps, $session, $operation, $payload, $options)
        }
    };
    ($caps:ident, $session:ident, $operation:literal, $payload:expr, $options:ident, header_accounting) => {
        if fixture::native_fixture_selected() {
            fixture::HeldHostImport::HeaderAccounting.source_paths(&$caps, &$session)
        } else {
            service_paths!($caps, $session, $operation, $payload, $options)
        }
    };
    ($caps:ident, $session:ident, $operation:literal, $payload:expr, $options:ident, full_profile($profile:ident)) => {
        if fixture::native_fixture_selected() {
            fixture::HeldHostImport::FullProfiles($profile).source_paths(&$caps, &$session)
        } else {
            service_paths!($caps, $session, $operation, $payload, $options)
        }
    };
}

// Capture exact typed requests before taking the mutable session borrow. The policy
// encoding uses derived Debug for closed SDK request enums (including every field).
macro_rules! call_service {
    ($caps:ident, $session:ident, kernel_check,
        $source:expr, $options:expr, $cancel:expr, $progress:expr $(; $policy_kind:ident)? $(,)?) => {{
        let request: String = ($source).to_owned();
        let options = ($options).source_options();
        let paths =
            service_paths!($caps, $session, "kernel_check", &format!("{request:?}"), options $(, $policy_kind)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .kernel_scope(env, scope, paths, request, options)
        }
        .expect("admit reviewed request");
        $session.kernel_check(&admitted, $cancel, $progress)
    }};
    ($caps:ident, $session:ident, elaborate,
        $source:expr, None, $options:expr, $cancel:expr $(; $policy_kind:ident $(($profile:ident))?)? $(,)?) => {{
        let request: String = ($source).to_owned();
        let options = ($options).source_options();
        let paths = service_paths!($caps, $session, "elaborate",
            &format!("{request:?}"), options $(, $policy_kind $(($profile))?)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .term_scope(env, scope, paths, request, None, options)
        }
        .expect("admit reviewed request");
        $session.elaborate(&admitted, $cancel)
    }};
    ($caps:ident, $session:ident, process_module_query,
        $source:expr, $query:expr, $options:expr, $cancel:expr $(; $policy_kind:ident)? $(,)?) => {{
        let request = lean_rs::host_bridge::HostModuleQuery {
            source: ($source).to_owned(),
            query: ($query).clone(),
        };
        let options = ($options).source_options();
        let paths = service_paths!($caps, $session, "process_module_query",
            &format!("{:?}", (&request.source, &request.query)), options $(, $policy_kind)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .module_query_scope(env, scope, paths, request, options)
        }
        .expect("admit reviewed request");
        $session.process_module_query(&admitted, $cancel)
    }};
    ($caps:ident, $session:ident, process_module_query_batch,
        $source:expr, $selectors:expr, $budgets:expr,
        $options:expr, $cancel:expr $(; $policy_kind:ident $(($profile:ident))?)? $(,)?) => {{
        let batch = lean_rs::host_bridge::HostModuleBatch {
            source: ($source).to_owned(),
            selectors: ($selectors).to_vec(),
            budgets: ($budgets).clone(),
        };
        let request = batch;
        let options = ($options).source_options();
        let paths = service_paths!($caps, $session, "process_module_query_batch",
            &format!("{:?}", (&request.source, &request.selectors, &request.budgets)),
            options $(, $policy_kind $(($profile))?)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .module_batch_scope(env, scope, paths, request, options)
        }
        .expect("admit reviewed request");
        $session.process_module_query_batch(&admitted, $cancel)
    }};
    ($caps:ident, $session:ident, process_module_query_batch_cached,
        $source:expr, $selectors:expr, $budgets:expr,
        $options:expr, $policy:expr, $cancel:expr $(; $policy_kind:ident)? $(,)?) => {{
        let batch = lean_rs::host_bridge::HostModuleBatch {
            source: ($source).to_owned(),
            selectors: ($selectors).to_vec(),
            budgets: ($budgets).clone(),
        };
        let p = $policy;
        let request = lean_rs::host_bridge::HostCachedModuleBatch {
            batch,
            policy: format!(
                "{}\n{}\n{}\n{}\n{}",
                p.file_identity, p.key, p.max_entries, p.ttl_millis, p.max_bytes
            ),
        };
        let options = ($options).source_options();
        let paths = service_paths!($caps, $session, "process_module_query_batch_cached",
            &format!(
                "{:?}",
                (
                    &request.batch.source,
                    &request.batch.selectors,
                    &request.batch.budgets,
                    &request.policy
                )
            ), options $(, $policy_kind)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .cached_module_batch_scope(env, scope, paths, request, options)
        }
        .expect("admit reviewed request");
        $session.process_module_query_batch_cached(&admitted, $cancel)
    }};
    ($caps:ident, $session:ident, verify_declaration_batch,
        $input:expr, $options:expr, $cancel:expr $(; $policy_kind:ident)? $(,)?) => {{
        let request = ($input).clone();
        let options = ($options).source_options();
        let paths = service_paths!($caps, $session, "verify_declaration_batch",
            &format!("{request:?}"), options $(, $policy_kind)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .verification_batch_scope(env, scope, paths, request, options)
        }
        .expect("admit reviewed request");
        $session.verify_declaration_batch(&admitted, $cancel)
    }};
    ($caps:ident, $session:ident, verify_declaration,
        $input:expr, $options:expr, $cancel:expr $(; $policy_kind:ident)? $(,)?) => {{
        let request = ($input).clone();
        let options = ($options).source_options();
        let paths = service_paths!($caps, $session, "verify_declaration",
            &format!("{request:?}"), options $(, $policy_kind)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .verification_scope(env, scope, paths, request, options)
        }
        .expect("admit reviewed request");
        $session.verify_declaration(&admitted, $cancel)
    }};
    ($caps:ident, $session:ident, attempt_proof,
        $input:expr, $options:expr, $cancel:expr $(; $policy_kind:ident)? $(,)?) => {{
        let request = ($input).clone();
        let options = ($options).source_options();
        let paths =
            service_paths!($caps, $session, "attempt_proof", &format!("{request:?}"), options $(, $policy_kind)?);
        let (env, scope) = $session.source_context();
        #[allow(
            unsafe_code,
            reason = "exact typed request checked against the independent fixture policy"
        )]
        // SAFETY: the reviewed payload and options cover this operation in this
        // admitted graph. The token binds the same environment, capability and retained images.
        let admitted = unsafe {
            $caps
                .shim_capability()
                .proof_attempt_scope(env, scope, paths, request, options)
        }
        .expect("admit reviewed request");
        $session.attempt_proof(&admitted, $cancel)
    }};
}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use lean_rs::LeanRuntime;
use lean_rs::error::{HostStage, LeanError};
use lean_toolchain::LEAN_VERSION;

use lean_rs_host::host::meta::{
    LeanMetaOptions, LeanMetaResponse, LeanMetaTransparency, MetaCallStatus, heartbeat_burn, infer_type, is_def_eq,
    pp_expr, whnf,
};
use lean_rs_host::{
    DeclarationInspectionFields, DeclarationInspectionRequest, DeclarationInspectionResult, DeclarationSearchRequest,
    EvidenceStatus, LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_PROOF_SUMMARY_BYTE_LIMIT, LeanBracketedImportRequest,
    LeanBracketedImportResult, LeanCancellationToken, LeanDeclarationFilter, LeanElabOptions, LeanHost,
    LeanKernelOutcome, LeanSession, LeanSessionImportProfile, LeanSeverity,
};

// -- fixture setup -------------------------------------------------------

fn fixture_lake_root() -> PathBuf {
    fixture::fixture_root()
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
}

struct TempLakeProject {
    name: String,
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
        Self {
            name: name.to_owned(),
            root,
        }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    /// Only load the independently reviewed copy of this specific project. Compare
    /// sources, dependency descriptions and compiled modules with the fresh Lake build.
    fn reviewed_copy(&self) -> PathBuf {
        assert!(matches!(
            self.name.as_str(),
            "shims-only-good"
                | "shims-only-broken"
                | "transitive-oleans"
                | "module-query-header-host"
                | "cached-module-header-host"
        ));
        let approved = fixture::fixture_root().join("host-service-projects").join(&self.name);
        fn inputs(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
            let mut selected = std::collections::BTreeMap::new();
            let mut dirs = vec![root.to_owned()];
            while let Some(dir) = dirs.pop() {
                for entry in fs::read_dir(dir).expect("project directory") {
                    let entry = entry.expect("project entry");
                    let path = entry.path();
                    let kind = entry.file_type().expect("project file type");
                    assert!(!kind.is_symlink(), "project input is a symlink");
                    if kind.is_dir() {
                        dirs.push(path);
                    } else if matches!(
                        path.extension().and_then(|s| s.to_str()),
                        Some("lean" | "olean" | "ilean" | "ir" | "toml")
                    ) || path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|name| name.ends_with(".olean.private") || name.ends_with(".olean.server"))
                        || matches!(
                            path.file_name().and_then(|s| s.to_str()),
                            Some("lake-manifest.json" | "lean-toolchain")
                        )
                    {
                        selected.insert(
                            path.strip_prefix(root).expect("relative project input").to_owned(),
                            fs::read(path).expect("project input"),
                        );
                    }
                }
            }
            selected
        }
        assert!(
            inputs(self.path()) == inputs(&approved),
            "fresh and reviewed project inputs differ"
        );
        approved
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("capability dylib loads + symbols resolve");
    // Sanity: caps is move-constructed, no public observable state to
    // assert against. The follow-on tests prove checked shim bindings
    // actually dispatch correctly.
    drop(caps);
}

#[test]
fn load_capabilities_missing_dylib_is_load_error() {
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let missing = host.project_library_path("does_not_exist", "NoSuchLib");
    assert!(!missing.exists());
    #[allow(unsafe_code, reason = "absent path under retained fixture custody")]
    // SAFETY: this missing fixture image cannot be created by a cooperating writer during the held loan.
    let err = unsafe { lean_rs::module::LeanLibrary::open(runtime(), missing) }.expect_err("missing dylib must fail");
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

    let host = LeanHost::from_lake_project(runtime(), project.reviewed_copy()).expect("host opens temp project");
    let caps = fixture::shims_only(&host).expect("shim-only capabilities load");
    let imports = service_requests::imports(
        &caps,
        good_shims_assertions::IMPORTS,
        LeanSessionImportProfile::default(),
    );
    good_shims_assertions::check(&caps, &imports, |session| {
        call_service!(
            caps,
            session,
            kernel_check,
            good_shims_assertions::KERNEL_SOURCE,
            &LeanElabOptions::new(),
            None,
            None,
        )
    });
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

    let host = LeanHost::from_lake_project(runtime(), project.reviewed_copy()).expect("host opens temp project");
    let caps = fixture::shims_only(&host).expect("shim-only capabilities load");
    good_shims_assertions::check_broken_shared(&caps, |name| {
        service_requests::imports(&caps, &[name], LeanSessionImportProfile::default())
    });
}

#[test]
fn import_finds_transitive_lake_package_oleans() {
    let project = TempLakeProject::new("transitive-oleans");
    write_transitive_dependency_fixture(&project);

    let host = LeanHost::from_lake_project(runtime(), project.reviewed_copy()).expect("host opens temp project");
    let caps = fixture::shims_only(&host).expect("shim-only capabilities load");
    transitive_assertions::check(&caps, |name| {
        service_requests::imports(&caps, &[name], LeanSessionImportProfile::default())
    });
}

// -- session import + query ---------------------------------------------

fn session_over_handles<'lean, 'c>(caps: &'c lean_rs_host::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(
        &service_requests::imports(caps, &["LeanRsFixture.Handles"], LeanSessionImportProfile::default()),
        None,
        None,
    )
    .expect("session imports cleanly")
}

fn native_handles_session<'lean, 'c>(caps: &'c lean_rs_host::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    if !fixture::native_fixture_selected() {
        return session_over_handles(caps);
    }
    caps.session(&fixture::native_imports(caps, "LeanRsFixture.Handles"), None, None)
        .expect("session imports cleanly")
}

#[test]
fn session_import_then_query_fixture_definition() {
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    use lean_rs_host::host::process::{
        ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult, ModuleQueryOutputBudgets,
        ModuleQuerySelector, ProofStateResult,
    };

    const IMPORTS: &[&str] = &["LeanRsFixture.Handles", "LeanRsHostShims.Elaboration"];
    const DECL: &str = "LeanRsFixture.Handles.nameAnonymous";
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let imports = |profile| {
        if fixture::native_fixture_selected() {
            fixture::HeldHostImport::FullProfiles(profile).imports(&caps)
        } else {
            service_requests::imports(&caps, IMPORTS, profile)
        }
    };

    for profile in [
        LeanSessionImportProfile::ExportedPublic,
        LeanSessionImportProfile::Server,
    ] {
        let Err(err) = caps.session_with_profile(&imports(profile), profile, None, None) else {
            panic!("{} should report the non-module fixture blocker", profile.label());
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
            .session_with_profile(&imports(profile), profile, None, None)
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

        let elaborated = call_service!(
            caps,
            session,
            elaborate,
            "(1 + 2 : Nat)",
            None,
            &LeanElabOptions::new(),
            None; full_profile(profile)
        )
        .unwrap_or_else(|err| panic!("{} elaboration dispatch succeeds: {err:?}", profile.label()));
        assert!(
            elaborated.is_ok(),
            "{} elaboration succeeds: {elaborated:?}",
            profile.label()
        );

        let outcome = call_service!(
            caps,
            session,
            process_module_query_batch,
            "theorem t (h : True) : True := by\n  exact h\n",
            &[ModuleQuerySelector::ProofState {
                id: "state".to_owned(),
                line: 2,
                column: 4,
            }],
            &ModuleQueryOutputBudgets::default(),
            &LeanElabOptions::new(),
            None; full_profile(profile)
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");

    let result = caps
        .bracketed_import_query(
            &service_requests::bracketed(&caps, IMPORTS),
            LeanBracketedImportRequest::new([DECL, MISSING]),
            None,
        )
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
    assert_eq!(
        result.import_stats.imported_bytes, result.import_stats.compacted_region_bytes,
        "bracketed imported bytes remains the compacted-region byte total"
    );
    assert_eq!(
        result.import_stats.compacted_region_bytes,
        result
            .import_stats
            .memory_mapped_region_bytes
            .saturating_add(result.import_stats.non_memory_mapped_region_bytes),
        "bracketed region bytes should split into mmap and non-mmap totals"
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
fn derived_cheap_inspection_skips_proof_search_indexes() {
    const DECL: &str = "LeanRsFixture.SourceRanges.documentedSimpTheorem";
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("fixture capability loads");
    let mut session = caps
        .session(
            &fixture_service_imports(&caps, "LeanRsFixture.SourceRanges"),
            None,
            None,
        )
        .expect("fixture session opens");

    let result = session
        .inspect_declaration(&DeclarationInspectionRequest::new(DECL), None)
        .expect("inspection succeeds");

    let DeclarationInspectionResult::Found { declaration } = result else {
        panic!("expected found inspection");
    };
    assert!(!declaration.proof_search.computed);
    assert!(declaration.proof_search.unavailable_reason.is_some());
    assert_eq!(declaration.derived_work.proof_search_fact_collections, 0);
    assert_eq!(declaration.derived_work.simp_extension_lookups, 0);
    assert!(
        !declaration.attributes.iter().any(|attr| attr == "simp"),
        "simp is proof-search-derived and must not be reported by default: {:?}",
        declaration.attributes
    );
}

#[test]
fn derived_proof_search_inspection_reports_explicit_work() {
    const DECL: &str = "LeanRsFixture.SourceRanges.documentedSimpTheorem";
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("fixture capability loads");
    let mut session = caps
        .session(
            &fixture_service_imports(&caps, "LeanRsFixture.SourceRanges"),
            None,
            None,
        )
        .expect("fixture session opens");
    let mut request = DeclarationInspectionRequest::new(DECL);
    request.fields.proof_search = true;

    let result = session
        .inspect_declaration(&request, None)
        .expect("inspection succeeds");

    let DeclarationInspectionResult::Found { declaration } = result else {
        panic!("expected found inspection");
    };
    assert!(declaration.proof_search.computed);
    assert!(declaration.proof_search.unavailable_reason.is_none());
    assert!(declaration.proof_search.is_simp);
    assert!(declaration.proof_search.is_rw_candidate);
    assert_eq!(declaration.derived_work.proof_search_fact_collections, 1);
    assert_eq!(declaration.derived_work.simp_extension_lookups, 1);
    assert!(declaration.attributes.iter().any(|attr| attr == "simp"));
}

#[test]
fn derived_statement_rendering_and_search_source_work_are_reported() {
    const DECL: &str = "LeanRsFixture.SourceRanges.knownTheorem";
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("fixture capability loads");
    let mut session = caps
        .session(
            &fixture_service_imports(&caps, "LeanRsFixture.SourceRanges"),
            None,
            None,
        )
        .expect("fixture session opens");

    let mut raw_request = DeclarationInspectionRequest::new(DECL);
    raw_request.fields = DeclarationInspectionFields {
        source: false,
        statement: true,
        docstring: false,
        attributes: false,
        flags: false,
        statement_pretty: false,
        proof_search: false,
    };
    let raw = session
        .inspect_declaration(&raw_request, None)
        .expect("raw inspection succeeds");
    let DeclarationInspectionResult::Found { declaration } = raw else {
        panic!("expected raw inspection");
    };
    assert_eq!(declaration.derived_work.raw_type_renderings, 1);
    assert_eq!(declaration.derived_work.pretty_prints, 0);

    let mut pretty_request = raw_request.clone();
    pretty_request.fields.statement_pretty = true;
    let pretty = session
        .inspect_declaration(&pretty_request, None)
        .expect("pretty inspection succeeds");
    let DeclarationInspectionResult::Found { declaration } = pretty else {
        panic!("expected pretty inspection");
    };
    assert_eq!(declaration.derived_work.pretty_prints, 1);

    let mut search = DeclarationSearchRequest::new("knownTheorem");
    search.include_source = false;
    let without_source = session.search_declarations(&search, None).expect("search succeeds");
    assert_eq!(without_source.facts.derived_work.source_range_lookups, 0);

    search.include_source = true;
    let with_source = session.search_declarations(&search, None).expect("search succeeds");
    assert!(with_source.facts.derived_work.source_range_lookups > 0);
}

#[test]
fn bracketed_import_query_result_surface_cannot_carry_lean_handles() {
    fn assert_owned<T: Send + Sync + 'static>() {}

    assert_owned::<LeanBracketedImportRequest>();
    assert_owned::<LeanBracketedImportResult>();
}

#[test]
fn bracketed_import_query_reports_rejected_full_session_operations() {
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let result = caps
        .bracketed_import_query(
            &service_requests::bracketed(&caps, &["LeanRsFixture.Handles"]),
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

    let absent = session
        .declaration_type("This.Name.Does.Not.Exist", None)
        .expect("type query for absent name");
    assert!(absent.is_none(), "missing name must yield None");
}

#[test]
fn session_declaration_name_renders_dotted_form() {
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_handles_session(&caps);

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

fn fixture_service_imports<'lean, 'c>(
    caps: &'c lean_rs_host::LeanCapabilities<'lean, '_>,
    module: &str,
) -> lean_rs::module::TrustedHostImports<'lean, 'c> {
    assert!(matches!(
        module,
        "LeanRsHostShims.Elaboration" | "LeanRsFixture.SourceRanges"
    ));
    let profile = LeanSessionImportProfile::default();
    if fixture::native_fixture_selected() {
        fixture::native_imports_for(caps, &[module], profile)
    } else {
        service_requests::imports(caps, &[module], profile)
    }
}

fn session_over_elaboration<'lean, 'c>(caps: &'c lean_rs_host::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(
        &fixture_service_imports(caps, "LeanRsHostShims.Elaboration"),
        None,
        None,
    )
    .expect("session imports cleanly")
}

fn fixture_elaborate<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    source: &str,
) -> lean_rs::LeanResult<Result<lean_rs::LeanExpr<'lean>, lean_rs_host::LeanElabFailure>> {
    let options = match source {
        "(1 + 2 : Nat)" | "(Nat.succ 0 : Nat)" | "1 +" | "(1 + \"hi\" : Nat)" => LeanElabOptions::new(),
        "(foo + bar + baz : Nat)" => LeanElabOptions::new().diagnostic_byte_limit(1),
        _ => panic!("unapproved elaboration fixture expression"),
    };
    if !fixture::native_fixture_selected() {
        return call_service!(caps, session, elaborate, source, None, &options, None);
    }
    let options = options.source_options();
    let paths = fixture::native_elaboration_paths(caps, session);
    let (env, scope) = session.source_context();
    #[allow(unsafe_code, reason = "five fixed terms in the held test fixture")]
    // SAFETY: the fixed request and its exact options use this held
    // elaboration environment, without an expected type or other source.
    let request = unsafe {
        caps.shim_capability()
            .term_scope(env, scope, paths, source.into(), None, options)
    }?;
    session.elaborate(&request, None)
}

fn fixture_kernel_check<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    source: &str,
) -> lean_rs::LeanResult<LeanKernelOutcome<'lean>> {
    let options = LeanElabOptions::new();
    if !fixture::native_fixture_selected() {
        return call_service!(caps, session, kernel_check, source, &options, None, None);
    }
    assert!(matches!(
        source,
        "theorem lean_rs_smoke : 1 + 1 = 2 := rfl"
            | "theorem lean_rs_bad : 1 = 2 := rfl"
            | "theorem :="
            | "#check Nat"
            | "theorem lean_rs_recheck : 1 + 1 = 2 := rfl"
            | "theorem lean_rs_summary : 1 + 1 = 2 := rfl"
    ));
    let paths = fixture::native_elaboration_paths(caps, session);
    let (env, scope) = session.source_context();
    #[allow(unsafe_code, reason = "six fixed commands in the held test fixture")]
    // SAFETY: the fixed command uses the held elaboration environment and
    // default kernel request options, with no other source.
    let request = unsafe {
        caps.shim_capability()
            .kernel_scope(env, scope, paths, source.into(), options.source_options())
    }?;
    session.kernel_check(&request, None, None)
}

#[test]
fn elaborate_success_returns_expr() {
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_elaborate(&caps, &mut session, "(1 + 2 : Nat)").expect("host stack reports no exception");
    let expr = outcome.expect("elaboration succeeds for a well-typed Nat term");
    // Returned LeanExpr is opaque; success path is asserted by Ok.
    drop(expr);
}

#[test]
fn session_expr_to_string_raw_renders_elaborated_expr() {
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let expr = fixture_elaborate(&caps, &mut session, "(Nat.succ 0 : Nat)")
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_elaborate(&caps, &mut session, "1 +").expect("host stack reports no exception");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // Mixing `String` with arithmetic against `Nat` triggers an
    // elaborator type error that carries a position.
    let outcome =
        fixture_elaborate(&caps, &mut session, "(1 + \"hi\" : Nat)").expect("host stack reports no exception");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem lean_rs_smoke : 1 + 1 = 2 := rfl";
    let outcome = fixture_kernel_check(&caps, &mut session, src).expect("host stack reports no exception");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem lean_rs_bad : 1 = 2 := rfl";
    let outcome = fixture_kernel_check(&caps, &mut session, src).expect("host stack reports no exception");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_kernel_check(&caps, &mut session, "theorem :=").expect("host stack reports no exception");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // `#check` is a command that elaborates cleanly but adds no
    // constant to the environment, so the classifier returns
    // `Unsupported` (no new theorem/definition).
    let outcome = fixture_kernel_check(&caps, &mut session, "#check Nat").expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        EvidenceStatus::Unsupported,
        "non-declaration command must classify as Unsupported, got {outcome:?}"
    );
}

#[test]
fn check_evidence_revalidates_checked_evidence() {
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_kernel_check(&caps, &mut session, "theorem lean_rs_recheck : 1 + 1 = 2 := rfl")
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_kernel_check(&caps, &mut session, "theorem lean_rs_summary : 1 + 1 = 2 := rfl")
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // Multiple unbound identifiers produce multiple diagnostics; a
    // single-byte budget cannot fit them all and must be reported as
    // truncated.
    let outcome =
        fixture_elaborate(&caps, &mut session, "(foo + bar + baz : Nat)").expect("host stack reports no exception");
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
    let caps = fixture::capabilities(&host).expect("load caps");

    // (a) One session, many queries.
    let start_reuse = Instant::now();
    {
        let mut session = caps
            .session(
                &service_requests::imports(&caps, &["LeanRsFixture.Handles"], LeanSessionImportProfile::default()),
                None,
                None,
            )
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
            .session(
                &service_requests::imports(&caps, &["LeanRsFixture.Handles"], LeanSessionImportProfile::default()),
                None,
                None,
            )
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

fn session_over_meta<'lean, 'c>(caps: &'c lean_rs_host::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(
        &service_requests::imports(
            caps,
            &["LeanRsFixture.Meta", "LeanRsHostShims.Meta"],
            LeanSessionImportProfile::default(),
        ),
        None,
        None,
    )
    .expect("session imports cleanly")
}

fn native_meta_session<'lean, 'c>(caps: &'c lean_rs_host::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    if !fixture::native_fixture_selected() {
        return session_over_meta(caps);
    }
    caps.session(
        &fixture::native_imports_for(
            caps,
            &["LeanRsFixture.Meta", "LeanRsHostShims.Meta"],
            LeanSessionImportProfile::default(),
        ),
        None,
        None,
    )
    .expect("session imports cleanly")
}

fn meta_expr<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    fixture: &str,
) -> lean_rs::LeanExpr<'lean> {
    let source = meta_source(fixture);
    call_service!(caps, session, elaborate, source, None, &LeanElabOptions::new(), None)
        .expect("fixture expression elaboration reports no exception")
        .expect("fixture expression elaborates")
}

fn meta_source(fixture: &str) -> &'static str {
    match fixture {
        "lean_rs_fixture_meta_expr_nat" => "Nat",
        "lean_rs_fixture_meta_expr_bool" => "Bool",
        "lean_rs_fixture_meta_expr_reducible_nat_alias" => "LeanRsFixture.Meta.ReducibleNatAlias",
        "lean_rs_fixture_meta_expr_irreducible_nat_alias" => "LeanRsFixture.Meta.IrreducibleNatAlias",
        other => panic!("unknown meta fixture expression export {other}"),
    }
}

fn native_meta_elaborate<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    source: &str,
) -> lean_rs::LeanResult<Result<lean_rs::LeanExpr<'lean>, lean_rs_host::LeanElabFailure>> {
    if !fixture::native_fixture_selected() {
        return call_service!(caps, session, elaborate, source, None, &LeanElabOptions::new(), None);
    }
    let term = fixture::native_meta_term(caps, session, source);
    session.elaborate(&term, None)
}

fn native_meta_expr<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    fixture: &str,
) -> lean_rs::LeanExpr<'lean> {
    native_meta_elaborate(caps, session, meta_source(fixture))
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

    let expr = native_meta_elaborate(&caps, &mut session, "(Nat.succ 0 : Nat)")
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

    let expr = native_meta_elaborate(&caps, &mut session, "(Nat.succ 0 : Nat)")
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

    let lhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_reducible_nat_alias");
    let rhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_nat");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

    let lhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_nat");
    let rhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_bool");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

    let lhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_irreducible_nat_alias");
    let rhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_nat");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

    let lhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_irreducible_nat_alias");
    let rhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_nat");
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
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = native_meta_session(&caps);

    let lhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_nat");
    let rhs = native_meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_nat");
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
    let caps = fixture::capabilities(&host).expect("load caps");
    let mut session = session_over_meta(&caps);

    let expr = meta_expr(&caps, &mut session, "lean_rs_fixture_meta_expr_nat");
    // Use the supported descriptor as a caller would, under strict fixture selection.
    good_shims_assertions::check_missing_whnf(&mut session, expr);
}

// -- process_module_query (bounded module projections) -------------------

enum ModuleQueryCase {
    Diagnostics,
    TypeAt,
    GoalAt,
    References,
    MissingImport,
    HeaderError,
}

impl ModuleQueryCase {
    fn request(self) -> (lean_rs::host_bridge::HostModuleQuery, LeanElabOptions) {
        use lean_rs_host::host::process::ModuleQuery;

        let (source, query, options) = match self {
            Self::Diagnostics => (
                "import Lean\n\ntheorem bad : True := 0\n",
                ModuleQuery::Diagnostics,
                LeanElabOptions::new(),
            ),
            Self::TypeAt => (
                "def x := 1\ntheorem t : x = 1 := by rfl\n",
                ModuleQuery::TypeAt { line: 2, column: 13 },
                LeanElabOptions::new(),
            ),
            Self::GoalAt => (
                "theorem t : True := by\n  trivial\n",
                ModuleQuery::GoalAt { line: 2, column: 4 },
                LeanElabOptions::new().diagnostic_byte_limit(256),
            ),
            Self::References => (
                "def x := 1\ntheorem t : x = 1 := by rfl\n#check x\n",
                ModuleQuery::References { name: "x".to_string() },
                LeanElabOptions::new(),
            ),
            Self::MissingImport => (
                "import Foo.Bar.Baz\n\ndef x := 1\n",
                ModuleQuery::Diagnostics,
                LeanElabOptions::new(),
            ),
            Self::HeaderError => (
                "import 123\n\ntheorem t : True := by trivial\n",
                ModuleQuery::Diagnostics,
                LeanElabOptions::new(),
            ),
        };
        (
            lean_rs::host_bridge::HostModuleQuery {
                source: source.into(),
                query,
            },
            options,
        )
    }
}

fn fixture_module_query<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    case: ModuleQueryCase,
) -> lean_rs::LeanResult<lean_rs_host::host::process::ModuleQueryOutcome> {
    let (request, options) = case.request();
    if !fixture::native_fixture_selected() {
        return call_service!(
            caps,
            session,
            process_module_query,
            &request.source,
            &request.query,
            &options,
            None
        );
    }
    let paths = fixture::native_elaboration_paths(caps, session);
    let (env, scope) = session.source_context();
    #[allow(unsafe_code, reason = "only the fixed queries in the held test fixture")]
    // SAFETY: the cases fix the source, query and options. The exact
    // retained native roots serve the same elaboration environment.
    let admitted = unsafe {
        caps.shim_capability()
            .module_query_scope(env, scope, paths, request, options.source_options())
    }?;
    session.process_module_query(&admitted, None)
}

#[test]
fn session_process_module_query_returns_diagnostics_without_info_tree_payload() {
    use lean_rs_host::host::process::{ModuleQueryOutcome, ModuleQueryResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_query(&caps, &mut session, ModuleQueryCase::Diagnostics)
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

enum ModuleBatchCase {
    CommandMessages,
    InvalidCommand,
    MissingImport,
    SmallMessageBudget,
    ProofContext,
    OutlineOrder,
    ScanTargets,
    ScanProofState,
    OutlineWithDiagnostics,
    SmallOutlineBudget,
    ScanOutline,
    PrivateOutline,
}

impl ModuleBatchCase {
    fn request(self) -> lean_rs::host_bridge::HostModuleBatch {
        use lean_rs_host::host::process::{ModuleQueryOutputBudgets, ModuleQuerySelector};

        let (source, selectors, budgets) = match self {
            Self::CommandMessages => (
                "#check Nat.add\n#print axioms Nat.add_assoc\n",
                vec![ModuleQuerySelector::Diagnostics {
                    id: "messages".to_owned(),
                }],
                ModuleQueryOutputBudgets::default(),
            ),
            Self::InvalidCommand => (
                "#check definitelyMissingCommandMessage\n",
                vec![ModuleQuerySelector::Diagnostics {
                    id: "messages".to_owned(),
                }],
                ModuleQueryOutputBudgets::default(),
            ),
            Self::MissingImport => (
                "import LeanRsFixture.DoesNotExist\n\n#check symbolThatWouldFailIfBodyRan\n",
                vec![ModuleQuerySelector::Diagnostics {
                    id: "messages".to_owned(),
                }],
                ModuleQueryOutputBudgets::default(),
            ),
            Self::SmallMessageBudget => (
                "#check Nat.add\n#check Nat.mul\n",
                vec![ModuleQuerySelector::Diagnostics {
                    id: "messages".to_owned(),
                }],
                ModuleQueryOutputBudgets {
                    per_field_bytes: 1,
                    total_bytes: 1024,
                },
            ),
            Self::ProofContext => (
                "theorem t (h : True) : True := by\n  exact h\n",
                vec![
                    ModuleQuerySelector::Diagnostics {
                        id: "diagnostics".to_owned(),
                    },
                    ModuleQuerySelector::ProofState {
                        id: "state".to_owned(),
                        line: 2,
                        column: 4,
                    },
                ],
                ModuleQueryOutputBudgets::default(),
            ),
            Self::OutlineOrder => (
                "\
namespace Outer
theorem first : True := by
  trivial
private theorem hidden : True := by
  trivial
def withBody (n : Nat) : Nat :=
  n + 1
end Outer
",
                vec![ModuleQuerySelector::DeclarationOutline {
                    id: "outline".to_owned(),
                }],
                ModuleQueryOutputBudgets::default(),
            ),
            Self::ScanTargets => {
                let selectors: Vec<ModuleQuerySelector> = [
                    ("target-structure", "ScanForms.Point"),
                    ("target-class", "ScanForms.Default"),
                    ("target-multi", "ScanForms.multi"),
                    ("target-theorem", "ScanForms.zeroOrSucc"),
                    ("target-where", "ScanForms.origin"),
                    ("target-instance", "ScanForms.namedDefault"),
                ]
                .into_iter()
                .map(|(id, name)| ModuleQuerySelector::DeclarationTarget {
                    id: id.to_owned(),
                    name: Some(name.to_owned()),
                    line: None,
                    column: None,
                })
                .collect();
                (SCAN_FORMS_SOURCE, selectors, ModuleQueryOutputBudgets::default())
            }
            Self::ScanProofState => (
                SCAN_FORMS_SOURCE,
                vec![
                    ModuleQuerySelector::SurroundingDeclaration {
                        id: "surrounding".to_owned(),
                        line: 29,
                        column: 7,
                    },
                    ModuleQuerySelector::ProofStateInDeclaration {
                        id: "proof-state".to_owned(),
                        declaration: "ScanForms.multiBy".to_owned(),
                        position: lean_rs_host::host::process::ProofPositionSelector::Default,
                        locals_raw: false,
                    },
                ],
                ModuleQueryOutputBudgets::default(),
            ),
            Self::OutlineWithDiagnostics => (
                "theorem ok : True := by\n  trivial\n#check missingName\n",
                vec![
                    ModuleQuerySelector::Diagnostics {
                        id: "diagnostics".to_owned(),
                    },
                    ModuleQuerySelector::DeclarationOutline {
                        id: "outline".to_owned(),
                    },
                ],
                ModuleQueryOutputBudgets::default(),
            ),
            Self::SmallOutlineBudget => (
                "theorem a : True := by\n  trivial\ntheorem b : True := by\n  trivial\n",
                vec![ModuleQuerySelector::DeclarationOutline {
                    id: "outline".to_owned(),
                }],
                ModuleQueryOutputBudgets {
                    per_field_bytes: 128,
                    total_bytes: 200,
                },
            ),
            Self::ScanOutline | Self::PrivateOutline => (
                if matches!(self, Self::ScanOutline) {
                    SCAN_FORMS_SOURCE
                } else {
                    PRIVATE_AXIOMS_SOURCE
                },
                vec![ModuleQuerySelector::DeclarationOutline {
                    id: "outline".to_owned(),
                }],
                ModuleQueryOutputBudgets::default(),
            ),
        };
        lean_rs::host_bridge::HostModuleBatch {
            source: source.into(),
            selectors,
            budgets,
        }
    }
}

fn fixture_module_batch<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    case: ModuleBatchCase,
) -> lean_rs::LeanResult<lean_rs_host::host::process::ModuleQueryBatchOutcome> {
    let request = case.request();
    if !fixture::native_fixture_selected() {
        return strict_module_batch(caps, session, &request);
    }
    let paths = fixture::native_elaboration_paths(caps, session);
    let (env, scope) = session.source_context();
    #[allow(unsafe_code, reason = "only the fixed module batches in the held test fixture")]
    // SAFETY: the closed cases fix the source, selectors and budgets, with
    // default options in the exact retained native elaboration environment.
    let admitted = unsafe {
        caps.shim_capability()
            .module_batch_scope(env, scope, paths, request, LeanElabOptions::new().source_options())
    }?;
    session.process_module_query_batch(&admitted, None)
}

fn strict_module_batch<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    request: &lean_rs::host_bridge::HostModuleBatch,
) -> lean_rs::LeanResult<lean_rs_host::host::process::ModuleQueryBatchOutcome> {
    call_service!(
        caps,
        session,
        process_module_query_batch,
        &request.source,
        &request.selectors,
        &request.budgets,
        &LeanElabOptions::new(),
        None
    )
}

#[test]
fn command_message_capture_returns_check_and_print_axioms_output_as_diagnostics() {
    use lean_rs_host::host::process::{ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::CommandMessages)
        .expect("host stack reports no exception");

    let ModuleQueryBatchOutcome::Ok { result, imports } = outcome else {
        panic!("expected Ok command-message outcome, got {outcome:?}");
    };
    assert!(imports.is_empty(), "body-only command source should have no imports");
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("expected one diagnostics item, got {:?}", result.items);
    };
    let ModuleQueryBatchResult::Diagnostics(diagnostics) = result.as_ref() else {
        panic!("expected diagnostics result, got {result:?}");
    };
    let messages: Vec<&str> = diagnostics.diagnostics().iter().map(|d| d.message()).collect();
    assert!(
        diagnostics
            .diagnostics()
            .iter()
            .all(|d| d.severity() != LeanSeverity::Error),
        "valid commands should not produce error diagnostics, got {messages:?}",
    );
    assert!(
        messages.iter().any(|message| message.contains("Nat.add")),
        "#check output should mention Nat.add, got {messages:?}",
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("Nat.add_assoc") && message.contains("axioms")),
        "#print axioms output should mention Nat.add_assoc axioms, got {messages:?}",
    );
}

#[test]
fn command_message_capture_invalid_command_returns_error_diagnostics() {
    use lean_rs_host::host::process::{ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::InvalidCommand)
        .expect("invalid command is a normal diagnostics outcome");

    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok diagnostics outcome for invalid command, got {outcome:?}");
    };
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("expected one diagnostics item, got {:?}", result.items);
    };
    let ModuleQueryBatchResult::Diagnostics(diagnostics) = result.as_ref() else {
        panic!("expected diagnostics result, got {result:?}");
    };
    assert!(
        diagnostics
            .diagnostics()
            .iter()
            .any(|d| d.severity() == LeanSeverity::Error && d.message().contains("definitelyMissingCommandMessage")),
        "invalid command should return an error diagnostic mentioning the missing name, got {:?}",
        diagnostics.diagnostics(),
    );
}

#[test]
fn command_message_capture_missing_import_uses_module_query_missing_imports() {
    use lean_rs_host::host::process::{ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::MissingImport)
        .expect("missing import is a normal module-query outcome");

    let ModuleQueryBatchOutcome::MissingImports {
        result,
        imports,
        missing,
    } = outcome
    else {
        panic!("expected MissingImports command-message outcome, got {outcome:?}");
    };
    assert_eq!(imports, vec!["LeanRsFixture.DoesNotExist".to_owned()]);
    assert_eq!(missing, vec!["LeanRsFixture.DoesNotExist".to_owned()]);
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("expected one diagnostics item, got {:?}", result.items);
    };
    let ModuleQueryBatchResult::Diagnostics(diagnostics) = result.as_ref() else {
        panic!("expected diagnostics result, got {result:?}");
    };
    assert!(
        diagnostics.diagnostics().is_empty(),
        "missing import should skip body command elaboration, got {:?}",
        diagnostics.diagnostics(),
    );
}

#[test]
fn command_message_capture_small_budget_truncates_later_messages() {
    use lean_rs_host::host::process::{ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::SmallMessageBudget)
        .expect("budgeted command-message query succeeds");

    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok command-message outcome, got {outcome:?}");
    };
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("expected one diagnostics item, got {:?}", result.items);
    };
    let ModuleQueryBatchResult::Diagnostics(diagnostics) = result.as_ref() else {
        panic!("expected diagnostics result, got {result:?}");
    };
    assert!(
        diagnostics.truncated(),
        "tiny budget should drop later command messages, got {:?}",
        diagnostics.diagnostics(),
    );
    assert!(
        !diagnostics.diagnostics().is_empty(),
        "diagnostic truncation should keep at least one message"
    );
}

#[test]
fn session_process_module_query_type_at_returns_one_selected_term() {
    use lean_rs_host::host::process::{ModuleQueryOutcome, ModuleQueryResult, TypeAtResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome =
        fixture_module_query(&caps, &mut session, ModuleQueryCase::TypeAt).expect("host stack reports no exception");

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
    use lean_rs_host::host::process::{GoalAtResult, ModuleQueryOutcome, ModuleQueryResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome =
        fixture_module_query(&caps, &mut session, ModuleQueryCase::GoalAt).expect("host stack reports no exception");

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
    use lean_rs_host::host::process::{
        ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult, ProofStateResult,
    };

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let before = session.stats();
    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::ProofContext)
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
fn declaration_outline_returns_source_order_namespace_private_and_body_spans() {
    use lean_rs_host::host::process::{
        DeclarationOutlineResult, ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    };

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::OutlineOrder)
        .expect("host stack reports no exception");

    let ModuleQueryBatchOutcome::Ok { result, imports } = outcome else {
        panic!("expected Ok declaration-outline outcome, got {outcome:?}");
    };
    assert!(imports.is_empty(), "body-only input should have no imports");
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("expected one Ok outline item, got {:?}", result.items);
    };
    let ModuleQueryBatchResult::DeclarationOutline(DeclarationOutlineResult {
        declarations,
        truncated,
    }) = result.as_ref()
    else {
        panic!("expected declaration-outline result, got {result:?}");
    };
    assert!(!truncated, "default budget should not truncate small outline");
    let short_names: Vec<&str> = declarations
        .iter()
        .map(|declaration| declaration.short_name.as_str())
        .collect();
    assert_eq!(short_names, ["first", "hidden", "withBody"]);
    let [first, hidden, with_body] = declarations.as_slice() else {
        panic!("expected three declaration-outline rows, got {declarations:?}");
    };
    assert_eq!(first.declaration_name, "Outer.first");
    assert_eq!(first.namespace_name, "Outer");
    assert!(
        hidden.declaration_name.contains("hidden"),
        "private declaration should be returned with Lean's resolved name, got {hidden:?}",
    );
    assert_ne!(
        with_body.name_span, with_body.body_span,
        "definition body span should be distinct from the name span"
    );
    assert!(
        first.declaration_span.start_line < with_body.declaration_span.start_line,
        "outline rows should preserve source order: {declarations:?}",
    );
}

/// Inline source exercising every declaration form the candidate scan
/// catalogued late: multi-clause equation `def` and `theorem`, a
/// `where`-structure def, a `structure`, a `class`, and an anonymous
/// `instance` (plus a named instance as the pre-existing control).
const SCAN_FORMS_SOURCE: &str = "\
namespace ScanForms

structure Point where
  x : Nat
  y : Nat

class Default (\u{3b1} : Type) where
  value : \u{3b1}

def multi : Nat \u{2192} Nat
  | 0 => 0
  | k + 1 => multi k + 1

theorem zeroOrSucc : \u{2200} n : Nat, n = 0 \u{2228} \u{2203} k, n = k + 1
  | 0 => Or.inl rfl
  | k + 1 => Or.inr \u{27e8}k, rfl\u{27e9}

def origin : Point where
  x := 0
  y := 0

instance : Default Point := \u{27e8}origin\u{27e9}

instance namedDefault : Default Nat := \u{27e8}0\u{27e9}

def multiBy : Nat \u{2192} Nat \u{2192} Nat
  | 0 => fun x => x
  | k + 1 => by
      intro x
      exact x + k

end ScanForms
";

fn outline_rows() -> Vec<lean_rs_host::host::process::DeclarationTargetInfo> {
    use lean_rs_host::host::process::{
        DeclarationOutlineResult, ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    };

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::ScanOutline)
        .expect("host stack reports no exception");
    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok declaration-outline outcome, got {outcome:?}");
    };
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("expected one Ok outline item, got {:?}", result.items);
    };
    let ModuleQueryBatchResult::DeclarationOutline(DeclarationOutlineResult {
        declarations,
        truncated,
    }) = result.as_ref()
    else {
        panic!("expected declaration-outline result, got {result:?}");
    };
    assert!(!truncated, "default budget should not truncate this outline");
    declarations.clone()
}

#[test]
fn declaration_outline_covers_multi_clause_where_structure_class_and_anonymous_instance() {
    let declarations = outline_rows();
    let rows: Vec<(&str, &str)> = declarations
        .iter()
        .map(|declaration| (declaration.short_name.as_str(), declaration.declaration_kind.as_str()))
        .collect();
    for expected in [
        ("Point", "structure"),
        ("Default", "class"),
        ("multi", "def"),
        ("zeroOrSucc", "theorem"),
        ("origin", "def"),
        ("namedDefault", "instance"),
    ] {
        assert!(
            rows.contains(&expected),
            "outline should contain {expected:?}, got {rows:?}"
        );
    }
    let anonymous = declarations
        .iter()
        .find(|declaration| {
            declaration.declaration_kind == "instance" && declaration.declaration_name.contains("instDefaultPoint")
        })
        .unwrap_or_else(|| panic!("anonymous instance should be catalogued under its generated name, got {rows:?}"));
    assert!(
        anonymous.declaration_name.starts_with("ScanForms.instDefaultPoint"),
        "generated instance name resolves in its namespace: {anonymous:?}"
    );
    // Every row keeps a distinct name/body span.
    for declaration in &declarations {
        assert_ne!(
            declaration.name_span, declaration.body_span,
            "row should carry distinct name and body spans: {declaration:?}"
        );
    }
    // The multi-clause def's body span covers both equation clauses.
    let multi = declarations
        .iter()
        .find(|declaration| declaration.short_name == "multi")
        .expect("multi row");
    assert!(
        multi.body_span.end_line > multi.body_span.start_line,
        "multi-clause body span covers more than one source line: {:?}",
        multi.body_span
    );
}

#[test]
fn declaration_target_resolves_the_new_scan_forms_by_name() {
    use lean_rs_host::host::process::{
        DeclarationTargetResult, ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    };

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::ScanTargets)
        .expect("host stack reports no exception");
    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok declaration-target outcome, got {outcome:?}");
    };
    for item in &result.items {
        let ModuleQueryBatchItem::Ok { id, result } = item else {
            panic!("expected Ok declaration-target item, got {item:?}");
        };
        let ModuleQueryBatchResult::DeclarationTarget(DeclarationTargetResult::Target(_)) = result.as_ref() else {
            panic!("{id} should resolve to a target, got {result:?}");
        };
    }
}

const PRIVATE_AXIOMS_SOURCE: &str = "module\npublic section\nnamespace PrivateAxioms\naxiom witness : False\nprivate theorem hidden : False := witness\nend PrivateAxioms\n";

enum VerificationBatchCase<'a> {
    AnonymousInstance(&'a str),
    PrivateAxioms(&'a str),
    NamedScanForms,
    OrderedResults,
    MissingKernelConstant,
    NoTargetsMissingImport,
    SmallTotalBudget,
}

impl VerificationBatchCase<'_> {
    fn request(self) -> lean_rs_host::host::process::DeclarationVerificationBatchRequest {
        use lean_rs_host::host::process::{
            DeclarationVerificationBatchItem, DeclarationVerificationBatchRequest, DeclarationVerificationTarget,
            ModuleQueryOutputBudgets, SorryPolicy,
        };
        match self {
            Self::AnonymousInstance(generated) => DeclarationVerificationBatchRequest {
                source: SCAN_FORMS_SOURCE.to_owned(),
                targets: vec![DeclarationVerificationBatchItem {
                    id: "anon".to_owned(),
                    target: DeclarationVerificationTarget::Name {
                        name: generated.to_owned(),
                    },
                }],
                sorry_policy: SorryPolicy::Deny,
                report_axioms: false,
                budgets: ModuleQueryOutputBudgets::default(),
            },
            Self::PrivateAxioms(canonical) => DeclarationVerificationBatchRequest {
                source: PRIVATE_AXIOMS_SOURCE.to_owned(),
                targets: ["hidden", "PrivateAxioms.hidden", canonical]
                    .into_iter()
                    .map(|name| DeclarationVerificationBatchItem {
                        id: name.to_owned(),
                        target: DeclarationVerificationTarget::Name { name: name.to_owned() },
                    })
                    .collect(),
                sorry_policy: SorryPolicy::Deny,
                report_axioms: true,
                budgets: ModuleQueryOutputBudgets::default(),
            },
            Self::NamedScanForms => {
                let targets = [
                    "ScanForms.multi",
                    "ScanForms.zeroOrSucc",
                    "ScanForms.origin",
                    "ScanForms.Point",
                    "ScanForms.Default",
                ]
                .into_iter()
                .map(|name| DeclarationVerificationBatchItem {
                    id: name.to_owned(),
                    target: DeclarationVerificationTarget::Name { name: name.to_owned() },
                })
                .collect();
                DeclarationVerificationBatchRequest {
                    source: SCAN_FORMS_SOURCE.to_owned(),
                    targets,
                    sorry_policy: SorryPolicy::Deny,
                    report_axioms: true,
                    budgets: ModuleQueryOutputBudgets::default(),
                }
            }
            Self::OrderedResults => {
                let source = "\
theorem closed : True := by
  trivial
theorem withSorry : True := by
  sorry
";
                DeclarationVerificationBatchRequest {
                    source: source.to_owned(),
                    targets: vec![
                        DeclarationVerificationBatchItem {
                            id: "closed-row".to_owned(),
                            target: DeclarationVerificationTarget::Name {
                                name: "closed".to_owned(),
                            },
                        },
                        DeclarationVerificationBatchItem {
                            id: "sorry-row".to_owned(),
                            target: DeclarationVerificationTarget::Name {
                                name: "withSorry".to_owned(),
                            },
                        },
                        DeclarationVerificationBatchItem {
                            id: "missing-row".to_owned(),
                            target: DeclarationVerificationTarget::Name {
                                name: "notDefinedHere".to_owned(),
                            },
                        },
                    ],
                    sorry_policy: SorryPolicy::Deny,
                    report_axioms: true,
                    budgets: ModuleQueryOutputBudgets::default(),
                }
            }
            Self::MissingKernelConstant => DeclarationVerificationBatchRequest {
                // The source declaration resolves, but Lean cannot add a theorem
                // whose type is not a proposition to the kernel environment.
                source: "theorem absent : Nat := 0\n".to_owned(),
                targets: vec![DeclarationVerificationBatchItem {
                    id: "absent".to_owned(),
                    target: DeclarationVerificationTarget::Name {
                        name: "absent".to_owned(),
                    },
                }],
                sorry_policy: SorryPolicy::Deny,
                report_axioms: true,
                budgets: ModuleQueryOutputBudgets::default(),
            },
            Self::NoTargetsMissingImport => DeclarationVerificationBatchRequest {
                source: "import Foo.Bar.Baz\n\ndef x := 1\n".to_owned(),
                targets: Vec::new(),
                sorry_policy: SorryPolicy::Deny,
                report_axioms: true,
                budgets: ModuleQueryOutputBudgets::default(),
            },
            Self::SmallTotalBudget => DeclarationVerificationBatchRequest {
                source: "theorem a : True := by trivial\ntheorem b : True := by trivial\n".to_owned(),
                targets: vec![
                    DeclarationVerificationBatchItem {
                        id: "a-row".to_owned(),
                        target: DeclarationVerificationTarget::Name { name: "a".to_owned() },
                    },
                    DeclarationVerificationBatchItem {
                        id: "b-row".to_owned(),
                        target: DeclarationVerificationTarget::Name { name: "b".to_owned() },
                    },
                ],
                sorry_policy: SorryPolicy::Deny,
                report_axioms: true,
                budgets: ModuleQueryOutputBudgets {
                    per_field_bytes: 1024,
                    total_bytes: 1,
                },
            },
        }
    }
}

fn fixture_verify_declarations<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    case: VerificationBatchCase<'_>,
) -> lean_rs::LeanResult<lean_rs_host::host::process::DeclarationVerificationBatchOutcome> {
    let request = case.request();
    let options = LeanElabOptions::new();
    if !fixture::native_fixture_selected() {
        return call_service!(caps, session, verify_declaration_batch, &request, &options, None);
    }
    let paths = fixture::native_elaboration_paths(caps, session);
    let (env, scope) = session.source_context();
    #[allow(
        unsafe_code,
        reason = "only the fixed verification requests in the held test fixture"
    )]
    // SAFETY: the cases fix the source, targets, policy and budgets. The
    // exact held native roots serve the same default elaboration environment.
    // Generated targets come from the outline of the same fixed source.
    let admitted = unsafe {
        caps.shim_capability()
            .verification_batch_scope(env, scope, paths, request, options.source_options())
    }?;
    session.verify_declaration_batch(&admitted, None)
}

#[test]
fn verify_declaration_batch_accepts_the_new_scan_forms_by_name() {
    use lean_rs_host::host::process::{DeclarationVerificationBatchOutcome, DeclarationVerificationStatus};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    let outcome = fixture_verify_declarations(&caps, &mut session, VerificationBatchCase::NamedScanForms)
        .expect("host stack reports no exception");
    let DeclarationVerificationBatchOutcome::Ok { results, .. } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    for row in &results {
        assert_eq!(
            row.status,
            DeclarationVerificationStatus::Accepted,
            "{} should verify by name instead of returning not_found",
            row.id
        );
    }
}

#[test]
fn verify_declaration_batch_resolves_anonymous_instance_under_generated_name() {
    use lean_rs_host::host::process::{DeclarationVerificationBatchOutcome, DeclarationVerificationStatus};

    // Phase 1: the outline reports the generated name the kernel recorded.
    let declarations = outline_rows();
    let generated = &declarations
        .iter()
        .find(|declaration| declaration.declaration_name.contains("instDefaultPoint"))
        .expect("anonymous instance is catalogued")
        .declaration_name;

    // Phase 2: verifying that name reaches the declaration, exactly what the
    // kan-proofs field report could not do.
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    let outcome = fixture_verify_declarations(&caps, &mut session, VerificationBatchCase::AnonymousInstance(generated))
        .expect("host stack reports no exception");
    let DeclarationVerificationBatchOutcome::Ok { results, .. } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    let [row] = results.as_slice() else {
        panic!("expected one verification row, got {results:?}");
    };
    assert_eq!(
        row.status,
        DeclarationVerificationStatus::Accepted,
        "anonymous instance verifies under its generated name {generated}"
    );
}

#[test]
fn surrounding_and_proof_state_work_inside_a_multi_clause_definition() {
    use lean_rs_host::host::process::{
        ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult, ProofStateResult,
        SurroundingDeclarationResult,
    };

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    // Line 29 is the `intro x` tactic inside the second equation clause of
    // `multiBy` (1-based): a tactic block nested in a multi-clause def.
    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::ScanProofState)
        .expect("host stack reports no exception");
    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok outcome, got {outcome:?}");
    };
    let [surrounding, proof_state] = result.items.as_slice() else {
        panic!("expected two items, got {:?}", result.items);
    };
    let ModuleQueryBatchItem::Ok {
        result: surrounding_result,
        ..
    } = surrounding
    else {
        panic!("expected Ok surrounding item, got {surrounding:?}");
    };
    let ModuleQueryBatchResult::SurroundingDeclaration(SurroundingDeclarationResult::Declaration(info)) =
        surrounding_result.as_ref()
    else {
        panic!("clause cursor should land inside a declaration, got {surrounding_result:?}");
    };
    assert_eq!(info.declaration_name, "ScanForms.multiBy");
    let ModuleQueryBatchItem::Ok {
        result: proof_state_result,
        ..
    } = proof_state
    else {
        panic!("expected Ok proof-state item, got {proof_state:?}");
    };
    assert!(
        matches!(
            proof_state_result.as_ref(),
            ModuleQueryBatchResult::ProofState(ProofStateResult::State { .. })
        ),
        "proof state resolves inside a multi-clause def, got {proof_state_result:?}"
    );
}

#[test]
fn declaration_outline_returns_rows_alongside_diagnostics() {
    use lean_rs_host::host::process::{
        DeclarationOutlineResult, ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    };

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::OutlineWithDiagnostics)
        .expect("host stack reports no exception");
    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok declaration-outline outcome, got {outcome:?}");
    };
    let diagnostics = result
        .items
        .iter()
        .find(|item| item.id() == "diagnostics")
        .expect("diagnostics item present");
    match diagnostics {
        ModuleQueryBatchItem::Ok { result, .. } => match result.as_ref() {
            ModuleQueryBatchResult::Diagnostics(failure) => assert!(
                failure
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.severity() == lean_rs_host::LeanSeverity::Error),
                "expected diagnostic error for missingName, got {failure:?}",
            ),
            other => panic!("expected diagnostics result, got {other:?}"),
        },
        other => panic!("expected Ok diagnostics item, got {other:?}"),
    }
    let outline = result
        .items
        .iter()
        .find(|item| item.id() == "outline")
        .expect("outline item present");
    match outline {
        ModuleQueryBatchItem::Ok { result, .. } => match result.as_ref() {
            ModuleQueryBatchResult::DeclarationOutline(DeclarationOutlineResult {
                declarations,
                truncated,
            }) => {
                assert!(!truncated);
                assert_eq!(declarations.len(), 1);
                let declaration = declarations.first().expect("outline row present");
                assert_eq!(declaration.declaration_name, "ok");
            }
            other => panic!("expected declaration-outline result, got {other:?}"),
        },
        other => panic!("expected Ok outline item, got {other:?}"),
    }
}

#[test]
fn declaration_outline_truncates_by_complete_rows_under_small_budget() {
    use lean_rs_host::host::process::{
        DeclarationOutlineResult, ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    };

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_batch(&caps, &mut session, ModuleBatchCase::SmallOutlineBudget)
        .expect("host stack reports no exception");
    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok declaration-outline outcome, got {outcome:?}");
    };
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("expected one Ok outline item, got {:?}", result.items);
    };
    let ModuleQueryBatchResult::DeclarationOutline(DeclarationOutlineResult {
        declarations,
        truncated,
    }) = result.as_ref()
    else {
        panic!("expected declaration-outline result, got {result:?}");
    };
    assert_eq!(declarations.len(), 1);
    let declaration = declarations.first().expect("outline row present");
    assert_eq!(declaration.declaration_name, "a");
    assert!(*truncated, "second row should be omitted as a complete row");
}

#[test]
fn session_process_module_query_references_returns_name_locations_only() {
    use lean_rs_host::host::process::{ModuleQueryOutcome, ModuleQueryResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_query(&caps, &mut session, ModuleQueryCase::References)
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
    use lean_rs_host::host::process::{ModuleQueryOutcome, ModuleQueryResult};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_query(&caps, &mut session, ModuleQueryCase::MissingImport)
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
fn declaration_verification_batch_preserves_order_and_uses_one_dispatch() {
    use lean_rs_host::host::process::{DeclarationVerificationBatchOutcome, DeclarationVerificationStatus};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let before = session.stats();
    let outcome = fixture_verify_declarations(&caps, &mut session, VerificationBatchCase::OrderedResults)
        .expect("host stack reports no exception");
    let after = session.stats();
    assert_eq!(after.ffi_calls, before.ffi_calls + 1);
    assert_eq!(after.batch_items, before.batch_items + 3);

    let DeclarationVerificationBatchOutcome::Ok { results, imports } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    assert!(imports.is_empty(), "body-only input should have no imports");
    let ids: Vec<&str> = results.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["closed-row", "sorry-row", "missing-row"]);
    let statuses: Vec<DeclarationVerificationStatus> = results.iter().map(|row| row.status).collect();
    assert_eq!(
        statuses,
        [
            DeclarationVerificationStatus::Accepted,
            DeclarationVerificationStatus::Rejected,
            DeclarationVerificationStatus::NotFound,
        ]
    );
    let [closed, with_sorry, missing] = results.as_slice() else {
        panic!("expected three verification rows, got {results:?}");
    };
    assert!(
        closed.facts.axioms_available,
        "resolved first row should have an axiom walk when requested"
    );
    assert!(with_sorry.facts.contains_sorry);
    assert!(!missing.facts.axioms_available);
}

#[test]
fn private_declaration_verification_reports_real_axioms() {
    use lean_rs_host::host::process::{
        DeclarationVerificationBatchOutcome, DeclarationVerificationStatus, ModuleQueryBatchItem,
        ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    };
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    // The module header shifts parser coordinates and exercises private-name resolution.
    let outline = fixture_module_batch(&caps, &mut session, ModuleBatchCase::PrivateOutline).expect("outline");
    let ModuleQueryBatchOutcome::Ok { result, .. } = outline else {
        panic!("{outline:?}")
    };
    let [ModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        panic!("{result:?}")
    };
    let ModuleQueryBatchResult::DeclarationOutline(outline) = result.as_ref() else {
        panic!("{result:?}")
    };
    let [declaration] = outline.declarations.as_slice() else {
        panic!("{outline:?}")
    };
    let canonical = &declaration.declaration_name;
    assert!(canonical.starts_with("_private."), "{canonical}");
    let outcome = fixture_verify_declarations(&caps, &mut session, VerificationBatchCase::PrivateAxioms(canonical))
        .expect("verify");
    let DeclarationVerificationBatchOutcome::Ok { results, .. } = outcome else {
        panic!("{outcome:?}")
    };
    for row in results {
        assert_eq!(row.status, DeclarationVerificationStatus::Accepted, "{row:?}");
        assert!(row.facts.axioms_available, "{row:?}");
        assert_eq!(row.facts.axioms, ["PrivateAxioms.witness"], "{row:?}");
    }
}

#[test]
fn declaration_without_kernel_constant_has_unavailable_axioms() {
    use lean_rs_host::host::process::{DeclarationVerificationBatchOutcome, DeclarationVerificationStatus};
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    let outcome =
        fixture_verify_declarations(&caps, &mut session, VerificationBatchCase::MissingKernelConstant).expect("verify");
    let DeclarationVerificationBatchOutcome::Ok { results, .. } = outcome else {
        panic!("{outcome:?}")
    };
    let [row] = results.as_slice() else {
        panic!("{results:?}")
    };
    assert_eq!(row.status, DeclarationVerificationStatus::Rejected);
    assert!(
        row.facts.target.is_some(),
        "must exercise axiom lookup for a resolved source declaration"
    );
    assert!(
        !row.facts.axioms_available,
        "missing kernel constant is not axiom-free: {row:?}"
    );
    assert!(row.facts.axioms.is_empty());
}

#[test]
fn declaration_verification_batch_empty_targets_still_reports_missing_imports() {
    use lean_rs_host::host::process::DeclarationVerificationBatchOutcome;

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_verify_declarations(&caps, &mut session, VerificationBatchCase::NoTargetsMissingImport)
        .expect("host stack reports no exception");
    let DeclarationVerificationBatchOutcome::MissingImports {
        results,
        imports,
        missing,
    } = outcome
    else {
        panic!("expected MissingImports batch outcome, got {outcome:?}");
    };
    assert!(results.is_empty());
    assert_eq!(imports, vec!["Foo.Bar.Baz".to_string()]);
    assert_eq!(missing, vec!["Foo.Bar.Baz".to_string()]);
}

#[test]
fn declaration_verification_batch_total_budget_exhaustion_returns_ordered_budget_rows() {
    use lean_rs_host::host::process::{DeclarationVerificationBatchOutcome, DeclarationVerificationStatus};

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_verify_declarations(&caps, &mut session, VerificationBatchCase::SmallTotalBudget)
        .expect("host stack reports no exception");
    let DeclarationVerificationBatchOutcome::Ok { results, .. } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    let [first, second] = results.as_slice() else {
        panic!("expected two verification rows, got {results:?}");
    };
    assert_eq!(first.id, "a-row");
    assert_eq!(second.id, "b-row");
    assert!(
        results
            .iter()
            .all(|row| row.status == DeclarationVerificationStatus::BudgetExceeded),
        "tiny total budget should produce BudgetExceeded rows, got {results:?}",
    );
}

#[test]
fn session_process_module_query_surfaces_header_parse_error() {
    use lean_rs_host::host::process::ModuleQueryOutcome;

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = fixture_module_query(&caps, &mut session, ModuleQueryCase::HeaderError)
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

const MODULE_SYNTAX_IMPORTS: [&str; 3] = ["Fixture.Imported", "Fixture.Internal", "Fixture.PrivateScope"];
const MODULE_SYNTAX_REORDERED: [&str; 3] = ["Fixture.Internal", "Fixture.PrivateScope", "Fixture.Imported"];

fn module_syntax_host(name: &str) -> (LeanHost<'static>, Option<TempLakeProject>) {
    if fixture::native_fixture_selected() {
        let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens fixture");
        return (host, None);
    }
    let project = TempLakeProject::new(name);
    write_module_syntax_fixture(&project);
    let host = LeanHost::from_lake_project(runtime(), project.reviewed_copy()).expect("host opens temp project");
    (host, Some(project))
}

fn module_syntax_session<'lean, 'c>(
    caps: &'c lean_rs_host::LeanCapabilities<'lean, '_>,
    names: &[&str],
) -> LeanSession<'lean, 'c> {
    let scope = if fixture::native_fixture_selected() {
        fixture::native_module_syntax_imports(caps, names)
    } else {
        service_requests::imports(caps, names, LeanSessionImportProfile::default())
    };
    caps.session(&scope, None, None)
        .expect("session imports module-system fixture dependencies")
}

fn module_syntax_query<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
) -> lean_rs::LeanResult<lean_rs_host::host::process::ModuleQueryOutcome> {
    use lean_rs_host::host::process::ModuleQuery;
    const SOURCE: &str = "\
module

public import Fixture.Imported
import all Fixture.Internal
import Fixture.PrivateScope

def moduleSyntaxFoo : Nat := imported + internalSecret
#check privateOnly
";

    let options = LeanElabOptions::new();
    if !fixture::native_fixture_selected() {
        return call_service!(
            caps,
            session,
            process_module_query,
            SOURCE,
            &ModuleQuery::Diagnostics,
            &options,
            None
        );
    }
    let paths = fixture::native_module_syntax_source_paths(caps, session, &MODULE_SYNTAX_IMPORTS);
    let (env, scope) = session.source_context();
    let request = lean_rs::host_bridge::HostModuleQuery {
        source: SOURCE.into(),
        query: ModuleQuery::Diagnostics,
    };
    #[allow(
        unsafe_code,
        reason = "one exact diagnostics query in the held module syntax fixture"
    )]
    // SAFETY: this fixed request uses the exact fixture graph, retained paths
    // and default diagnostics options; no caller-provided source enters.
    let admitted = unsafe {
        caps.shim_capability()
            .module_query_scope(env, scope, paths, request, options.source_options())
    }?;
    session.process_module_query(&admitted, None)
}

#[test]
fn session_process_module_query_handles_module_system_header() {
    use lean_rs_host::host::process::{ModuleQueryOutcome, ModuleQueryResult};

    let (host, _temp) = module_syntax_host("module-query-header-host");
    let caps = fixture::native_shims_only(&host).expect("shim-only capabilities load");
    let mut session = module_syntax_session(&caps, &MODULE_SYNTAX_IMPORTS);
    let outcome = module_syntax_query(&caps, &mut session).expect("host stack reports no exception");
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

#[derive(Clone, Copy)]
enum ModuleSyntaxCacheCase {
    Base,
    Body,
    Visibility,
    Private,
    Public,
    Options,
    Label,
    NewSession,
}

impl ModuleSyntaxCacheCase {
    fn key(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Body => "body",
            Self::Visibility => "visibility",
            Self::Private => "private",
            Self::Public => "public",
            Self::Options => "options",
            Self::Label => "label",
            Self::NewSession => "new-session",
        }
    }
}

fn module_syntax_cached_query<'lean>(
    caps: &lean_rs_host::LeanCapabilities<'lean, '_>,
    session: &mut LeanSession<'lean, '_>,
    case: ModuleSyntaxCacheCase,
) -> lean_rs::LeanResult<lean_rs_host::host::process::ModuleQueryBatchCachedOutcome> {
    use lean_rs_host::host::process::{ModuleQueryCachePolicy, ModuleQueryOutputBudgets, ModuleQuerySelector};

    let header = "module\npublic import Fixture.Imported\nimport all Fixture.Internal\nimport Fixture.PrivateScope\n";
    let base = format!("{header}\ndef foo : Nat := imported + internalSecret\n#check privateOnly\n");
    let source = match case {
        ModuleSyntaxCacheCase::Body => format!("{base}\ndef bar : Nat := 3\n"),
        ModuleSyntaxCacheCase::Visibility => base.replace("import all Fixture.Internal", "import Fixture.Internal"),
        ModuleSyntaxCacheCase::Private => {
            base.replace("import Fixture.PrivateScope", "import all Fixture.PrivateScope")
        }
        ModuleSyntaxCacheCase::Public => base.replace("public import Fixture.Imported", "import Fixture.Imported"),
        ModuleSyntaxCacheCase::Base
        | ModuleSyntaxCacheCase::Options
        | ModuleSyntaxCacheCase::Label
        | ModuleSyntaxCacheCase::NewSession => base,
    };
    let policy = ModuleQueryCachePolicy {
        file_identity: "module-header-test".to_owned(),
        key: case.key().to_owned(),
        max_entries: 8,
        ttl_millis: 60_000,
        max_bytes: 1_048_576,
    };
    let label = if matches!(case, ModuleSyntaxCacheCase::Label) {
        "/cache/other.lean"
    } else {
        "/cache/module.lean"
    };
    let heartbeats = if matches!(case, ModuleSyntaxCacheCase::Options) {
        200_000
    } else {
        100_000
    };
    let options = LeanElabOptions::new().file_label(label).heartbeat_limit(heartbeats);
    let selectors = [ModuleQuerySelector::Diagnostics {
        id: "diagnostics".into(),
    }];
    let budgets = ModuleQueryOutputBudgets::default();
    if !fixture::native_fixture_selected() {
        return call_service!(
            caps,
            session,
            process_module_query_batch_cached,
            &source,
            &selectors,
            &budgets,
            &options,
            &policy,
            None
        );
    }
    let names = if matches!(case, ModuleSyntaxCacheCase::NewSession) {
        &MODULE_SYNTAX_REORDERED
    } else {
        &MODULE_SYNTAX_IMPORTS
    };
    let paths = fixture::native_module_syntax_source_paths(caps, session, names);
    let (env, scope) = session.source_context();
    let request = lean_rs::host_bridge::HostCachedModuleBatch {
        batch: lean_rs::host_bridge::HostModuleBatch {
            source,
            selectors: selectors.into(),
            budgets,
        },
        policy: format!(
            "{}\n{}\n{}\n{}\n{}",
            policy.file_identity, policy.key, policy.max_entries, policy.ttl_millis, policy.max_bytes
        ),
    };
    #[allow(
        unsafe_code,
        reason = "eight exact cached requests in the held module syntax fixture"
    )]
    // SAFETY: the cases fix the source, diagnostics selector, budgets, cache
    // policy and options in the exact retained import graph.
    let admitted = unsafe {
        caps.shim_capability()
            .cached_module_batch_scope(env, scope, paths, request, options.source_options())
    }?;
    session.process_module_query_batch_cached(&admitted, None)
}

/// Warm module headers must preserve the same visibility and diagnostics as a
/// fresh import, including when a caller changes the header or its options.
#[test]
fn cached_module_headers_match_cold_queries_after_edits() {
    use lean_rs_host::host::process::ModuleQueryBatchCachedOutcome;

    let (host, _temp) = module_syntax_host("cached-module-header-host");
    let caps = fixture::native_shims_only(&host).expect("shim-only capabilities load");
    let mut session = module_syntax_session(&caps, &MODULE_SYNTAX_IMPORTS);
    use ModuleSyntaxCacheCase::{Base, Body, Label, NewSession, Options, Private, Public, Visibility};
    for case in [Body, Visibility, Private, Public, Options, Label] {
        let name = case.key();
        session.clear_module_snapshot_cache().expect("clear prior case");
        session.clear_module_header_cache().expect("clear prior header");
        module_syntax_cached_query(&caps, &mut session, Base).expect("seed snapshot");
        let warm = module_syntax_cached_query(&caps, &mut session, case).expect("warm query");
        session.clear_module_snapshot_cache().expect("clear warm snapshot");
        session
            .clear_module_header_cache()
            .expect("force genuinely cold header");
        let cold = module_syntax_cached_query(&caps, &mut session, case).expect("cold query");
        let (
            ModuleQueryBatchCachedOutcome::Ok { result: warm, .. },
            ModuleQueryBatchCachedOutcome::Ok { result: cold, .. },
        ) = (warm, cold)
        else {
            panic!("expected successful envelopes for {name}");
        };
        assert_eq!(
            format!("{:?}", warm.items),
            format!("{:?}", cold.items),
            "warm/cold mismatch: {name}"
        );
    }
    // Same file and header, but a different imported environment. The global
    // snapshot cache must not carry a header environment across sessions.
    let mut other = module_syntax_session(&caps, &MODULE_SYNTAX_REORDERED);
    let warm = module_syntax_cached_query(&caps, &mut other, NewSession).expect("new-session query");
    other.clear_module_snapshot_cache().expect("clear new-session snapshot");
    other.clear_module_header_cache().expect("clear new-session header");
    let cold = module_syntax_cached_query(&caps, &mut other, NewSession).expect("cold new-session query");
    let (
        ModuleQueryBatchCachedOutcome::Ok { result: warm, .. },
        ModuleQueryBatchCachedOutcome::Ok { result: cold, .. },
    ) = (warm, cold)
    else {
        panic!("expected successful new-session envelopes");
    };
    assert_eq!(format!("{:?}", warm.items), format!("{:?}", cold.items));
}

#[test]
fn session_import_observes_lean_root_order_before_reusing_a_profile() {
    let project = TempLakeProject::new("import-root-order");
    let dependency = project.path().join("dependency");
    let inherited = project.path().join("inherited");
    fs::create_dir(&dependency).expect("dependency root");
    fs::create_dir(&inherited).expect("inherited root");
    for (root, declaration) in [
        (project.path(), "fromProjectRoot"),
        (dependency.as_path(), "fromDependencyRoot"),
    ] {
        fs::write(
            root.join("OrderSelection.lean"),
            format!("module\npublic def {declaration} : Nat := 1\n"),
        )
        .expect("write module");
        let build = Command::new("lean")
            .args(["-o", "OrderSelection.olean", "OrderSelection.lean"])
            .current_dir(root)
            .env_remove("LEAN_PATH")
            .output()
            .expect("compile root selection module");
        assert!(
            build.status.success(),
            "{} {}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
    }

    let shims = lean_rs_host::host::prepare_host_artifacts().expect("prepare bundled shims");
    let shim_paths = shims.shim_search_paths();
    let inherited_path =
        std::env::join_paths(std::iter::once(inherited.as_path()).chain(shim_paths.iter().map(PathBuf::as_path)))
            .expect("encode inherited path");
    let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lean/session_import_paths.lean");
    let run = Command::new("lean")
        .arg("--run")
        .arg(probe)
        .arg(project.path())
        .arg(dependency)
        .args(shim_paths)
        .env("LEAN_PATH", inherited_path)
        .output()
        .expect("run real Lean import");
    assert!(
        run.status.success(),
        "{} {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn session_import_runs_the_selected_plugin_with_its_compiled_module() {
    let project = TempLakeProject::new("import-plugin");
    let root = project.path().canonicalize().expect("private plugin project");
    project.write("QualifiedPlugin.lean", include_str!("lean/QualifiedPlugin.lean"));
    let prefix = Path::new(lean_toolchain::LEAN_HEADER_PATH)
        .ancestors()
        .nth(3)
        .expect("selected Lean release prefix");
    let path = std::env::join_paths([prefix.join("bin"), PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
        .expect("fixture executable search path");
    // Select the linked release and a clean loader environment before executing native tools.
    let tool = |name: &str| {
        let mut command = Command::new(prefix.join("bin").join(name));
        command
            .current_dir(&root)
            .env_clear()
            .env("PATH", &path)
            .env("LEAN_SYSROOT", prefix);
        command
    };
    let run = |command: &mut Command| {
        let output = command.output().expect("execute selected Lean tool");
        assert!(
            output.status.success(),
            "{command:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(tool("lean").args([
        "-o",
        "QualifiedPlugin.olean",
        "-c",
        "QualifiedPlugin.c",
        "QualifiedPlugin.lean",
    ]));
    let image = root.join(format!(
        "{}QualifiedPlugin{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    run(tool("leanc")
        .args(["-DLEAN_EXPORTING", "-shared", "-o"])
        .arg(&image)
        .arg("QualifiedPlugin.c"));
    let shims = lean_rs_host::host::prepare_host_artifacts().expect("prepare bundled shims");
    let paths = std::env::join_paths(shims.shim_search_paths()).expect("shim import paths");
    let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lean/session_import_plugin.lean");
    let marker = root.join("initialized");
    run(tool("lean")
        .env("LEAN_PATH", paths)
        .env("LEAN_RS_TEST_PLUGIN_MARKER", &marker)
        .arg("--run")
        .arg(probe)
        .arg(&root)
        .arg(image)
        .arg(marker));
}

/// Check reuse by object identity, not timing. Compile the private-state probe
/// with the shim source so no test-only symbol enters the shipped library.
#[test]
fn module_header_reuse_keeps_the_environment_and_rejects_changed_inputs() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shims/lean-rs-host-shims");
    let build = Command::new("lake")
        .args(["build", "LeanRsHostShims.Elaboration"])
        .current_dir(&package)
        .output()
        .expect("build probe imports");
    assert!(
        build.status.success(),
        "probe imports: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let project = TempLakeProject::new("header-reuse-private-probe");
    let source = fs::read_to_string(package.join("LeanRsHostShims/InfoTree.lean")).expect("read shim source");
    project.write(
        "HeaderReuseProbe.lean",
        &format!("{source}\n{}", include_str!("lean/header_reuse_probe.lean")),
    );
    let compile = Command::new("lake")
        .args(["env", "lean", "-R"])
        .arg(project.path())
        .arg("-o")
        .arg(project.path().join("HeaderReuseProbe.olean"))
        .arg(project.path().join("HeaderReuseProbe.lean"))
        .current_dir(&package)
        .output()
        .expect("compile header probe module");
    assert!(
        compile.status.success(),
        "header probe compile: {} {}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    project.write(
        "Run.lean",
        "import HeaderReuseProbe\n#eval LeanRsFixture.InfoTree.runHeaderReuseProbe\n",
    );
    let result = Command::new("lake")
        .args(["env", "lean"])
        .env("LEAN_PATH", project.path())
        .arg(project.path().join("Run.lean"))
        .current_dir(package)
        .output()
        .expect("run header reuse probe");
    assert!(
        result.status.success(),
        "header reuse probe failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
}

#[test]
fn module_options_match_direct_lean_and_invalidate_cached_elaboration() {
    use lean_rs_host::host::process::{
        DeclarationVerificationBatchItem, DeclarationVerificationBatchOutcome, DeclarationVerificationBatchRequest,
        DeclarationVerificationOutcome, DeclarationVerificationRequest, DeclarationVerificationStatus,
        DeclarationVerificationTarget, ModuleQuery, ModuleQueryBatchCachedOutcome, ModuleQueryBatchItem,
        ModuleQueryBatchOutcome, ModuleQueryBatchResult, ModuleQueryCachePolicy, ModuleQueryOutcome,
        ModuleQueryOutputBudgets, ModuleQueryResult, ModuleQuerySelector, SorryPolicy,
    };
    use lean_toolchain::LeanOptionValue;

    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    let source = "module\n@[expose] public section\nuniverse u\ndef before (A : Type u) := A\nend\n@[expose] public section\ndef after (A : Type u) := A\nend\n";
    let default = LeanElabOptions::new().file_label("option-regression.lean");
    let strict = default.clone().lean_options(
        [
            ("autoImplicit".to_owned(), LeanOptionValue::Bool(false)),
            ("maxSynthPendingDepth".to_owned(), LeanOptionValue::Nat(3)),
            ("pp.unicode.fun".to_owned(), LeanOptionValue::Bool(true)),
        ]
        .into(),
    );
    let weak_strict = default.clone().lean_options(
        [
            (
                "weak.autoImplicit".to_owned(),
                LeanOptionValue::String("false".to_owned()),
            ),
            (
                "weak.unregistered.regression.option".to_owned(),
                LeanOptionValue::Bool(true),
            ),
        ]
        .into(),
    );
    let selectors = [ModuleQuerySelector::Diagnostics {
        id: "diagnostics".to_owned(),
    }];
    let budgets = ModuleQueryOutputBudgets::default();
    // Deliberately identical key: the host must not reuse a snapshot with other options.
    let policy = ModuleQueryCachePolicy {
        file_identity: "option-regression.lean".to_owned(),
        key: "same-source".to_owned(),
        max_entries: 8,
        ttl_millis: 60_000,
        max_bytes: 1_048_576,
    };
    let request = DeclarationVerificationRequest {
        source: source.to_owned(),
        target: DeclarationVerificationTarget::Name {
            name: "after".to_owned(),
        },
        sorry_policy: SorryPolicy::Deny,
        report_axioms: true,
        budgets: budgets.clone(),
    };
    for (options, should_error) in [
        (&default, false),
        (&strict, true),
        (&default, false),
        (&strict, true),
        (&weak_strict, true),
    ] {
        let ModuleQueryOutcome::Ok {
            result: ModuleQueryResult::Diagnostics(diags),
            ..
        } = call_service!(
            caps,
            session,
            process_module_query,
            source,
            &ModuleQuery::Diagnostics,
            options,
            None; option_regression
        )
        .expect("query")
        else {
            panic!("expected source diagnostics");
        };
        assert_eq!(
            diags
                .diagnostics()
                .iter()
                .any(|d| d.message().contains("unknown universe level")),
            should_error
        );
        let ModuleQueryBatchCachedOutcome::Ok { result, .. } = call_service!(
            caps,
            session,
            process_module_query_batch_cached,
            source,
            &selectors,
            &budgets,
            options,
            &policy,
            None; option_regression
        )
        .expect("cached query") else {
            panic!("expected cached diagnostics");
        };
        let ModuleQueryBatchItem::Ok { result, .. } = result.items.first().expect("one selector result") else {
            panic!("expected selector result")
        };
        let ModuleQueryBatchResult::Diagnostics(diags) = result.as_ref() else {
            panic!("expected diagnostics")
        };
        assert_eq!(
            diags
                .diagnostics()
                .iter()
                .any(|d| d.message().contains("unknown universe level")),
            should_error
        );
        let DeclarationVerificationOutcome::Ok { status, .. } =
            call_service!(caps, session, verify_declaration, &request, options, None; option_regression)
                .expect("verification")
        else {
            panic!("expected verification")
        };
        assert_eq!(status == DeclarationVerificationStatus::Accepted, !should_error);
        let batch = DeclarationVerificationBatchRequest {
            source: source.to_owned(),
            targets: vec![DeclarationVerificationBatchItem {
                id: "after".to_owned(),
                target: request.target.clone(),
            }],
            sorry_policy: SorryPolicy::Deny,
            report_axioms: true,
            budgets: budgets.clone(),
        };
        let DeclarationVerificationBatchOutcome::Ok { results, .. } =
            call_service!(caps, session, verify_declaration_batch, &batch, options, None; option_regression)
                .expect("batch verification")
        else {
            panic!("expected batch verification");
        };
        assert_eq!(
            results.first().expect("one row").status == DeclarationVerificationStatus::Accepted,
            !should_error
        );
        let ModuleQueryBatchOutcome::Ok { result, .. } = call_service!(
            caps,
            session,
            process_module_query_batch,
            source,
            &selectors,
            &budgets,
            options,
            None; option_regression
        )
        .expect("uncached batch") else {
            panic!("expected batch diagnostics");
        };
        let ModuleQueryBatchItem::Ok { result, .. } = result.items.first().expect("one selector") else {
            panic!("expected diagnostics")
        };
        let ModuleQueryBatchResult::Diagnostics(diags) = result.as_ref() else {
            panic!("expected diagnostics")
        };
        assert_eq!(
            diags
                .diagnostics()
                .iter()
                .any(|d| d.message().contains("unknown universe level")),
            should_error
        );
    }
    // A source-local option must still override initial project defaults.
    let local = source.replace(
        "@[expose] public section",
        "set_option autoImplicit true\n@[expose] public section",
    );
    let ModuleQueryOutcome::Ok {
        result: ModuleQueryResult::Diagnostics(diags),
        ..
    } = call_service!(
        caps,
        session,
        process_module_query,
        &local,
        &ModuleQuery::Diagnostics,
        &strict,
        None; option_regression
    )
    .expect("local option")
    else {
        panic!("expected source diagnostics");
    };
    assert!(!diags.diagnostics().iter().any(|d| d.severity() == LeanSeverity::Error));
}

#[test]
fn module_options_reach_trials_and_reject_unknown_configuration() {
    use lean_rs_host::host::process::{
        ModuleQuery, ModuleQueryOutcome, ModuleQueryOutputBudgets, ModuleQueryResult, ProofAttemptOutcome,
        ProofAttemptRequest, ProofAttemptStatus, ProofCandidate, ProofEditTarget, ProofPositionSelector,
    };
    use lean_toolchain::LeanOptionValue;
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);
    let strict =
        LeanElabOptions::new().lean_options([("autoImplicit".to_owned(), LeanOptionValue::Bool(false))].into());
    let request = ProofAttemptRequest {
        source: "theorem trialOptions : True := by\n  trivial\n".to_owned(),
        edit: ProofEditTarget::Declaration {
            name: "trialOptions".to_owned(),
            position: ProofPositionSelector::Entry,
        },
        candidates: vec![ProofCandidate {
            id: "candidate".to_owned(),
            text: "have f (A : Type missingUniverse) := A\ntrivial".to_owned(),
        }],
        budgets: ModuleQueryOutputBudgets::default(),
    };
    let ProofAttemptOutcome::Ok { result, .. } =
        call_service!(caps, session, attempt_proof, &request, &strict, None; option_regression).expect("trial")
    else {
        panic!("expected trial result");
    };
    let row = result.candidates.first().expect("one candidate");
    assert_eq!(row.status, ProofAttemptStatus::Failed);
    assert!(
        row.diagnostics
            .diagnostics()
            .iter()
            .any(|d| d.message().contains("unknown universe level"))
    );
    let kernel = call_service!(
        caps,
        session,
        kernel_check,
        "def undeclaredUniverse (A : Type u) := A",
        &strict,
        None,
        None; option_regression
    )
    .expect("kernel check");
    assert!(matches!(kernel, LeanKernelOutcome::Rejected(_)));
    let invalid = LeanElabOptions::new()
        .lean_options([("unregistered.regression.option".to_owned(), LeanOptionValue::Bool(true))].into());
    assert!(
        call_service!(
            caps,
            session,
            process_module_query,
            "def x := 1",
            &ModuleQuery::Diagnostics,
            &invalid,
            None; option_regression
        )
        .is_err()
    );
    // Weak options must not override the host's explicit heartbeat budget.
    let bounded = LeanElabOptions::new().heartbeat_limit(12345).lean_options(
        [
            ("weak.maxHeartbeats".to_owned(), LeanOptionValue::Nat(0)),
            ("maxSynthPendingDepth".to_owned(), LeanOptionValue::Nat(3)),
            (
                "tactic.simp.user.exampleString".to_owned(),
                LeanOptionValue::String("option-value-probe".to_owned()),
            ),
        ]
        .into(),
    );
    let source = "import Lean\nrun_cmd do\n  let opts ← Lean.MonadOptions.getOptions\n  unless opts.get `maxHeartbeats (0 : Nat) == 12345 do throwError \"wrong heartbeat limit\"\n  unless opts.get `maxSynthPendingDepth (0 : Nat) == 3 do throwError \"wrong natural option\"\n  unless opts.get `tactic.simp.user.exampleString \"\" == \"option-value-probe\" do throwError \"wrong string option\"\n";
    let ModuleQueryOutcome::Ok {
        result: ModuleQueryResult::Diagnostics(diags),
        ..
    } = call_service!(
        caps,
        session,
        process_module_query,
        source,
        &ModuleQuery::Diagnostics,
        &bounded,
        None; option_regression
    )
    .expect("bounded query")
    else {
        panic!("expected diagnostics");
    };
    assert!(
        !diags.diagnostics().iter().any(|d| d.severity() == LeanSeverity::Error),
        "{diags:?}"
    );
}

#[test]
fn header_import_accounting_survives_snapshot_eviction_and_query_paths() {
    use lean_rs_host::host::process::{ModuleQuery, ModuleQueryOutputBudgets, ModuleQuerySelector};
    let host = LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("host opens cleanly");
    let caps = fixture::native_capabilities(&host).expect("caps");
    let imports = if fixture::native_fixture_selected() {
        fixture::HeldHostImport::HeaderAccounting.imports(&caps)
    } else {
        service_requests::imports(&caps, &["Lean"], LeanSessionImportProfile::default())
    };
    let mut session = caps.session(&imports, None, None).expect("session");
    let source = "module\nimport Lean\npublic def headerAccountingProbe : Nat := 1\n";
    let opts = LeanElabOptions::new().file_label("header-accounting.lean");
    session
        .configure_header_import_budget(0, 0)
        .expect("unlimited test budget");
    let before = session.header_import_state().expect("initial accounting");
    call_service!(
        caps,
        session,
        process_module_query,
        source,
        &ModuleQuery::Diagnostics,
        &opts,
        None; header_accounting
    )
    .expect("first query");
    let imported = session.header_import_state().expect("fresh accounting");
    assert_eq!(imported.imports, before.imports + 1);
    assert!(!imported.accounting_unknown);
    session.clear_module_snapshot_cache().expect("evict body snapshots");
    call_service!(
        caps,
        session,
        process_module_query_batch,
        source,
        &[ModuleQuerySelector::Diagnostics {
            id: "diagnostics".to_owned(),
        }],
        &ModuleQueryOutputBudgets::default(),
        &opts,
        None; header_accounting
    )
    .expect("other query path");
    let reused = session.header_import_state().expect("warm accounting");
    assert_eq!(reused.imports, imported.imports);
    assert_eq!(reused.non_memory_mapped_bytes, imported.non_memory_mapped_bytes);
    session.clear_module_header_cache().expect("evict session header");
    let cleared = session.header_import_state().expect("retained accounting");
    assert_eq!(cleared.non_memory_mapped_bytes, imported.non_memory_mapped_bytes);
    call_service!(
        caps,
        session,
        process_module_query,
        source,
        &ModuleQuery::Diagnostics,
        &opts,
        None; header_accounting
    )
    .expect("reimport");
    assert_eq!(
        session.header_import_state().expect("reimport accounting").imports,
        imported.imports + 1
    );
}
