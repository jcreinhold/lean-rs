#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

#[path = "../../lean-rs-host/tests/support/fixed_fixture.rs"]
mod fixture;

#[path = "support/ordinary_typed_selection.rs"]
mod ordinary_typed_selection;
#[path = "support/ordinary_typed_worker.rs"]
mod ordinary_typed_worker;
use ordinary_typed_selection::ordinary_typed_selection;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use lean_rs::LeanRuntime;
use lean_rs_host::{EvidenceStatus, LeanHost};
use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerConfig, LeanWorkerDeclarationFilter, LeanWorkerElabOptions,
    LeanWorkerError, LeanWorkerKernelStatus, LeanWorkerProcessTracker, LeanWorkerProgressEvent, LeanWorkerProgressSink,
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
    let fixture = fixture_root();
    lean_toolchain::LakeWorkspace::new(&fixture)
        .shared_library("LeanRsFixture")
        .expect("fixture Lake target builds");
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
}

#[test]
fn ordinary_worker_refuses_invalid_prepared_shims_before_handshake() {
    let result = Command::new(worker_binary())
        .env("LEAN_RS_PREPARED_HOST_ARTIFACTS", "{}")
        .output()
        .expect("run the ordinary worker");
    assert!(!result.status.success());
    assert!(result.stdout.is_empty(), "refused launch must not emit a handshake");
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing field `protocol`"));
}

fn handles_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        ordinary_typed_selection().map_or_else(fixture_root, |selected| selected.fixture.clone()),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsFixture.Handles"],
    )
}

fn declaration_worker() -> (LeanWorker, Option<LeanWorkerProcessTracker>) {
    if let Some(config) = ordinary_typed_worker::worker_config() {
        let tracker = LeanWorkerProcessTracker::new();
        let worker = LeanWorker::spawn(&config.process_tracker(tracker.clone())).expect("selected worker starts");
        (worker, Some(tracker))
    } else {
        ensure_fixture_built();
        (LeanWorker::spawn(&worker_config()).expect("worker starts"), None)
    }
}

fn stop_selected_worker(worker: LeanWorker, tracker: Option<LeanWorkerProcessTracker>) {
    if let Some(tracker) = tracker {
        let exit = worker.shutdown().expect("selected worker terminates").exit;
        assert!(exit.success, "selected worker must shut down cleanly");
        assert_eq!(tracker.active_processes(), 0, "all selected workers must be reaped");
    }
}

fn elaboration_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        ordinary_typed_selection().map_or_else(fixture_root, |selected| selected.fixture.clone()),
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
        fs::write(self.root.join(relative), content).expect("write temporary Lake project file");
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

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn direct_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture::fixture_root()).expect("host opens cleanly")
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<LeanWorkerProgressEvent>>,
}

impl RecordingSink {
    fn events(&self) -> Vec<LeanWorkerProgressEvent> {
        self.events.lock().expect("progress lock is not poisoned").clone()
    }
}

impl LeanWorkerProgressSink for RecordingSink {
    fn report(&self, event: LeanWorkerProgressEvent) {
        self.events.lock().expect("progress lock is not poisoned").push(event);
    }
}

struct CancelOnFirstProgress<'a> {
    token: &'a LeanWorkerCancellationToken,
    events: Mutex<Vec<LeanWorkerProgressEvent>>,
}

impl LeanWorkerProgressSink for CancelOnFirstProgress<'_> {
    fn report(&self, event: LeanWorkerProgressEvent) {
        self.events.lock().expect("progress lock is not poisoned").push(event);
        self.token.cancel();
    }
}

