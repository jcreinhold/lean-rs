//! Builder for worker-backed downstream capabilities.
//!
//! This module composes Lake target building, worker child resolution, worker
//! startup, session opening, and optional metadata validation. It deliberately
//! does not know downstream command names or row schemas.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lean_rs::LeanBuiltCapability;
use serde_json::Value;

use crate::pool::{LeanWorkerRestartPolicyClass, LeanWorkerSessionKey};
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerCapabilityMetadata, LeanWorkerProgressSink, LeanWorkerRuntimeMetadata,
    LeanWorkerSession, LeanWorkerSessionConfig,
};
use crate::supervisor::{
    LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING, LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerRestartPolicy,
};

const WORKER_CHILD_ENV: &str = "LEAN_RS_WORKER_CHILD";

/// Builder for a worker-backed Lean capability session.
///
/// The builder hides the common setup sequence for downstream tools:
///
/// 1. build the Lake shared-library target with `lean-toolchain`;
/// 2. resolve and start the `lean-rs-worker-child` process;
/// 3. health-check the worker;
/// 4. open the configured host session once; and
/// 5. optionally validate downstream capability metadata.
///
/// Callers still provide the Lake project root, package name, library target,
/// and imports because those are the downstream capability's identity. Worker
/// framing, child lifecycle, path probing, timeouts, and restart policy stay
/// behind the builder.
#[derive(Clone, Debug)]
pub struct LeanWorkerCapabilityBuilder {
    project_root: PathBuf,
    package: String,
    lib_name: String,
    imports: Vec<String>,
    built_dylib_path: Option<PathBuf>,
    worker_child: Option<LeanWorkerChild>,
    startup_timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
    metadata_check: Option<CapabilityMetadataCheck>,
}

