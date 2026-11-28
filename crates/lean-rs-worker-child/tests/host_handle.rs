//! Bootstrap and import tests for worker host handles and plain worker sessions.
#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerBootstrapDiagnosticCode, LeanWorkerChild, LeanWorkerConfig, LeanWorkerDeclarationFilter,
    LeanWorkerError, LeanWorkerHostHandleBuilder, LeanWorkerSessionConfig, LeanWorkerSessionImportProfile,
};

#[path = "support/checked_worker.rs"]
mod checked_worker;
#[path = "support/ordinary_interop_selection.rs"]
mod ordinary_interop_selection;

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

    fn lake_build_err(&self, target: &str) {
        let output = Command::new("lake")
            .arg("build")
            .arg(target)
            .current_dir(&self.root)
            .output()
            .expect("lake command starts");
        assert!(
            !output.status.success(),
            "`lake build {target}` unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
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
        concat!(
            r#"{"version":"1.2.0","packagesDir":".lake/packages","#,
            r#""packages":[{"type":"path","scope":"","name":"dep","manifestFile":"lake-manifest.json","#,
            r#""inherited":false,"dir":"./.lake/packages/dep","configFile":"lakefile.lean"}],"#,
            r#""name":"consumer","lakeDir":".lake"}"#,
        ),
    );
    project.lake_build_ok("Consumer");
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostBuilderSelection {
    #[serde(default)]
    case: Option<String>,
    project: PathBuf,
    worker: PathBuf,
    sysroot: PathBuf,
    worker_sha256: String,
}

fn checked_host_builder_selection(selection: &str, expected_case: Option<&str>) -> HostBuilderSelection {
    use sha2::{Digest, Sha256};
    let selected: HostBuilderSelection = serde_json::from_str(selection).expect("checked fixture selection");
    assert_eq!(
        selected.case.as_deref(),
        expected_case,
        "selected host builder test differs"
    );
    assert_eq!(
        selected.project.canonicalize().expect("canonical fixture"),
        selected.project
    );
    assert_eq!(
        selected.worker.canonicalize().expect("canonical worker"),
        selected.worker
    );
    use std::fmt::Write as _;
    use std::io::Read as _;
    const MAX_WORKER_BYTES: u64 = 256 * 1024 * 1024;
    let metadata = fs::symlink_metadata(&selected.worker).expect("selected worker metadata");
    assert!(metadata.is_file() && metadata.len() <= MAX_WORKER_BYTES);
    let mut file = fs::File::open(&selected.worker).expect("open selected worker");
    let opened = file.metadata().expect("opened worker metadata");
    assert!(opened.is_file() && opened.len() == metadata.len());
    let mut remaining = opened.len();
    let mut digest = Sha256::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let count = file.read(&mut chunk).expect("read selected worker");
        if count == 0 {
            break;
        }
        remaining = remaining.checked_sub(count as u64).expect("selected worker grew");
        digest.update(chunk.get(..count).expect("bytes read fit the buffer"));
    }
    assert_eq!(remaining, 0, "selected worker shrank");
    assert_eq!(file.metadata().expect("final worker metadata").len(), opened.len());
    let mut actual = String::new();
    for byte in digest.finalize() {
        write!(&mut actual, "{byte:02x}").expect("encode digest");
    }
    assert_eq!(actual, selected.worker_sha256, "selected worker changed");
    assert!(std::env::var_os("LEAN_PATH").is_none());
    selected
}

fn selected_host_builder_fixture(case: &str) -> Option<(PathBuf, LeanWorkerChild)> {
    let selection = option_env!("LEAN_RS_TEST_HOST_BUILDER")?;
    let selected = checked_host_builder_selection(selection, Some(case));
    assert_eq!(
        std::env::var_os("LEAN_RS_TEST_HOST_BUILDER_CASE").as_deref(),
        Some(std::ffi::OsStr::new(case)),
        "runtime launch must match the selected host builder test"
    );
    let name = match case {
        "a_host_handle_built_without_imports_opens_no_session" => "lean-lazy",
        "shims_only_host_handle_skips_user_shared_build" => "lean-shims-only",
        "shims_only_host_handle_imports_transitive_lake_package_oleans" => "lean-transitive",
        _ => panic!("unrecognized selected host builder test"),
    };
    let root = workspace_root()
        .canonicalize()
        .expect("canonical SDK checkout")
        .join("target");
    assert_eq!(
        selected.project,
        root.join("metadata-qualification/broken-import").join(name),
        "expected the exact sealed project for this host builder test"
    );
    assert_eq!(
        selected.worker,
        root.join("debug/metadata-worker"),
        "expected the exact fixture worker"
    );
    let child = LeanWorkerChild::for_toolchain(selected.worker, selected.sysroot).checked_native_loader_environment();
    Some((selected.project, child))
}

