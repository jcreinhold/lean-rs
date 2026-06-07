#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lean_rs::LeanBuiltCapability;
use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerBootstrapDiagnosticCode, LeanWorkerCapabilityBuilder, LeanWorkerCapabilityFact,
    LeanWorkerCapabilityMetadata, LeanWorkerChild, LeanWorkerCommandMetadata, LeanWorkerConfig,
    LeanWorkerDeclarationFilter, LeanWorkerError, LeanWorkerHostHandleBuilder, LeanWorkerJsonCommand, LeanWorkerPool,
    LeanWorkerPoolConfig, LeanWorkerRestartPolicy, LeanWorkerSessionConfig, LeanWorkerStreamingCommand,
    LeanWorkerTypedDataRow, LeanWorkerTypedDataSink,
};
use lean_rs_worker_protocol::worker_exports::fixture_mul_signature;
use lean_toolchain::CargoLeanCapability;
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

fn shipped_template_target_dir() -> PathBuf {
    static TARGET_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    TARGET_DIR
        .get_or_init(|| {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos();
            std::env::temp_dir().join(format!(
                "lean-rs-shipped-template-target-{}-{nonce}",
                std::process::id(),
            ))
        })
        .clone()
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
            .env("CARGO_TARGET_DIR", shipped_template_target_dir())
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
    let build_dir = shipped_template_target_dir().join("debug").join("build");
    let mut candidates = std::fs::read_dir(&build_dir)
        .expect("template build directory exists")
        .filter_map(Result::ok)
        .flat_map(|entry| {
            std::fs::read_dir(entry.path().join("out"))
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|candidate| candidate.path())
        })
        .filter(|path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("ShipLeanDemo") && name.ends_with(".lean-rs-capability.json"))
        })
        .filter(|path| {
            let bytes = std::fs::read(path).expect("read emitted capability manifest");
            let json: serde_json::Value = serde_json::from_slice(&bytes).expect("capability manifest is JSON");
            json.get("schema_version").and_then(serde_json::Value::as_u64)
                == Some(u64::from(lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION))
        })
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
    shipped_template_target_dir()
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
        r#"{"version":"1.2.0","packagesDir":".lake/packages","packages":[{"type":"path","scope":"","name":"dep","manifestFile":"lake-manifest.json","inherited":false,"dir":"./.lake/packages/dep","configFile":"lakefile.lean"}],"name":"consumer","lakeDir":".lake"}"#,
    );
    project.lake_build_ok("Consumer");
}

fn write_external_import_workspace_fixture(project: &TempLakeProject) {
    project.write(
        "lakefile.lean",
        "import Lake\nopen Lake DSL\npackage audited_workspace\nlean_lib B\n",
    );
    project.write("B.lean", "import B.Mod\n");
    project.write("B/Mod.lean", "def B.Mod.externalValue : Nat := 7\n");
    project.lake_build_ok("B.Mod");
}

fn external_import_builder() -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["B.Mod"],
    )
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_index")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_timeout_after_row")
    .streaming_command_export("lean_rs_interop_consumer_worker_shape_panic_after_row")
    .worker_executable(worker_binary())
}

#[derive(Default)]
struct CountingJsonRows {
    rows: Mutex<u64>,
}

impl CountingJsonRows {
    fn count(&self) -> u64 {
        *self.rows.lock().expect("row count lock is not poisoned")
    }
}