impl LeanWorkerCapabilityBuilder {
    /// Create a builder for a Lake project and capability library.
    ///
    /// `project_root` is the directory containing `lakefile.lean`. `package`
    /// is the Lake package name used by `lean-rs-host`, and `lib_name` is the
    /// Lake `lean_lib` target to build and load.
    #[must_use]
    pub fn new(
        project_root: impl Into<PathBuf>,
        package: impl Into<String>,
        lib_name: impl Into<String>,
        imports: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            package: package.into(),
            lib_name: lib_name.into(),
            imports: imports.into_iter().map(Into::into).collect(),
            built_dylib_path: None,
            worker_child: None,
            startup_timeout: None,
            request_timeout: None,
            restart_policy: None,
            metadata_check: None,
        }
    }

    /// Create a builder from a build-script produced capability.
    ///
    /// The dylib path comes from `spec`; the Lake project root is inferred
    /// from the standard `.lake/build/lib/<dylib>` layout so the worker child
    /// can still initialize Lean's import search path.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the built dylib path cannot be resolved,
    /// the descriptor is missing package/module names, or the dylib is not
    /// under a standard Lake build directory.
    pub fn from_built_capability(
        spec: &LeanBuiltCapability,
        imports: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, LeanWorkerError> {
        let dylib_path = spec.dylib_path().map_err(|err| LeanWorkerError::Setup {
            message: err.to_string(),
        })?;
        let project_root = infer_lake_project_root_from_dylib(&dylib_path)?;
        let package = spec.package_name().ok_or_else(|| LeanWorkerError::Setup {
            message: "LeanBuiltCapability is missing the Lake package name; call `.package(...)`".to_owned(),
        })?;
        let module = spec.module_name().ok_or_else(|| LeanWorkerError::Setup {
            message: "LeanBuiltCapability is missing the root Lean module name; call `.module(...)`".to_owned(),
        })?;
        Ok(Self {
            project_root,
            package: package.to_owned(),
            lib_name: module.to_owned(),
            imports: imports.into_iter().map(Into::into).collect(),
            built_dylib_path: Some(dylib_path),
            worker_child: None,
            startup_timeout: None,
            request_timeout: None,
            restart_policy: None,
            metadata_check: None,
        })
    }

    /// Use an explicit `lean-rs-worker-child` executable.
    ///
    /// Tests and packaged applications should use this when the worker child
    /// is not discoverable beside the current executable.
    #[must_use]
    pub fn worker_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.worker_child = Some(LeanWorkerChild::path(path));
        self
    }

    /// Resolve the worker executable with a packaged worker-child locator.
    #[must_use]
    pub fn worker_child(mut self, child: LeanWorkerChild) -> Self {
        self.worker_child = Some(child);
        self
    }

    /// Set the maximum time to wait for worker startup.
    #[must_use]
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = Some(timeout);
        self
    }

    /// Set the maximum time to wait for one worker request.
    #[must_use]
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Use the documented long-running request timeout profile.
    #[must_use]
    pub fn long_running_requests(mut self) -> Self {
        self.request_timeout = Some(LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING);
        self
    }

    /// Set the worker restart policy used after startup.
    #[must_use]
    pub fn restart_policy(mut self, policy: LeanWorkerRestartPolicy) -> Self {
        self.restart_policy = Some(policy);
        self
    }

    /// Validate generic capability metadata after the session opens.
    ///
    /// The export must have ABI `String -> IO String`, matching
    /// `LeanWorkerSession::capability_metadata`. The returned metadata is
    /// stored on the opened capability for callers that need it.
    #[must_use]
    pub fn validate_metadata(mut self, export: impl Into<String>, request: Value) -> Self {
        self.metadata_check = Some(CapabilityMetadataCheck {
            export: export.into(),
            request,
            expected: None,
        });
        self
    }

    /// Validate that a capability metadata export returns the expected facts.
    ///
    /// This is the pool-facing metadata expectation hook. The metadata remains
    /// downstream-defined; `lean-rs-worker` only checks that the generic
    /// metadata envelope matches the caller's requested expectation.
    #[must_use]
    pub fn expect_metadata(
        mut self,
        export: impl Into<String>,
        request: Value,
        expected: LeanWorkerCapabilityMetadata,
    ) -> Self {
        self.metadata_check = Some(CapabilityMetadataCheck {
            export: export.into(),
            request,
            expected: Some(expected),
        });
        self
    }

    /// Return the session reuse key represented by this builder.
    ///
    /// The key is for worker-pool reuse only. It is not a downstream cache key
    /// and does not encode row schemas, ranking, reporting, or source
    /// provenance.
    #[must_use]
    pub fn session_key(&self) -> LeanWorkerSessionKey {
        let restart_policy_class = match &self.restart_policy {
            Some(policy) if policy == &LeanWorkerRestartPolicy::default() => LeanWorkerRestartPolicyClass::Default,
            Some(_policy) => LeanWorkerRestartPolicyClass::Custom,
            None => LeanWorkerRestartPolicyClass::Default,
        };
        let mut key = LeanWorkerSessionKey::new(
            self.project_root.clone(),
            self.package.clone(),
            self.lib_name.clone(),
            self.imports.clone(),
        )
        .restart_policy_class(restart_policy_class);
        if let Some(check) = &self.metadata_check {
            key = key.metadata_expectation(check.export.clone(), check.request.clone(), check.expected.clone());
        }
        key
    }

    pub(crate) fn pool_request_timeout(&self) -> Duration {
        self.request_timeout
            .unwrap_or(crate::supervisor::LEAN_WORKER_REQUEST_TIMEOUT_DEFAULT)
    }

    /// Build the Lake target, start the worker, open the session, and return a ready capability.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if Lake cannot build the target, the worker
    /// child cannot be resolved or spawned, the worker fails startup/health,
    /// the session cannot open, or metadata validation fails.
    pub fn open(self) -> Result<LeanWorkerCapability, LeanWorkerError> {
        let dylib_path = match self.built_dylib_path {
            Some(path) => path,
            None => lean_toolchain::build_lake_target_quiet(&self.project_root, &self.lib_name)
                .map_err(|diagnostic| LeanWorkerError::CapabilityBuild { diagnostic })?,
        };
        let worker_executable = self
            .worker_child
            .map_or_else(resolve_default_worker_executable, |child| child.resolve())?;

        let mut config = LeanWorkerConfig::new(worker_executable);
        if let Some(timeout) = self.startup_timeout {
            config = config.startup_timeout(timeout);
        }
        if let Some(timeout) = self.request_timeout {
            config = config.request_timeout(timeout);
        }
        if let Some(policy) = self.restart_policy {
            config = config.restart_policy(policy);
        }

        let mut worker = LeanWorker::spawn(&config)?;
        worker.health()?;

        let session_config = LeanWorkerSessionConfig::new(
            self.project_root.clone(),
            self.package.clone(),
            self.lib_name.clone(),
            self.imports.clone(),
        );

        let validated_metadata = {
            let mut session = worker.open_session(&session_config, None, None)?;
            match self.metadata_check {
                Some(check) => {
                    let metadata = session.capability_metadata(&check.export, &check.request, None, None)?;
                    if let Some(expected) = check.expected
                        && metadata != expected
                    {
                        return Err(LeanWorkerError::CapabilityMetadataMismatch {
                            export: check.export,
                            expected: Box::new(expected),
                            actual: Box::new(metadata),
                        });
                    }
                    Some(metadata)
                }
                None => None,
            }
        };

        Ok(LeanWorkerCapability {
            worker,
            session_config,
            dylib_path,
            validated_metadata,
        })
    }
}