#[test]
fn shims_only_host_handle_skips_user_shared_build() {
    let mut temporary = None;
    let (project, child) =
        if let Some(selected) = selected_host_builder_fixture("shims_only_host_handle_skips_user_shared_build") {
            selected
        } else {
            let project = temporary.insert(TempLakeProject::new("host-handle-shims-only"));
            project.write(
                "lakefile.lean",
                "import Lake\nopen Lake DSL\npackage worker_host_handle\nlean_lib Demo\n",
            );
            project.write("Demo.lean", "import Demo.Good\nimport Demo.Broken\n");
            project.write("Demo/Good.lean", "def goodValue : Nat := 41\n");
            project.write("Demo/Broken.lean", "theorem broken : True := sorry_that_doesnt_exist\n");
            project.lake_build_ok("Demo.Good");
            project.lake_build_err("Demo:shared");
            (project.path().to_path_buf(), LeanWorkerChild::path(worker_binary()))
        };

    let mut handle = LeanWorkerHostHandleBuilder::shims_only(&project, ["Demo.Good"])
        .worker_child(child)
        .open()
        .expect("shims-only host handle opens without building Demo:shared");

    let mut session = handle
        .open_session_with_imports(["Demo.Good"], None, None)
        .expect("shims-only host handle imports a prebuilt user module");
    let declarations = session
        .list_declarations_strings(&LeanWorkerDeclarationFilter::default(), None, None)
        .expect("standard declaration service works through shims-only handle");

    assert!(
        declarations.iter().any(|name| name == "goodValue"),
        "shims-only host handle should see declarations from prebuilt user oleans"
    );

    match handle.open_session_with_imports(["Demo.Broken"], None, None) {
        Ok(_) => panic!("broken module import unexpectedly succeeded"),
        Err(LeanWorkerError::Worker { code, message }) => {
            assert_eq!(code, "lean_rs.lean_exception", "got worker error message: {message}");
        }
        Err(other) => panic!("expected LeanException worker error for broken import, got {other:?}"),
    }
}

/// A builder given no imports opens no session, and the child never imports one.
///
/// `open` used to import the builder's import set — empty or not — purely to
/// prove a session could open. For a caller that names its imports per call
/// that session is never reused, so the import was thrown away while its
/// unreclaimable Lean regions, one pooled session slot, and one of
/// `max_imports` stayed spent for the life of the child. A builder that *does*
/// name imports still opens eagerly, because then the session it prepares is
/// the one the caller's commands go on to reuse.
#[test]
fn a_host_handle_built_without_imports_opens_no_session() {
    let mut temporary = None;
    let (project, child) =
        if let Some(selected) = selected_host_builder_fixture("a_host_handle_built_without_imports_opens_no_session") {
            selected
        } else {
            let project = temporary.insert(TempLakeProject::new("host-handle-no-bootstrap-import"));
            project.write(
                "lakefile.lean",
                "import Lake\nopen Lake DSL\npackage worker_host_lazy\nlean_lib Demo\n",
            );
            project.write("Demo.lean", "import Demo.Good\n");
            project.write("Demo/Good.lean", "def goodValue : Nat := 41\n");
            project.lake_build_ok("Demo.Good");
            (project.path().to_path_buf(), LeanWorkerChild::path(worker_binary()))
        };

    let mut handle = LeanWorkerHostHandleBuilder::shims_only(&project, std::iter::empty::<String>())
        .worker_child(child.clone())
        .open()
        .expect("a shims-only handle with no imports opens");
    assert_eq!(
        handle.stats().imports,
        0,
        "opening the handle must not import; there is nothing to import yet"
    );

    // The caller's own first session is the first import, and it carries the
    // caller's imports rather than the builder's empty set.
    handle
        .open_session_with_imports(["Demo.Good"], None, None)
        .expect("the first real session opens");
    assert_eq!(
        handle.stats().imports,
        1,
        "the caller's first session should be the child's first and only import"
    );

    // `check` still opens one, which is what keeps deployment validation
    // available up front rather than deferred to the caller's first session.
    let report = LeanWorkerHostHandleBuilder::shims_only(project, std::iter::empty::<String>())
        .worker_child(child)
        .check();
    assert!(
        report.first_error().is_none(),
        "a healthy project reports no bootstrap error: {:?}",
        report.first_error()
    );
}