impl LeanWorkerTypedDataSink<serde_json::Value> for CountingJsonRows {
    fn report(&self, _row: LeanWorkerTypedDataRow<serde_json::Value>) {
        let mut rows = self.rows.lock().expect("row count lock is not poisoned");
        *rows = rows.saturating_add(1);
    }
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
fn shims_only_host_handle_skips_user_shared_build() {
    let project = TempLakeProject::new("host-handle-shims-only");
    project.write(
        "lakefile.lean",
        "import Lake\nopen Lake DSL\npackage worker_host_handle\nlean_lib Demo\n",
    );
    project.write("Demo.lean", "import Demo.Good\nimport Demo.Broken\n");
    project.write("Demo/Good.lean", "def goodValue : Nat := 41\n");
    project.write("Demo/Broken.lean", "theorem broken : True := sorry_that_doesnt_exist\n");
    project.lake_build_ok("Demo.Good");
    project.lake_build_err("Demo:shared");

    let mut handle = LeanWorkerHostHandleBuilder::shims_only(project.path(), ["Demo.Good"])
        .worker_executable(worker_binary())
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

#[test]
fn shims_only_host_handle_imports_transitive_lake_package_oleans() {
    let project = TempLakeProject::new("host-handle-transitive-oleans");
    write_transitive_dependency_fixture(&project);

    let mut handle = LeanWorkerHostHandleBuilder::shims_only(project.path(), ["Consumer"])
        .worker_executable(worker_binary())
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
fn manifest_capability_import_workspace_root_imports_external_workspace_only() {
    let project = TempLakeProject::new("capability-external-import-root");
    write_external_import_workspace_fixture(&project);

    let mut capability = external_import_builder()
        .import_workspace_root(project.path())
        .open()
        .expect("capability imports the external workspace module");
    let mut session = capability
        .open_session(None, None)
        .expect("external workspace session opens");
    let declarations = session
        .list_declarations_strings(&LeanWorkerDeclarationFilter::default(), None, None)
        .expect("standard declaration service sees external workspace");
    assert!(
        declarations.iter().any(|name| name == "B.Mod.externalValue"),
        "external import root should expose B.Mod declarations"
    );

    match external_import_builder().open() {
        Ok(_) => panic!("B.Mod unexpectedly imported from the capability project root"),
        Err(LeanWorkerError::Worker { code, message }) => {
            assert_eq!(code, "lean_rs.lean_exception", "got worker error message: {message}");
            assert!(
                message.contains("unknown module prefix 'B'"),
                "missing external module prefix should be named in the import error: {message}"
            );
        }
        Err(other) => panic!("expected Lean import failure without import root override, got {other:?}"),
    }

    match capability.open_session_with_imports(["LeanRsInteropConsumer.Callback"], None, None) {
        Ok(_) => panic!("capability project module leaked into the external import root"),
        Err(LeanWorkerError::Worker { code, message }) => {
            assert_eq!(code, "lean_rs.lean_exception", "got worker error message: {message}");
            assert!(
                message.contains("unknown module prefix 'LeanRsInteropConsumer'"),
                "leak-check import error should name the capability module prefix: {message}"
            );
        }
        Err(other) => panic!("expected Lean import failure for capability-project module, got {other:?}"),
    }
}

#[test]
fn pooled_manifest_capability_reuses_external_import_workspace_session() {
    let project = TempLakeProject::new("capability-external-import-root-pool");
    write_external_import_workspace_fixture(&project);
    let command = LeanWorkerStreamingCommand::<serde_json::Value, serde_json::Value, serde_json::Value>::new(
        "lean_rs_interop_consumer_worker_shape_index",
    );
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));

    let cold_started = Instant::now();
    {
        let mut lease = pool
            .acquire_lease(external_import_builder().import_workspace_root(project.path()))
            .expect("pool opens external import-root lease");
        let cold_elapsed = cold_started.elapsed();
        let imports_after_open = lease.snapshot().imports;
        let sink = CountingJsonRows::default();
        let summary = lease
            .run_streaming_command(
                &command,
                &json!({"source": "external-root-cold"}),
                &sink,
                None,
                None,
                None,
            )
            .expect("streaming command runs through pooled external-root lease");
        assert_eq!(summary.total_rows, sink.count());
        assert_eq!(
            lease.snapshot().imports,
            imports_after_open,
            "streaming command should attach to the already-open pool session"
        );
        println!(
            "workload=external_import_root_pooled_capability platform={}/{} capability_imports=1 audited_modules=1 cold_first_lease_ms={} workers={} imports_after_cold={}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            cold_elapsed.as_millis(),
            lease.snapshot().workers,
            lease.snapshot().imports,
        );
    }

    let imports_after_cold = pool.snapshot().imports;
    let warm_started = Instant::now();
    {
        let mut lease = pool
            .acquire_lease(external_import_builder().import_workspace_root(project.path().join(".")))
            .expect("pool reuses canonical-equivalent external import-root lease");
        let warm_elapsed = warm_started.elapsed();
        let sink = CountingJsonRows::default();
        let summary = lease
            .run_streaming_command(
                &command,
                &json!({"source": "external-root-warm"}),
                &sink,
                None,
                None,
                None,
            )
            .expect("streaming command runs through warm external-root lease");
        assert_eq!(summary.total_rows, sink.count());
        assert_eq!(
            lease.snapshot().imports,
            imports_after_cold,
            "warm same-workspace lease should not reopen the imported session"
        );
        println!(
            "workload=external_import_root_pooled_capability warm_second_lease_ms={} workers={} imports_after_warm={}",
            warm_elapsed.as_millis(),
            lease.snapshot().workers,
            lease.snapshot().imports,
        );
    }

    let snapshot = pool.snapshot();
    assert_eq!(snapshot.workers, 1);
    assert_eq!(snapshot.warm_leases, 1);
    assert_eq!(snapshot.imports, imports_after_cold);
    println!(
        "workload=external_import_root_pooled_capability final_workers={} final_warm_leases={} final_imports={}",
        snapshot.workers, snapshot.warm_leases, snapshot.imports,
    );
}

#[test]
fn external_import_workspace_root_preserves_timeout_and_fatal_errors() {
    let project = TempLakeProject::new("capability-external-import-root-failures");
    write_external_import_workspace_fixture(&project);
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));

    {
        let mut lease = pool
            .acquire_lease(external_import_builder().import_workspace_root(project.path()))
            .expect("pool opens external import-root lease");
        lease
            .set_request_timeout(Duration::from_millis(50))
            .expect("request timeout override is accepted");
        let command = LeanWorkerStreamingCommand::<serde_json::Value, serde_json::Value, serde_json::Value>::new(
            "lean_rs_interop_consumer_worker_shape_timeout_after_row",
        );
        let sink = CountingJsonRows::default();
        let err = lease
            .run_streaming_command(
                &command,
                &json!({"source": "external-root-timeout"}),
                &sink,
                None,
                None,
                None,
            )
            .expect_err("timeout fixture should preserve existing timeout semantics");
        match err {
            LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "worker_run_data_stream"),
            other => panic!("expected timeout error, got {other:?}"),
        }
        assert!(!lease.is_valid(), "timeout should invalidate the current lease");
    }

    {
        let mut lease = pool
            .acquire_lease(external_import_builder().import_workspace_root(project.path()))
            .expect("pool reacquires external import-root lease after timeout");
        let command = LeanWorkerStreamingCommand::<serde_json::Value, serde_json::Value, serde_json::Value>::new(
            "lean_rs_interop_consumer_worker_shape_panic_after_row",
        );
        let sink = CountingJsonRows::default();
        let err = lease
            .run_streaming_command(
                &command,
                &json!({"source": "external-root-fatal"}),
                &sink,
                None,
                None,
                None,
            )
            .expect_err("fatal fixture should preserve existing fatal-exit semantics");
        match err {
            LeanWorkerError::ChildPanicOrAbort { exit } => assert!(!exit.success),
            other => panic!("expected fatal child exit, got {other:?}"),
        }
        assert!(
            !lease.is_valid(),
            "fatal child exit should invalidate the current lease"
        );
    }
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
            assert_eq!(code, "lean_rs.worker.checked_binding");
            assert!(
                message.contains("lean_rs_interop_consumer_worker_metadata_missing"),
                "message should name missing export, got {message}",
            );
        }
        other => panic!("expected worker symbol lookup error, got {other:?}"),
    }
}