#[test]
fn declaration_bulk_adapter_matches_in_process_session() {
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
        "This.Name.Does.Not.Exist",
    ];

    let (mut worker, tracker) = declaration_worker();
    let host = if let Some(selected) = ordinary_typed_selection() {
        assert!(
            fixture::native_fixture_selected(),
            "parity requires the held direct-host fixture"
        );
        let root = fixture::native_fixture_root();
        assert_eq!(root, selected.fixture, "direct and worker fixtures differ");
        LeanHost::from_lake_project(runtime(), root).expect("host opens selected fixture")
    } else {
        direct_host()
    };
    let (caps, native) = if tracker.is_some() {
        (
            fixture::native_capabilities(&host).expect("selected capability loads"),
            true,
        )
    } else {
        (fixture::capabilities(&host).expect("fixture capability loads"), false)
    };
    let imports = if native {
        fixture::native_imports(&caps, "LeanRsFixture.Handles")
    } else {
        fixture::imports(&caps, "LeanRsFixture.Handles")
    };
    let mut direct = caps.session(&imports, None, None).expect("direct session opens");
    let direct_kinds = direct
        .declaration_kind_bulk(&names, None, None)
        .expect("direct kind bulk succeeds");
    let direct_names = direct
        .declaration_name_bulk(&names, None, None)
        .expect("direct name bulk succeeds");

    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");
    let worker_kinds = session
        .declaration_kinds(&names, None, None)
        .expect("worker kind bulk succeeds");
    let worker_names = session
        .declaration_names(&names, None, None)
        .expect("worker name bulk succeeds");

    assert_eq!(worker_kinds, direct_kinds);
    assert_eq!(worker_names, direct_names);
    stop_selected_worker(worker, tracker);
}

#[test]
fn shims_only_session_opens_without_user_dylib() {
    let mut temporary = None;
    let (project, config, tracker) = if let Some(selected) = ordinary_typed_selection::ordinary_scalar_selection() {
        let target = workspace_root().canonicalize().expect("canonical SDK").join("target");
        assert_eq!(
            selected.fixture,
            target.join("metadata-qualification/broken-import/lean-good")
        );
        assert_eq!(selected.worker, target.join("debug/metadata-worker"));
        let Some(prepared) = option_env!("LEAN_RS_PREPARED_HOST_ARTIFACTS") else {
            panic!("selected Good fixture requires its sealed shim descriptor");
        };
        let tracker = LeanWorkerProcessTracker::new();
        let config = LeanWorkerConfig::new(&selected.worker)
            .checked_native_loader_environment()
            .env("LEAN_RS_PREPARED_HOST_ARTIFACTS", prepared)
            .process_tracker(tracker.clone());
        (selected.fixture.clone(), config, Some(tracker))
    } else {
        let project = temporary.insert(TempLakeProject::new("shims-only"));
        project.write(
            "lakefile.lean",
            "import Lake\nopen Lake DSL\npackage worker_shims\nlean_lib Good\n",
        );
        project.write("Good.lean", "def goodValue : Nat := 41\n");
        project.lake_build_ok("Good");
        (project.path().to_path_buf(), worker_config(), None)
    };

    let mut worker = LeanWorker::spawn(&config).expect("worker starts");
    let mut session = worker
        .open_session(&LeanWorkerSessionConfig::shims_only(project, ["Good"]), None, None)
        .expect("shims-only worker session opens");
    let declarations = session
        .list_declarations_strings(&LeanWorkerDeclarationFilter::default(), None, None)
        .expect("list declarations works through shims-only worker session");

    assert!(
        declarations.iter().any(|name| name == "goodValue"),
        "worker shims-only session should see user oleans"
    );
    stop_selected_worker(worker, tracker);
}