/// An import set the builder *was* given still fails at `open`, as before.
#[test]
fn a_host_handle_built_with_a_broken_import_still_fails_at_open() {
    let mut temporary = None;
    let (project, child) = if let Some(selection) = option_env!("LEAN_RS_TEST_BROKEN_IMPORT") {
        let selected = checked_host_builder_selection(selection, None);
        let bare_tracker = lean_rs_worker_parent::LeanWorkerProcessTracker::new();
        match LeanWorkerHostHandleBuilder::shims_only(&selected.project, ["Demo.Broken"])
            .worker_child(LeanWorkerChild::for_toolchain(worker_binary(), &selected.sysroot))
            .process_tracker(bare_tracker.clone())
            .open()
        {
            Err(LeanWorkerError::Worker { code, message }) => {
                assert_eq!(code, "lean_rs.module_init", "{message}");
                assert!(message.contains("application-installed worker authority"), "{message}");
            }
            _ => panic!("bare worker did not refuse the named import"),
        }
        assert_eq!(bare_tracker.active_processes(), 0, "bare worker must be reaped");
        let child = LeanWorkerChild::for_toolchain(selected.worker, selected.sysroot);
        (selected.project, child)
    } else {
        let project = temporary.insert(TempLakeProject::new("host-handle-broken-eager-import"));
        project.write(
            "lakefile.lean",
            "import Lake\nopen Lake DSL\npackage worker_host_broken\nlean_lib Demo\n",
        );
        project.write("Demo.lean", "import Demo.Broken\n");
        project.write("Demo/Broken.lean", "theorem broken : True := sorry_that_doesnt_exist\n");
        project.lake_build_err("Demo.Broken");
        (project.path().to_path_buf(), LeanWorkerChild::path(worker_binary()))
    };
    let tracker = lean_rs_worker_parent::LeanWorkerProcessTracker::new();
    let result = LeanWorkerHostHandleBuilder::shims_only(project, ["Demo.Broken"])
        .worker_child(child)
        .process_tracker(tracker.clone())
        .open();
    assert_eq!(tracker.active_processes(), 0, "failed import must retire its child");
    match result {
        Ok(_) => panic!("a named broken import should still fail while opening the handle"),
        Err(LeanWorkerError::Worker { code, message }) => {
            assert_eq!(code, "lean_rs.lean_exception", "got worker error message: {message}");
        }
        Err(other) => panic!("expected a LeanException worker error for the broken import, got {other:?}"),
    }
}

#[test]
fn shims_only_host_handle_imports_transitive_lake_package_oleans() {
    let mut temporary = None;
    let (project, child) = if let Some(selected) =
        selected_host_builder_fixture("shims_only_host_handle_imports_transitive_lake_package_oleans")
    {
        selected
    } else {
        let project = temporary.insert(TempLakeProject::new("host-handle-transitive-oleans"));
        write_transitive_dependency_fixture(project);
        (project.path().to_path_buf(), LeanWorkerChild::path(worker_binary()))
    };

    let mut handle = LeanWorkerHostHandleBuilder::shims_only(&project, ["Consumer"])
        .worker_child(child)
        .open()
        .expect("shims-only host handle opens over consumer");

    let mut session = handle
        .open_session_with_imports(["Consumer"], None, None)
        .expect("consumer imports dependency through worker");
    let declarations = session
        .list_declarations_strings(&LeanWorkerDeclarationFilter::default(), None, None)
        .expect("standard declaration service works through shims-only handle");
    assert!(
        declarations.iter().any(|name| name.starts_with("Dep.")),
        "dependency declaration should be visible through the worker shims-only handle"
    );

    match handle.open_session_with_imports(["Dep.NonExistent"], None, None) {
        Ok(_) => panic!("missing dependency module unexpectedly imported"),
        Err(LeanWorkerError::Worker { code, message }) => {
            assert_eq!(code, "lean_rs.lean_exception", "got worker error message: {message}");
            assert!(
                message.contains("Dep.NonExistent"),
                "missing module should be reported by Lean import, got: {message}"
            );
        }
        Err(other) => panic!("expected LeanException worker error for missing import, got {other:?}"),
    }
}