#[test]
fn manifest_missing_worker_export_signature_fails_before_dispatch() {
    let built = CargoLeanCapability::new(interop_root(), "LeanRsInteropConsumer")
        .package("lean_rs_interop_consumer")
        .module("LeanRsInteropConsumer")
        .build_quiet()
        .expect("interop capability manifest builds without worker command signatures");

    let mut worker = LeanWorker::spawn(&LeanWorkerConfig::new(worker_binary())).expect("worker starts");
    let config = LeanWorkerSessionConfig::manifest_backed(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        built.manifest_path(),
        ["LeanRsInteropConsumer.Callback"],
    );
    let mut session = worker
        .open_session(&config, None, None)
        .expect("manifest-backed session opens");
    let command = LeanWorkerJsonCommand::<serde_json::Value, serde_json::Value>::new(
        "lean_rs_interop_consumer_worker_json_command",
    );

    let err = session
        .run_json_command(&command, &json!({"caller": "missing-signature-test"}), None, None)
        .expect_err("missing manifest signature should fail before invoking Lean");

    match err {
        LeanWorkerError::Worker { code, message } => {
            assert_eq!(code, "lean_rs.worker.checked_binding");
            assert!(
                message.contains("lean_rs_interop_consumer_worker_json_command"),
                "diagnostic should name missing export: {message}",
            );
            assert!(
                message.contains("json command String -> IO String"),
                "diagnostic should name operation shape: {message}",
            );
        }
        other => panic!("expected checked binding worker error, got {other:?}"),
    }
}