/// A worker-backed capability with its Lake target built and worker started.
///
/// The value owns the worker supervisor and the session configuration. It is
/// the normal entry point for downstream capability use until the typed command
/// facade lands on top of it.
#[derive(Debug)]
pub struct LeanWorkerCapability {
    worker: LeanWorker,
    session_config: LeanWorkerSessionConfig,
    dylib_path: PathBuf,
    validated_metadata: Option<LeanWorkerCapabilityMetadata>,
}

impl LeanWorkerCapability {
    /// Open a worker session for this capability.
    ///
    /// The builder has already proved that the session can open. This method
    /// is still fallible because worker cycling, cancellation, or a child
    /// failure may require a fresh session.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child cannot open
    /// the configured imports, cancellation is already requested, a progress
    /// sink panics, or protocol communication fails.
    pub fn open_session(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerSession<'_>, LeanWorkerError> {
        self.worker.open_session(&self.session_config, cancellation, progress)
    }

    /// Return the built capability dylib path resolved by `lean-toolchain`.
    #[must_use]
    pub fn dylib_path(&self) -> &Path {
        &self.dylib_path
    }

    /// Return the session configuration used by this capability.
    #[must_use]
    pub fn session_config(&self) -> &LeanWorkerSessionConfig {
        &self.session_config
    }

    /// Return capability metadata validated by the builder, if requested.
    #[must_use]
    pub fn validated_metadata(&self) -> Option<&LeanWorkerCapabilityMetadata> {
        self.validated_metadata.as_ref()
    }

    /// Return protocol/runtime facts captured from the worker handshake.
    #[must_use]
    pub fn runtime_metadata(&self) -> LeanWorkerRuntimeMetadata {
        self.worker.runtime_metadata()
    }

    /// Borrow the underlying worker for lifecycle operations such as cycling.
    #[must_use]
    pub fn worker(&self) -> &LeanWorker {
        &self.worker
    }

    /// Mutably borrow the underlying worker for lifecycle operations such as cycling.
    #[must_use]
    pub fn worker_mut(&mut self) -> &mut LeanWorker {
        &mut self.worker
    }

    /// Terminate the worker child and return its exit status.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is already dead, the terminate
    /// request fails, or waiting for the child fails.
    pub fn terminate(self) -> Result<crate::supervisor::LeanWorkerExit, LeanWorkerError> {
        self.worker.terminate()
    }
}

#[derive(Clone, Debug)]
struct CapabilityMetadataCheck {
    export: String,
    request: Value,
    expected: Option<LeanWorkerCapabilityMetadata>,
}

/// Locator for an app-owned worker child executable.
///
/// Dependency binaries are not automatically installed with downstream
/// applications. Production apps should ship a tiny binary that calls
/// [`crate::run_worker_child_stdio`] and point the capability builder at it
/// through this locator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerChild {
    executable_name: Option<String>,
    explicit_path: Option<PathBuf>,
    env_var: Option<String>,
}

impl LeanWorkerChild {
    /// Locate a worker child beside the current executable, or beside the
    /// Cargo profile directory during tests and `cargo run`.
    #[must_use]
    pub fn sibling(executable_name: impl Into<String>) -> Self {
        Self {
            executable_name: Some(with_exe_suffix(executable_name.into())),
            explicit_path: None,
            env_var: None,
        }
    }