#[test]
fn missing_worker_child_is_a_bootstrap_error() {
    let missing = workspace_root().join("target").join("definitely-not-a-worker-child");
    let err = LeanWorkerHostHandleBuilder::shims_only(workspace_root(), std::iter::empty::<String>())
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
    let report = LeanWorkerHostHandleBuilder::shims_only(workspace_root(), std::iter::empty::<String>())
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
fn handshake_failure_is_a_bootstrap_diagnostic() {
    let shell = PathBuf::from("/bin/sh");
    if !shell.is_file() {
        return;
    }
    let report = LeanWorkerHostHandleBuilder::shims_only(workspace_root(), std::iter::empty::<String>())
        .worker_child(LeanWorkerChild::path(shell))
        .startup_timeout(Duration::from_millis(50))
        .check();
    let first = report.first_error().expect("non-worker child fails bootstrap");
    assert_eq!(
        first.code(),
        LeanWorkerBootstrapDiagnosticCode::WorkerHandshakeFailed,
        "{first:?}"
    );
}

fn sealed_interop_session() -> Option<(LeanWorkerConfig, LeanWorkerSessionConfig)> {
    ordinary_interop_selection::ordinary_interop_selection().map(|selected| {
        let worker = LeanWorkerConfig::new(&selected.worker)
            .checked_native_loader_environment()
            .env("LEAN_SYSROOT", selected.sysroot.as_os_str());
        let session = LeanWorkerSessionConfig::manifest_backed(
            &selected.fixture,
            "lean_rs_interop_consumer",
            "LeanRsInteropConsumer",
            &selected.capability_manifest,
            ["LeanRsInteropConsumer.Callback"],
        );
        (worker, session)
    })
}

#[test]
fn session_import_profile_reaches_child_import() {
    let (worker, default_config) = sealed_interop_session().unwrap_or_else(|| {
        (
            LeanWorkerConfig::new(worker_binary()),
            LeanWorkerSessionConfig::new(
                interop_root(),
                "lean_rs_interop_consumer",
                "LeanRsInteropConsumer",
                ["LeanRsInteropConsumer.Callback"],
            ),
        )
    });
    let mut worker = LeanWorker::spawn(&worker).expect("worker starts");
    let compat_config = default_config
        .clone()
        .with_import_profile(LeanWorkerSessionImportProfile::FullPrivateCompat);
    assert_eq!(
        compat_config
            .with_imports(["LeanRsInteropConsumer.Callback"])
            .import_profile(),
        LeanWorkerSessionImportProfile::FullPrivateCompat,
        "with_imports must preserve the selected import profile",
    );

    {
        let _session = worker
            .open_session(&default_config, None, None)
            .expect("default private worker session opens");
    }
    let default_stats = worker
        .stats()
        .last_import_stats
        .expect("default session reports import stats");
    assert_eq!(default_stats.import_level, "private");
    assert!(!default_stats.import_all);
    assert!(default_stats.load_exts);

    {
        let _session = worker
            .open_session(&compat_config, None, None)
            .expect("explicit full-private compatibility worker session opens");
    }
    let compat_stats = worker
        .stats()
        .last_import_stats
        .expect("compat session reports import stats");
    assert_eq!(compat_stats.import_level, "private");
    assert!(compat_stats.import_all);
    assert!(compat_stats.load_exts);
}