#[test]
fn manifest_wrong_worker_export_signature_fails_before_dispatch() {
    let built = CargoLeanCapability::new(interop_root(), "LeanRsInteropConsumer")
        .package("lean_rs_interop_consumer")
        .module("LeanRsInteropConsumer")
        .export_signature(fixture_mul_signature("lean_rs_interop_consumer_worker_json_command"))
        .build_quiet()
        .expect("interop capability manifest builds with intentionally wrong worker command signature");

    let mut worker = LeanWorker::spawn(&LeanWorkerConfig::new(worker_binary())).expect("worker starts");
    let config = LeanWorkerSessionConfig::manifest_backed(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        built.manifest_path(),
        ["LeanRsInteropConsumer.Callback"],
    );
    let mut session = worker
        .open_session(&config, None, None)
        .expect("manifest-backed session opens");
    let command = LeanWorkerJsonCommand::<serde_json::Value, serde_json::Value>::new(
        "lean_rs_interop_consumer_worker_json_command",
    );

    let err = session
        .run_json_command(&command, &json!({"caller": "wrong-signature-test"}), None, None)
        .expect_err("wrong manifest signature should fail before invoking Lean");

    match err {
        LeanWorkerError::Worker { code, message } => {
            assert_eq!(code, "lean_rs.worker.checked_binding");
            assert!(
                message.contains("lean_rs_interop_consumer_worker_json_command"),
                "diagnostic should name wrong-signature export: {message}",
            );
            assert!(
                message.contains("json command String -> IO String"),
                "diagnostic should name expected operation shape: {message}",
            );
        }
        other => panic!("expected checked binding worker error, got {other:?}"),
    }
}

#[test]
fn no_manifest_worker_session_rejects_worker_command_as_checked_binding() {
    let mut worker = LeanWorker::spawn(&LeanWorkerConfig::new(worker_binary())).expect("worker starts");
    let config = LeanWorkerSessionConfig::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    );
    let mut session = worker.open_session(&config, None, None).expect("legacy session opens");
    let command = LeanWorkerJsonCommand::<serde_json::Value, serde_json::Value>::new(
        "lean_rs_interop_consumer_worker_json_command",
    );

    let err = session
        .run_json_command(&command, &json!({"caller": "no-manifest-test"}), None, None)
        .expect_err("worker command without manifest identity should fail before unchecked dispatch");

    match err {
        LeanWorkerError::Worker { code, message } => {
            assert_eq!(code, "lean_rs.worker.checked_binding");
            assert!(
                message.contains("lean_rs_interop_consumer_worker_json_command"),
                "diagnostic should name requested export: {message}",
            );
            assert!(
                message.contains("json command String -> IO String"),
                "diagnostic should name operation shape: {message}",
            );
        }
        other => panic!("expected checked binding worker error, got {other:?}"),
    }
}

#[test]
fn restart_policy_override_is_applied_during_builder_startup() {
    let capability = builder()
        .restart_policy(LeanWorkerRestartPolicy::default().max_requests(1))
        .open()
        .expect("builder opens capability with restart policy");

    let stats = capability.stats();
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