    /// Use an explicit worker child path.
    #[must_use]
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            executable_name: None,
            explicit_path: Some(path.into()),
            env_var: None,
        }
    }

    /// Add an environment-variable override for launchers and tests.
    #[must_use]
    pub fn env_override(mut self, env_var: impl Into<String>) -> Self {
        self.env_var = Some(env_var.into());
        self
    }

    fn resolve(&self) -> Result<PathBuf, LeanWorkerError> {
        let mut tried = Vec::new();
        if let Some(env_var) = &self.env_var
            && let Some(value) = env::var_os(env_var)
        {
            let path = PathBuf::from(value);
            if path.is_file() {
                return Ok(path);
            }
            tried.push(path);
            return Err(LeanWorkerError::WorkerChildUnresolved { tried });
        }
        if let Some(path) = &self.explicit_path {
            return Ok(path.clone());
        }

        let executable_name = self
            .executable_name
            .clone()
            .unwrap_or_else(|| with_exe_suffix("lean-rs-worker-child".to_owned()));
        tried.extend(candidate_sibling_worker_paths(&executable_name));
        if executable_name == with_exe_suffix("lean-rs-worker-child".to_owned())
            && let Some(path) = try_build_workspace_worker_child(&executable_name, &mut tried)
        {
            return Ok(path);
        }
        for path in dedup_paths(&tried) {
            if path.is_file() {
                return Ok(path);
            }
        }
        Err(LeanWorkerError::WorkerChildUnresolved { tried })
    }
}

impl Default for LeanWorkerChild {
    fn default() -> Self {
        Self::sibling("lean-rs-worker-child").env_override(WORKER_CHILD_ENV)
    }
}

fn resolve_default_worker_executable() -> Result<PathBuf, LeanWorkerError> {
    LeanWorkerChild::default().resolve()
}

fn candidate_sibling_worker_paths(executable_name: &str) -> Vec<PathBuf> {
    let mut tried = Vec::new();
    if let Ok(current_exe) = env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            tried.push(dir.join(executable_name));
        }
        if let Some(profile_dir) = current_exe.parent().and_then(Path::parent) {
            tried.push(profile_dir.join(executable_name));
        }
    }
    tried
}

fn with_exe_suffix(mut executable_name: String) -> String {
    if !env::consts::EXE_SUFFIX.is_empty() && !executable_name.ends_with(env::consts::EXE_SUFFIX) {
        executable_name.push_str(env::consts::EXE_SUFFIX);
    }
    executable_name
}

fn infer_lake_project_root_from_dylib(dylib_path: &Path) -> Result<PathBuf, LeanWorkerError> {
    let lib_dir = dylib_path.parent();
    let build_dir = lib_dir.and_then(Path::parent);
    let lake_dir = build_dir.and_then(Path::parent);
    let project_root = lake_dir.and_then(Path::parent);
    match (lib_dir, build_dir, lake_dir, project_root) {
        (Some(lib), Some(build), Some(lake), Some(root))
            if lib.file_name().is_some_and(|name| name == "lib")
                && build.file_name().is_some_and(|name| name == "build")
                && lake.file_name().is_some_and(|name| name == ".lake") =>
        {
            Ok(root.to_path_buf())
        }
        _ => Err(LeanWorkerError::Setup {
            message: format!(
                "built capability dylib '{}' is not under a standard .lake/build/lib directory",
                dylib_path.display()
            ),
        }),
    }
}

fn try_build_workspace_worker_child(executable_name: &str, tried: &mut Vec<PathBuf>) -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir.parent()?.parent()?;
    if !workspace
        .join("crates")
        .join("lean-rs-worker")
        .join("Cargo.toml")
        .is_file()
    {
        return None;
    }

    let debug = workspace.join("target").join("debug").join(executable_name);
    let release = workspace.join("target").join("release").join(executable_name);
    tried.push(debug.clone());
    tried.push(release.clone());
    if debug.is_file() {
        return Some(debug);
    }
    if release.is_file() {
        return Some(release);
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(workspace)
        .args(["build", "-p", "lean-rs-worker", "--bin", "lean-rs-worker-child"])
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    debug.is_file().then_some(debug)
}

fn dedup_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.iter().any(|existing| existing == path) {
            unique.push(path.clone());
        }
    }
    unique
}