#[test]
fn elaboration_and_kernel_adapter_match_status_shape() {
    let opts = LeanWorkerElabOptions::new();
    let (mut worker, tracker) = declaration_worker();
    let mut session = worker
        .open_session(&elaboration_session_config(), None, None)
        .expect("worker session opens");
    let ok = session
        .elaborate("(1 + 1 : Nat)", &opts, None, None)
        .expect("worker elaboration dispatch succeeds");
    let bad = session
        .elaborate("1 +", &opts, None, None)
        .expect("worker failed elaboration returns diagnostics");
    let checked = session
        .kernel_check("theorem lean_rs_worker_checked : 1 + 1 = 2 := rfl", &opts, None, None)
        .expect("worker kernel check dispatch succeeds");

    assert!(ok.success);
    assert!(!bad.success);
    assert!(!bad.diagnostics.is_empty(), "failed elaboration carries diagnostics");
    assert_eq!(checked.status, LeanWorkerKernelStatus::Checked);
    let summary = checked
        .summary
        .as_ref()
        .expect("Checked kernel result carries a proof summary");
    assert_eq!(summary.declaration_name, "lean_rs_worker_checked");
    assert_eq!(summary.kind, "theorem");
    assert!(
        summary.type_signature.contains("Nat") && summary.type_signature.contains("Eq"),
        "summary type_signature should mention the underlying Nat equality, got {:?}",
        summary.type_signature
    );

    let rejected = session
        .kernel_check("theorem lean_rs_worker_rejected : 1 + 1 = 3 := rfl", &opts, None, None)
        .expect("worker kernel check dispatch succeeds on rejected source");
    assert_eq!(rejected.status, LeanWorkerKernelStatus::Rejected);
    assert!(
        rejected.summary.is_none(),
        "Rejected kernel result must not carry a summary, got {:?}",
        rejected.summary
    );

    let selected = tracker.is_some();
    let host = if selected {
        LeanHost::from_lake_project(runtime(), fixture::native_fixture_root()).expect("selected host opens")
    } else {
        direct_host()
    };
    let caps = if selected {
        fixture::native_capabilities(&host)
    } else {
        fixture::capabilities(&host)
    }
    .expect("fixture capability loads");
    let imports = if selected {
        fixture::native_imports_for(
            &caps,
            &["LeanRsHostShims.Elaboration"],
            lean_rs_host::LeanSessionImportProfile::default(),
        )
    } else {
        fixture::imports(&caps, "LeanRsHostShims.Elaboration")
    };
    let mut direct = caps.session(&imports, None, None).expect("direct session opens");
    let theorem = if selected {
        let paths = fixture::native_elaboration_paths(&caps, &direct);
        let (environment, scope) = direct.source_context();
        #[allow(unsafe_code, reason = "one fixed theorem in the held elaboration fixture")]
        // SAFETY: only the existing curated theorem with default options can run
        // against these exact held elaboration imports.
        unsafe {
            caps.shim_capability().kernel_scope(
                environment,
                scope,
                paths,
                "theorem lean_rs_curated : 1 + 1 = 2 := rfl".into(),
                lean_rs_host::LeanElabOptions::new().source_options(),
            )
        }
        .expect("attest curated theorem")
    } else {
        fixture::curated_theorem(&caps, &direct)
    };
    let direct_status = direct
        .kernel_check(&theorem, None, None)
        .expect("direct kernel check succeeds")
        .status();
    assert_eq!(direct_status, EvidenceStatus::Checked);
    stop_selected_worker(worker, tracker);
}

#[test]
fn progress_events_cross_worker_boundary_in_order() {
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];
    let sink = RecordingSink::default();
    let (mut worker, tracker) = declaration_worker();
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let kinds = session
        .declaration_kinds(&names, None, Some(&sink))
        .expect("worker kind bulk with progress succeeds");

    assert_eq!(kinds.len(), names.len());
    let events = sink.events();
    assert_eq!(events.len(), names.len());
    for (idx, event) in events.iter().enumerate() {
        assert_eq!(event.phase, "declaration_kind_bulk");
        assert_eq!(event.current, u64::try_from(idx + 1).expect("idx fits u64"));
        assert_eq!(event.total, Some(names.len() as u64));
    }
    stop_selected_worker(worker, tracker);
}

#[test]
fn parent_can_cancel_long_worker_request_at_progress_boundary() {
    let token = LeanWorkerCancellationToken::new();
    let sink = CancelOnFirstProgress {
        token: &token,
        events: Mutex::new(Vec::new()),
    };
    let names = vec!["LeanRsFixture.Handles.nameAnonymous"; 20_000];
    let (mut worker, tracker) = declaration_worker();
    let mut session = worker
        .open_session(&handles_session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .declaration_kinds(&names, Some(&token), Some(&sink))
        .expect_err("parent cancellation should stop worker request");

    match err {
        LeanWorkerError::Cancelled { operation, .. } => assert_eq!(operation, "worker_declaration_kinds"),
        other => panic!("expected worker cancellation, got {other:?}"),
    }
    assert!(
        !sink.events.lock().expect("progress lock is not poisoned").is_empty(),
        "cancellation happens after at least one worker progress event",
    );
    stop_selected_worker(worker, tracker);
}
