//! Builders for worker-backed downstream capabilities and host sessions.
//!
//! This module composes worker child resolution, worker startup, and session
//! opening. User-export capabilities also build a Lake shared-library
//! target and may validate downstream metadata. Shim-backed host sessions skip
//! that user dylib path entirely and use only the bundled host services.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationInspectionRequest, LeanWorkerDeclarationInspectionResult,
    LeanWorkerDeclarationSearch, LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationVerificationRequest,
    LeanWorkerDeclarationVerificationResult, LeanWorkerElabOptions, LeanWorkerModuleQuery,
    LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryOutcome, LeanWorkerModuleQuerySelector,
    LeanWorkerOutputBudgets, LeanWorkerProofAttemptRequest, LeanWorkerProofAttemptResult,
};
use lean_rs_worker_protocol::worker_exports::{
    doctor_signature, json_command_signature, metadata_signature, streaming_command_signature,
};
use lean_toolchain::{LeanBuiltCapability, LeanExportSignature, LeanLoaderDiagnosticCode};
use serde::Deserialize;
use serde_json::Value;

use crate::pool::{LeanWorkerRestartPolicyClass, LeanWorkerSessionKey};
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerProgressSink, LeanWorkerRuntimeMetadata, LeanWorkerSession,
    LeanWorkerSessionConfig,
};
use crate::supervisor::{
    LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING, LeanWorker, LeanWorkerConfig, LeanWorkerError, LeanWorkerRestartPolicy,
    LeanWorkerRestartReason, LeanWorkerStats, LeanWorkerStatus,
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
///
/// Use [`LeanWorkerHostHandleBuilder::shims_only`] for tools that only need
/// the bundled Meta, elaboration, kernel, declaration, and info-tree services.
/// That path does not build or load the user's `:shared` facet and therefore
/// keeps working when unrelated user modules break the shared library build.
#[derive(Clone, Debug)]
pub struct LeanWorkerCapabilityBuilder {
    project_root: PathBuf,
    package: String,
    lib_name: String,
    imports: Vec<String>,
    built_dylib_path: Option<PathBuf>,
    built_manifest_path: Option<PathBuf>,
    built_capability: Option<LeanBuiltCapability>,
    worker_child: Option<LeanWorkerChild>,
    startup_timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
    module_cache_limits: Option<LeanWorkerModuleCacheLimits>,
    metadata_check: Option<CapabilityMetadataCheck>,
    max_frame_bytes: Option<u32>,
    worker_export_signatures: Vec<LeanExportSignature>,
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
            built_manifest_path: None,
            built_capability: None,
            worker_child: None,
            startup_timeout: None,
            request_timeout: None,
            restart_policy: None,
            module_cache_limits: None,
            metadata_check: None,
            max_frame_bytes: None,
            worker_export_signatures: Vec::new(),
        }
    }

    /// Create a builder from a build-script produced capability.
    ///
    /// Manifest-backed descriptors are the canonical packaged-app path. The
    /// builder reads package, module, and primary dylib facts from the
    /// manifest, then infers the Lake project root from the standard
    /// `.lake/build/lib/<dylib>` layout so the worker child can initialize
    /// Lean's import search path. Direct dylib descriptors remain supported as
    /// a compatibility path when callers also provide package and module names.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if manifest data cannot be parsed, the
    /// fallback dylib path cannot be resolved, the compatibility descriptor is
    /// missing package/module names, or the dylib is not under a standard Lake
    /// build directory.
    pub fn from_built_capability(
        spec: &LeanBuiltCapability,
        imports: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, LeanWorkerError> {
        let artifact = WorkerCapabilityArtifact::from_built_capability(spec)?;
        let project_root = infer_lake_project_root_from_dylib(&artifact.dylib_path)?;
        Ok(Self {
            project_root,
            package: artifact.package,
            lib_name: artifact.module,
            imports: imports.into_iter().map(Into::into).collect(),
            built_dylib_path: Some(artifact.dylib_path),
            built_manifest_path: artifact.manifest_path,
            built_capability: Some(spec.clone()),
            worker_child: None,
            startup_timeout: None,
            request_timeout: None,
            restart_policy: None,
            module_cache_limits: None,
            metadata_check: None,
            max_frame_bytes: None,
            worker_export_signatures: Vec::new(),
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

    /// Set typed limits for the worker child's module snapshot cache.
    ///
    /// These are deliberately not exposed as a generic child-env passthrough:
    /// the cache knobs are part of the worker lifecycle contract, and callers
    /// should not need to know the child process's environment-variable names.
    #[must_use]
    pub fn module_cache_limits(mut self, limits: LeanWorkerModuleCacheLimits) -> Self {
        self.module_cache_limits = Some(limits);
        self
    }

    /// Set the per-frame byte cap negotiated with the worker child at handshake.
    ///
    /// See [`LeanWorkerConfig::max_frame_bytes`] for the policy and the
    /// `[MIN_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP]` clamp. Raise this for
    /// capabilities whose single logical result composes into one frame
    /// (e.g. an outline of an entire module, a file-scoped diagnostics
    /// snapshot) and would otherwise trip `FrameTooLarge`.
    #[must_use]
    pub fn max_frame_bytes(mut self, max_frame_bytes: u32) -> Self {
        self.max_frame_bytes = Some(max_frame_bytes);
        self
    }

    /// Validate generic capability metadata after the session opens.
    ///
    /// The export must have ABI `String -> IO String`, matching
    /// `LeanWorkerSession::capability_metadata`. The returned metadata is
    /// stored on the opened capability for callers that need it.
    #[must_use]
    pub fn validate_metadata(mut self, export: impl Into<String>, request: Value) -> Self {
        let export = export.into();
        self.add_worker_export_signature(metadata_signature(export.clone()));
        self.metadata_check = Some(CapabilityMetadataCheck {
            export,
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
        let export = export.into();
        self.add_worker_export_signature(metadata_signature(export.clone()));
        self.metadata_check = Some(CapabilityMetadataCheck {
            export,
            request,
            expected: Some(expected),
        });
        self
    }

    /// Trust one manifest-backed metadata export with ABI `String -> IO String`.
    #[must_use]
    pub fn metadata_export(mut self, export: impl Into<String>) -> Self {
        self.add_worker_export_signature(metadata_signature(export));
        self
    }

    /// Trust one manifest-backed doctor export with ABI `String -> IO String`.
    #[must_use]
    pub fn doctor_export(mut self, export: impl Into<String>) -> Self {
        self.add_worker_export_signature(doctor_signature(export));
        self
    }

    /// Trust one manifest-backed JSON command export with ABI `String -> IO String`.
    #[must_use]
    pub fn json_command_export(mut self, export: impl Into<String>) -> Self {
        self.add_worker_export_signature(json_command_signature(export));
        self
    }

    /// Trust one manifest-backed streaming command export with ABI `String, USize, USize -> IO UInt8`.
    #[must_use]
    pub fn streaming_command_export(mut self, export: impl Into<String>) -> Self {
        self.add_worker_export_signature(streaming_command_signature(export));
        self
    }

    fn add_worker_export_signature(&mut self, signature: LeanExportSignature) {
        if self
            .worker_export_signatures
            .iter()
            .all(|existing| existing.symbol() != signature.symbol())
        {
            self.worker_export_signatures.push(signature);
        }
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

    /// Check deployment facts before running a real worker command.
    ///
    /// The report validates the worker child locator, manifest-backed
    /// capability artifact when present, worker protocol handshake, session
    /// opening, and optional metadata expectation. It keeps child paths,
    /// protocol frames, and loader environment details below the worker
    /// boundary.
    #[must_use]
    pub fn check(&self) -> LeanWorkerBootstrapReport {
        let mut checks = self.bootstrap_static_checks();
        if checks.iter().any(LeanWorkerBootstrapCheck::is_error) {
            return LeanWorkerBootstrapReport::new(checks);
        }

        match self.clone().open_unchecked() {
            Ok(capability) => {
                drop(capability.terminate());
            }
            Err(err) => checks.push(check_from_open_error(&err)),
        }
        LeanWorkerBootstrapReport::new(checks)
    }

    fn bootstrap_static_checks(&self) -> Vec<LeanWorkerBootstrapCheck> {
        let mut checks = Vec::new();
        checks.extend(worker_child_static_checks(self.worker_child.as_ref()));

        if let Some(spec) = &self.built_capability
            && let Ok(manifest_path) = spec.resolved_manifest_path()
        {
            let report = lean_toolchain::manifest_validation::check_static(&manifest_path);
            for check in report.errors() {
                checks.push(LeanWorkerBootstrapCheck::error(
                    LeanWorkerBootstrapDiagnosticCode::CapabilityPreflight { code: check.code() },
                    check.subject().to_owned(),
                    check.message().to_owned(),
                    check.repair_hint().to_owned(),
                ));
            }
        }
        checks
    }

    /// Build the Lake target, start the worker, open the session, and return a ready capability.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if Lake cannot build the target, the worker
    /// child cannot be resolved or spawned, the worker fails startup/health,
    /// the session cannot open, or metadata validation fails.
    pub fn open(self) -> Result<LeanWorkerCapability, LeanWorkerError> {
        let report = self.bootstrap_static_report();
        if let Some(check) = report.first_error() {
            return Err(LeanWorkerError::Bootstrap {
                code: check.code(),
                message: check.message().to_owned(),
            });
        }
        self.open_unchecked()
    }

    fn bootstrap_static_report(&self) -> LeanWorkerBootstrapReport {
        LeanWorkerBootstrapReport::new(self.bootstrap_static_checks())
    }

    fn open_unchecked(self) -> Result<LeanWorkerCapability, LeanWorkerError> {
        let (dylib_path, manifest_path) = match (self.built_dylib_path, self.built_manifest_path) {
            (Some(dylib_path), Some(manifest_path)) => (dylib_path, manifest_path),
            (_, None) => {
                let mut builder = lean_toolchain::CargoLeanCapability::new(&self.project_root, &self.lib_name)
                    .package(&self.package)
                    .module(&self.lib_name);
                for signature in self.worker_export_signatures {
                    builder = builder.export_signature(signature);
                }
                let built = builder
                    .build_quiet()
                    .map_err(|diagnostic| LeanWorkerError::CapabilityBuild { diagnostic })?;
                (built.dylib_path().to_path_buf(), built.manifest_path().to_path_buf())
            }
            (None, Some(manifest_path)) => {
                let artifact = WorkerCapabilityArtifact::from_manifest(&manifest_path)?;
                (artifact.dylib_path, manifest_path)
            }
        };
        let mut worker = spawn_checked_worker(
            self.worker_child,
            self.startup_timeout,
            self.request_timeout,
            self.restart_policy,
            self.module_cache_limits,
            self.max_frame_bytes,
        )?;

        let session_config = LeanWorkerSessionConfig::manifest_backed(
            self.project_root.clone(),
            self.package.clone(),
            self.lib_name.clone(),
            manifest_path,
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

/// Builder for a worker-backed host session that loads only bundled shims.
///
/// This is the bootstrap path for tools that use the standard host services
/// exposed through `lean-rs-host`: Meta queries, elaboration, kernel checking,
/// declaration listing, source ranges, and info trees. It deliberately has no
/// package/library fields and no metadata validation hook because no user
/// `@[export]` dylib is built or opened.
#[derive(Clone, Debug)]
pub struct LeanWorkerHostHandleBuilder {
    project_root: PathBuf,
    imports: Vec<String>,
    worker_child: Option<LeanWorkerChild>,
    startup_timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
    module_cache_limits: Option<LeanWorkerModuleCacheLimits>,
    max_frame_bytes: Option<u32>,
}

/// Typed limits for the worker child's module snapshot cache.
///
/// The worker child still receives these values as environment variables at
/// launch time, but the public API names the lifecycle policy rather than the
/// transport mechanism. A field left unset uses the child default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LeanWorkerModuleCacheLimits {
    max_entries: Option<u64>,
    ttl_millis: Option<u64>,
    max_bytes: Option<u64>,
    rss_guard_kib: Option<u64>,
}

impl LeanWorkerModuleCacheLimits {
    /// Set the maximum retained cache entries.
    #[must_use]
    pub fn max_entries(mut self, max_entries: u64) -> Self {
        self.max_entries = Some(max_entries.max(1));
        self
    }

    /// Set the time-to-live for retained module snapshots.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl_millis = Some(u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1));
        self
    }

    /// Set the approximate retained-cache byte ceiling.
    #[must_use]
    pub fn max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = Some(max_bytes.max(1));
        self
    }

    /// Set the child RSS guard above which the child clears retained snapshots
    /// before cacheable module-query requests.
    #[must_use]
    pub fn rss_guard_kib(mut self, rss_guard_kib: u64) -> Self {
        self.rss_guard_kib = Some(rss_guard_kib.max(1));
        self
    }
}

impl LeanWorkerHostHandleBuilder {
    /// Create a shims-only worker host-session builder for a Lake project.
    ///
    /// `project_root` is the directory containing `lakefile.lean`. `imports`
    /// are the modules to import when the builder performs its initial session
    /// open. The builder does not build a Lake `:shared` target.
    #[must_use]
    pub fn shims_only(project_root: impl Into<PathBuf>, imports: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            project_root: project_root.into(),
            imports: imports.into_iter().map(Into::into).collect(),
            worker_child: None,
            startup_timeout: None,
            request_timeout: None,
            restart_policy: None,
            module_cache_limits: None,
            max_frame_bytes: None,
        }
    }

    /// Use an explicit `lean-rs-worker-child` executable.
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

    /// Set typed limits for the worker child's module snapshot cache.
    ///
    /// These are applied when the worker child is spawned. They are scoped to
    /// this handle and do not mutate process-global environment variables.
    #[must_use]
    pub fn module_cache_limits(mut self, limits: LeanWorkerModuleCacheLimits) -> Self {
        self.module_cache_limits = Some(limits);
        self
    }

    /// Set the per-frame byte cap negotiated with the worker child at handshake.
    #[must_use]
    pub fn max_frame_bytes(mut self, max_frame_bytes: u32) -> Self {
        self.max_frame_bytes = Some(max_frame_bytes);
        self
    }

    /// Check worker bootstrap facts before running a real command.
    ///
    /// The report validates the worker child locator, protocol handshake, and
    /// shims-only session opening. It never builds a user shared-library target.
    #[must_use]
    pub fn check(&self) -> LeanWorkerBootstrapReport {
        let mut checks = self.bootstrap_static_checks();
        if checks.iter().any(LeanWorkerBootstrapCheck::is_error) {
            return LeanWorkerBootstrapReport::new(checks);
        }

        match self.clone().open_unchecked() {
            Ok(handle) => {
                drop(handle.terminate());
            }
            Err(err) => checks.push(check_from_open_error(&err)),
        }
        LeanWorkerBootstrapReport::new(checks)
    }

    /// Start the worker, open a shims-only host session once, and return a ready handle.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker child cannot be resolved or
    /// spawned, startup/health fails, or the shims-only session cannot open.
    /// This method does not build a user Lake shared-library target.
    pub fn open(self) -> Result<LeanWorkerHostHandle, LeanWorkerError> {
        let report = self.bootstrap_static_report();
        if let Some(check) = report.first_error() {
            return Err(LeanWorkerError::Bootstrap {
                code: check.code(),
                message: check.message().to_owned(),
            });
        }
        self.open_unchecked()
    }

    fn bootstrap_static_report(&self) -> LeanWorkerBootstrapReport {
        LeanWorkerBootstrapReport::new(self.bootstrap_static_checks())
    }

    fn bootstrap_static_checks(&self) -> Vec<LeanWorkerBootstrapCheck> {
        worker_child_static_checks(self.worker_child.as_ref())
    }

    fn open_unchecked(self) -> Result<LeanWorkerHostHandle, LeanWorkerError> {
        let mut worker = spawn_checked_worker(
            self.worker_child,
            self.startup_timeout,
            self.request_timeout,
            self.restart_policy,
            self.module_cache_limits,
            self.max_frame_bytes,
        )?;
        let session_config = LeanWorkerSessionConfig::shims_only(self.project_root, self.imports);
        {
            let _session = worker.open_session(&session_config, None, None)?;
        }
        Ok(LeanWorkerHostHandle { worker, session_config })
    }
}

/// Stable worker bootstrap diagnostic codes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LeanWorkerBootstrapDiagnosticCode {
    /// The worker child locator did not resolve to a file.
    WorkerChildUnresolved,
    /// The worker child exists but is not executable.
    WorkerChildNotExecutable,
    /// Manifest-backed capability preflight reported a loader/artifact issue.
    CapabilityPreflight { code: LeanLoaderDiagnosticCode },
    /// The worker child did not complete the protocol handshake.
    WorkerHandshakeFailed,
    /// Capability metadata did not match the caller's expectation.
    CapabilityMetadataMismatch,
    /// Worker bootstrap failed for a reason outside the named deployment checks.
    WorkerStartupFailed,
}

impl LeanWorkerBootstrapDiagnosticCode {
    /// Stable string identifier suitable for logs and support reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkerChildUnresolved => "lean_rs.worker.bootstrap.child_unresolved",
            Self::WorkerChildNotExecutable => "lean_rs.worker.bootstrap.child_not_executable",
            Self::CapabilityPreflight { code } => code.as_str(),
            Self::WorkerHandshakeFailed => "lean_rs.worker.bootstrap.handshake_failed",
            Self::CapabilityMetadataMismatch => "lean_rs.worker.bootstrap.metadata_mismatch",
            Self::WorkerStartupFailed => "lean_rs.worker.bootstrap.startup_failed",
        }
    }
}

impl std::fmt::Display for LeanWorkerBootstrapDiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Severity of one worker bootstrap finding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LeanWorkerBootstrapSeverity {
    /// Informational finding that does not block startup.
    Info,
    /// Suspicious state that may still start.
    Warning,
    /// The worker should not start real commands until this is fixed.
    Error,
}

/// One bounded worker bootstrap finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerBootstrapCheck {
    code: LeanWorkerBootstrapDiagnosticCode,
    severity: LeanWorkerBootstrapSeverity,
    subject: String,
    message: String,
    repair_hint: String,
}

impl LeanWorkerBootstrapCheck {
    fn error(
        code: LeanWorkerBootstrapDiagnosticCode,
        subject: impl Into<String>,
        message: impl Into<String>,
        repair_hint: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: LeanWorkerBootstrapSeverity::Error,
            subject: bound_bootstrap_text(subject.into()),
            message: bound_bootstrap_text(message.into()),
            repair_hint: bound_bootstrap_text(repair_hint.into()),
        }
    }

    /// Stable diagnostic code.
    #[must_use]
    pub fn code(&self) -> LeanWorkerBootstrapDiagnosticCode {
        self.code
    }

    /// Whether this finding blocks worker startup.
    #[must_use]
    pub fn severity(&self) -> LeanWorkerBootstrapSeverity {
        self.severity
    }

    /// Child binary, artifact, export, or protocol step this finding concerns.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Bounded explanation of the finding.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Bounded repair hint for packaged applications.
    #[must_use]
    pub fn repair_hint(&self) -> &str {
        &self.repair_hint
    }

    fn is_error(&self) -> bool {
        self.severity == LeanWorkerBootstrapSeverity::Error
    }
}

/// Structured result of worker bootstrap checks for one capability builder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerBootstrapReport {
    checks: Vec<LeanWorkerBootstrapCheck>,
}

impl LeanWorkerBootstrapReport {
    fn new(checks: Vec<LeanWorkerBootstrapCheck>) -> Self {
        Self { checks }
    }

    /// All bootstrap findings.
    #[must_use]
    pub fn checks(&self) -> &[LeanWorkerBootstrapCheck] {
        &self.checks
    }

    /// Blocking bootstrap findings.
    pub fn errors(&self) -> impl Iterator<Item = &LeanWorkerBootstrapCheck> {
        self.checks
            .iter()
            .filter(|check| check.severity == LeanWorkerBootstrapSeverity::Error)
    }

    /// Whether the worker bootstrap checks found no blocking findings.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.first_error().is_none()
    }

    /// First blocking finding, if any.
    #[must_use]
    pub fn first_error(&self) -> Option<&LeanWorkerBootstrapCheck> {
        self.errors().next()
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

    /// Open a worker session with a caller-supplied import set, overriding the imports
    /// the builder was constructed with. The capability's `project_root` / `package` /
    /// `lib_name` are unchanged.
    ///
    /// Lifecycle is identical to [`open_session`](Self::open_session): the returned
    /// session borrows from `&mut self` and dies when dropped.
    ///
    /// # Errors
    ///
    /// Same as [`open_session`](Self::open_session).
    pub fn open_session_with_imports(
        &mut self,
        imports: impl IntoIterator<Item = impl Into<String>>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerSession<'_>, LeanWorkerError> {
        let config = self.session_config.with_imports(imports);
        self.worker.open_session(&config, cancellation, progress)
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

    /// Return a snapshot of worker lifecycle counters.
    #[must_use]
    pub fn stats(&self) -> LeanWorkerStats {
        self.worker.stats()
    }

    /// Return the current worker lifecycle status.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if checking the process status fails.
    pub fn status(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        self.worker.status()
    }

    /// Measure the current child RSS in KiB when supported by the platform.
    pub fn rss_kib(&mut self) -> Option<u64> {
        self.worker.rss_kib()
    }

    /// Explicitly cycle the worker process.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker cannot be replaced.
    pub fn cycle(&mut self) -> Result<(), LeanWorkerError> {
        self.worker.cycle()
    }

    pub(crate) fn cycle_with_restart_reason(&mut self, reason: LeanWorkerRestartReason) -> Result<(), LeanWorkerError> {
        self.worker.cycle_with_restart_reason(reason)
    }

    /// Set the request timeout for subsequent commands.
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.worker.set_request_timeout(timeout);
    }

    #[doc(hidden)]
    /// Kill the child process for supervisor tests.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is already dead or kill fails.
    pub fn __kill_for_test(&mut self) -> Result<(), LeanWorkerError> {
        self.worker.__kill_for_test()
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

/// A worker-backed host session handle that is backed only by bundled shims.
///
/// Unlike [`LeanWorkerCapability`], this type has no user dylib path and no
/// metadata exports. It owns the worker supervisor and a shims-only session
/// configuration; each opened session can import project `.olean` files and
/// call the standard worker services.
#[derive(Debug)]
pub struct LeanWorkerHostHandle {
    worker: LeanWorker,
    session_config: LeanWorkerSessionConfig,
}

impl LeanWorkerHostHandle {
    /// Open a worker session for this host handle.
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

    /// Open a worker session with a caller-supplied import set, overriding the
    /// imports the builder was constructed with.
    ///
    /// Lifecycle is identical to [`open_session`](Self::open_session): the
    /// returned session borrows from `&mut self` and dies when dropped.
    ///
    /// # Errors
    ///
    /// Same as [`open_session`](Self::open_session).
    pub fn open_session_with_imports(
        &mut self,
        imports: impl IntoIterator<Item = impl Into<String>>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerSession<'_>, LeanWorkerError> {
        let config = self.session_config.with_imports(imports);
        self.worker.open_session(&config, cancellation, progress)
    }

    fn with_session_imports<T>(
        &mut self,
        imports: Vec<String>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
        command: impl Fn(&mut LeanWorkerSession<'_>) -> Result<T, LeanWorkerError>,
    ) -> Result<T, LeanWorkerError> {
        let result = {
            let mut session = self.open_session_with_imports(imports.clone(), cancellation, progress)?;
            command(&mut session)
        };
        match result {
            Ok(value) => Ok(value),
            Err(err) if worker_session_missing(&err) => {
                let mut session = self.open_session_with_imports(imports, cancellation, progress)?;
                command(&mut session)
            }
            Err(err) => Err(err),
        }
    }

    /// Open a session with `imports`, process one module query, and retry once
    /// if an automatic worker lifecycle cycle invalidated the just-opened
    /// session before the command frame was sent.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` for worker, protocol, cancellation, or
    /// progress-sink failures other than the internally retried
    /// `session_missing` race.
    pub fn process_module_query_with_imports(
        &mut self,
        imports: Vec<String>,
        source: &str,
        query: &LeanWorkerModuleQuery,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryOutcome, LeanWorkerError> {
        self.with_session_imports(imports, cancellation, progress, |session| {
            session.process_module_query(source, query.clone(), options, cancellation, progress)
        })
    }

    /// Open a session with `imports`, process one module-query batch, and
    /// retry once on the `session_missing` lifecycle race.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_module_query_with_imports`].
    pub fn process_module_query_batch_with_imports(
        &mut self,
        imports: Vec<String>,
        source: &str,
        selectors: &[LeanWorkerModuleQuerySelector],
        budgets: &LeanWorkerOutputBudgets,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryBatchOutcome, LeanWorkerError> {
        self.with_session_imports(imports, cancellation, progress, |session| {
            session.process_module_query_batch(source, selectors, budgets, options, cancellation, progress)
        })
    }

    /// Open a session with `imports` and inspect one declaration.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_module_query_with_imports`].
    pub fn inspect_declaration_with_imports(
        &mut self,
        imports: Vec<String>,
        request: &LeanWorkerDeclarationInspectionRequest,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationInspectionResult, LeanWorkerError> {
        self.with_session_imports(imports, cancellation, progress, |session| {
            session.inspect_declaration(request, cancellation, progress)
        })
    }

    /// Open a session with `imports` and run bounded declaration search.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_module_query_with_imports`].
    pub fn search_declarations_with_imports(
        &mut self,
        imports: Vec<String>,
        search: &LeanWorkerDeclarationSearch,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationSearchResult, LeanWorkerError> {
        self.with_session_imports(imports, cancellation, progress, |session| {
            session.search_declarations(search, cancellation, progress)
        })
    }

    /// Open a session with `imports` and try proof fragments in-memory.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_module_query_with_imports`].
    pub fn attempt_proof_with_imports(
        &mut self,
        imports: Vec<String>,
        request: &LeanWorkerProofAttemptRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerProofAttemptResult, LeanWorkerError> {
        self.with_session_imports(imports, cancellation, progress, |session| {
            session.attempt_proof(request, options, cancellation, progress)
        })
    }

    /// Open a session with `imports` and verify one declaration in-memory.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_module_query_with_imports`].
    pub fn verify_declaration_with_imports(
        &mut self,
        imports: Vec<String>,
        request: &LeanWorkerDeclarationVerificationRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationResult, LeanWorkerError> {
        self.with_session_imports(imports, cancellation, progress, |session| {
            session.verify_declaration(request, options, cancellation, progress)
        })
    }

    /// Return the session configuration used by this host handle.
    #[must_use]
    pub fn session_config(&self) -> &LeanWorkerSessionConfig {
        &self.session_config
    }

    /// Return protocol/runtime facts captured from the worker handshake.
    #[must_use]
    pub fn runtime_metadata(&self) -> LeanWorkerRuntimeMetadata {
        self.worker.runtime_metadata()
    }

    /// Return a snapshot of worker lifecycle counters.
    #[must_use]
    pub fn stats(&self) -> LeanWorkerStats {
        self.worker.stats()
    }

    /// Return the current worker lifecycle status.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if checking the process status fails.
    pub fn status(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        self.worker.status()
    }

    /// Measure the current child RSS in KiB when supported by the platform.
    pub fn rss_kib(&mut self) -> Option<u64> {
        self.worker.rss_kib()
    }

    /// Explicitly cycle the worker process.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker cannot be replaced.
    pub fn cycle(&mut self) -> Result<(), LeanWorkerError> {
        self.worker.cycle()
    }

    /// Restart this worker using its original configuration.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker cannot be replaced.
    pub fn restart(&mut self) -> Result<(), LeanWorkerError> {
        self.worker.restart()
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

fn worker_session_missing(err: &LeanWorkerError) -> bool {
    matches!(err, LeanWorkerError::Worker { code, .. } if code == "lean_rs.worker.session_missing")
}

#[derive(Clone, Debug)]
struct CapabilityMetadataCheck {
    export: String,
    request: Value,
    expected: Option<LeanWorkerCapabilityMetadata>,
}

#[derive(Debug)]
struct WorkerCapabilityArtifact {
    dylib_path: PathBuf,
    manifest_path: Option<PathBuf>,
    package: String,
    module: String,
}

impl WorkerCapabilityArtifact {
    fn from_built_capability(spec: &LeanBuiltCapability) -> Result<Self, LeanWorkerError> {
        if let Ok(manifest_path) = spec.resolved_manifest_path() {
            let mut artifact = Self::from_manifest(&manifest_path)?;
            artifact.manifest_path = Some(manifest_path);
            return Ok(artifact);
        }

        let dylib_path = spec.dylib_path().map_err(|err| LeanWorkerError::Setup {
            message: err.to_string(),
        })?;
        let package = spec.package_name().ok_or_else(|| LeanWorkerError::Setup {
            message: "LeanBuiltCapability is missing the Lake package name; call `.package(...)`".to_owned(),
        })?;
        let module = spec.module_name().ok_or_else(|| LeanWorkerError::Setup {
            message: "LeanBuiltCapability is missing the root Lean module name; call `.module(...)`".to_owned(),
        })?;
        Ok(Self {
            dylib_path,
            manifest_path: None,
            package: package.to_owned(),
            module: module.to_owned(),
        })
    }

    fn from_manifest(manifest_path: &Path) -> Result<Self, LeanWorkerError> {
        let bytes = std::fs::read(manifest_path).map_err(|err| LeanWorkerError::Bootstrap {
            code: LeanWorkerBootstrapDiagnosticCode::CapabilityPreflight {
                code: LeanLoaderDiagnosticCode::MissingManifest,
            },
            message: format!(
                "could not read Lean capability manifest '{}': {err}",
                manifest_path.display()
            ),
        })?;
        let manifest: WorkerCapabilityManifest =
            serde_json::from_slice(&bytes).map_err(|err| LeanWorkerError::Bootstrap {
                code: LeanWorkerBootstrapDiagnosticCode::CapabilityPreflight {
                    code: LeanLoaderDiagnosticCode::MalformedManifest,
                },
                message: format!(
                    "Lean capability manifest '{}' is malformed: {err}",
                    manifest_path.display()
                ),
            })?;
        if manifest.schema_version != u64::from(lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION) {
            return Err(LeanWorkerError::Bootstrap {
                code: LeanWorkerBootstrapDiagnosticCode::CapabilityPreflight {
                    code: LeanLoaderDiagnosticCode::UnsupportedManifestSchema,
                },
                message: format!(
                    "unsupported Lean capability manifest schema {}; supported schema is {}",
                    manifest.schema_version,
                    lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION
                ),
            });
        }
        Ok(Self {
            dylib_path: manifest.primary_dylib,
            manifest_path: Some(manifest_path.to_path_buf()),
            package: manifest.package,
            module: manifest.module,
        })
    }
}

#[derive(Deserialize)]
struct WorkerCapabilityManifest {
    schema_version: u64,
    primary_dylib: PathBuf,
    package: String,
    module: String,
}

fn worker_child_static_checks(worker_child: Option<&LeanWorkerChild>) -> Vec<LeanWorkerBootstrapCheck> {
    let mut checks = Vec::new();
    match worker_child.map_or_else(resolve_default_worker_executable, LeanWorkerChild::resolve) {
        Ok(path) => {
            if let Err(err) = validate_worker_child_path(&path) {
                checks.push(check_from_open_error(&err));
            }
        }
        Err(err) => checks.push(check_from_open_error(&err)),
    }
    checks
}

fn spawn_checked_worker(
    worker_child: Option<LeanWorkerChild>,
    startup_timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
    module_cache_limits: Option<LeanWorkerModuleCacheLimits>,
    max_frame_bytes: Option<u32>,
) -> Result<LeanWorker, LeanWorkerError> {
    let worker_child = worker_child.unwrap_or_default();
    let worker_executable = worker_child.resolve()?;
    validate_worker_child_path(&worker_executable)?;
    let lean_sysroot = worker_child.resolve_lean_sysroot()?;

    let mut config = LeanWorkerConfig::new(worker_executable).env("LEAN_SYSROOT", lean_sysroot.as_os_str());
    if let Some(timeout) = startup_timeout {
        config = config.startup_timeout(timeout);
    }
    if let Some(timeout) = request_timeout {
        config = config.request_timeout(timeout);
    }
    if let Some(policy) = restart_policy {
        config = config.restart_policy(policy);
    }
    if let Some(limits) = module_cache_limits.as_ref() {
        config = apply_module_cache_limits(config, limits);
    }
    if let Some(cap) = max_frame_bytes {
        config = config.max_frame_bytes(cap);
    }

    let mut worker = LeanWorker::spawn(&config)?;
    worker.health()?;
    Ok(worker)
}

fn apply_module_cache_limits(mut config: LeanWorkerConfig, limits: &LeanWorkerModuleCacheLimits) -> LeanWorkerConfig {
    if let Some(value) = limits.max_entries {
        config = config.env("LEAN_RS_MODULE_CACHE_MAX_ENTRIES", value.to_string());
    }
    if let Some(value) = limits.ttl_millis {
        config = config.env("LEAN_RS_MODULE_CACHE_TTL_MILLIS", value.to_string());
    }
    if let Some(value) = limits.max_bytes {
        config = config.env("LEAN_RS_MODULE_CACHE_MAX_BYTES", value.to_string());
    }
    if let Some(value) = limits.rss_guard_kib {
        config = config.env("LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB", value.to_string());
    }
    config
}

/// Locator for an app-owned worker child executable.
///
/// Dependency binaries are not automatically installed with downstream
/// applications. Production apps should ship a tiny binary that calls
/// `lean_rs_worker_child::run_worker_child_stdio` and point the capability
/// builder at it through this locator.
///
/// # Toolchain binding
///
/// A worker child binary is *built against one Lean toolchain*: its rpath
/// points at one `libleanshared`, and `LEAN_SYSROOT` at spawn time must point
/// at the matching stdlib oleans (`<sysroot>/lib/lean/Init.olean`). Mismatched
/// rpath and sysroot abort with `incompatible header` before the handshake.
///
/// The locator carries both: the binary path (via [`Self::path`] or
/// [`Self::sibling`]) and, optionally, the matching sysroot (via
/// [`Self::for_toolchain`] or [`Self::lean_sysroot`]). When the supervisor
/// spawns the child, it sets `LEAN_SYSROOT` from the locator (or from
/// [`lean_toolchain::discover_toolchain`] as a fallback) so callers never have
/// to thread the env var manually.
///
/// # Design note: no generic `env(key, value)` passthrough
///
/// `LeanWorkerCapabilityBuilder` and `LeanWorkerChild` deliberately do **not**
/// expose a general `env(key, value)` builder. Every environment variable the
/// worker child cares about has a typed method whose name describes the
/// invariant it enforces (e.g. [`Self::lean_sysroot`] enforces the
/// rpath/sysroot match). If a future env var needs to be plumbed through, add
/// a typed builder for it—do **not** add a generic `env(...)`. Generic
/// passthroughs leak implementation knowledge (env var names, framing
/// invariants) into every caller and erode the structural guarantee that
/// supported configurations cannot be misconstructed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerChild {
    executable_name: Option<String>,
    explicit_path: Option<PathBuf>,
    env_var: Option<String>,
    lean_sysroot: Option<PathBuf>,
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
            lean_sysroot: None,
        }
    }

    /// Use an explicit worker child path.
    #[must_use]
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            executable_name: None,
            explicit_path: Some(path.into()),
            env_var: None,
            lean_sysroot: None,
        }
    }

    /// Locate a worker child and declare the Lean toolchain its rpath was
    /// built against.
    ///
    /// `sysroot` is the Lean prefix containing `lib/lean/Init.olean`. The
    /// supervisor sets `LEAN_SYSROOT` to this value when spawning the child,
    /// so a single parent process can host multiple workers each pinned to a
    /// different toolchain.
    #[must_use]
    pub fn for_toolchain(path: impl Into<PathBuf>, sysroot: impl Into<PathBuf>) -> Self {
        Self {
            executable_name: None,
            explicit_path: Some(path.into()),
            env_var: None,
            lean_sysroot: Some(sysroot.into()),
        }
    }

    /// Set or override the Lean sysroot the spawned child uses.
    ///
    /// When unset, the supervisor falls back to
    /// [`lean_toolchain::discover_toolchain`] at spawn time.
    #[must_use]
    pub fn lean_sysroot(mut self, sysroot: impl Into<PathBuf>) -> Self {
        self.lean_sysroot = Some(sysroot.into());
        self
    }

    /// Add an environment-variable override for launchers and tests.
    #[must_use]
    pub fn env_override(mut self, env_var: impl Into<String>) -> Self {
        self.env_var = Some(env_var.into());
        self
    }

    /// Return the sysroot the supervisor will set as `LEAN_SYSROOT`.
    ///
    /// Returns the explicit sysroot if one was bound via
    /// [`Self::for_toolchain`] or [`Self::lean_sysroot`]; otherwise runs
    /// [`lean_toolchain::discover_toolchain`] with default options and returns
    /// the discovered prefix.
    fn resolve_lean_sysroot(&self) -> Result<PathBuf, LeanWorkerError> {
        if let Some(sysroot) = &self.lean_sysroot {
            return Ok(sysroot.clone());
        }
        let info = lean_toolchain::discover_toolchain(&lean_toolchain::DiscoverOptions::default()).map_err(|diag| {
            LeanWorkerError::Setup {
                message: format!("could not discover Lean sysroot for worker spawn: {diag}"),
            }
        })?;
        Ok(info.prefix)
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

fn validate_worker_child_path(path: &Path) -> Result<(), LeanWorkerError> {
    if !path.is_file() {
        return Err(LeanWorkerError::WorkerChildNotExecutable {
            path: path.to_path_buf(),
            reason: "path does not point to a file".to_owned(),
        });
    }
    if !is_executable_file(path) {
        return Err(LeanWorkerError::WorkerChildNotExecutable {
            path: path.to_path_buf(),
            reason: "file is not executable by this user".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(_path: &Path) -> bool {
    true
}

fn check_from_open_error(err: &LeanWorkerError) -> LeanWorkerBootstrapCheck {
    match err {
        LeanWorkerError::WorkerChildUnresolved { tried } => LeanWorkerBootstrapCheck::error(
            LeanWorkerBootstrapDiagnosticCode::WorkerChildUnresolved,
            "worker child",
            format!("could not resolve worker child; tried {}", format_paths(tried)),
            "ship an app-owned worker child binary beside the app or configure LeanWorkerChild::env_override",
        ),
        LeanWorkerError::WorkerChildNotExecutable { path, reason } => LeanWorkerBootstrapCheck::error(
            LeanWorkerBootstrapDiagnosticCode::WorkerChildNotExecutable,
            path.display().to_string(),
            reason.clone(),
            "ship an app-owned worker child binary and ensure it is executable",
        ),
        LeanWorkerError::Bootstrap { code, message } => LeanWorkerBootstrapCheck::error(
            *code,
            code.as_str(),
            message.clone(),
            "fix the reported bootstrap input",
        ),
        LeanWorkerError::Handshake { message } => LeanWorkerBootstrapCheck::error(
            LeanWorkerBootstrapDiagnosticCode::WorkerHandshakeFailed,
            "worker handshake",
            message.clone(),
            "ensure the worker child calls lean_rs_worker_child::run_worker_child_stdio and matches this crate version",
        ),
        LeanWorkerError::Timeout {
            operation: "startup", ..
        } => LeanWorkerBootstrapCheck::error(
            LeanWorkerBootstrapDiagnosticCode::WorkerHandshakeFailed,
            "worker handshake",
            err.to_string(),
            "check that the worker child starts promptly and writes the lean-rs-worker handshake",
        ),
        LeanWorkerError::CapabilityMetadataMismatch { export, .. } => LeanWorkerBootstrapCheck::error(
            LeanWorkerBootstrapDiagnosticCode::CapabilityMetadataMismatch,
            export.clone(),
            "capability metadata did not match the requested expectation",
            "rebuild or select a capability whose metadata matches the caller expectation",
        ),
        other @ (LeanWorkerError::Spawn { .. }
        | LeanWorkerError::CapabilityBuild { .. }
        | LeanWorkerError::Setup { .. }
        | LeanWorkerError::Protocol { .. }
        | LeanWorkerError::Worker { .. }
        | LeanWorkerError::ChildExited { .. }
        | LeanWorkerError::ChildPanicOrAbort { .. }
        | LeanWorkerError::Timeout { .. }
        | LeanWorkerError::Cancelled { .. }
        | LeanWorkerError::ProgressPanic { .. }
        | LeanWorkerError::DataSinkPanic { .. }
        | LeanWorkerError::DiagnosticSinkPanic { .. }
        | LeanWorkerError::StreamExportFailed { .. }
        | LeanWorkerError::StreamCallbackFailed { .. }
        | LeanWorkerError::StreamRowMalformed { .. }
        | LeanWorkerError::CapabilityMetadataMalformed { .. }
        | LeanWorkerError::CapabilityDoctorMalformed { .. }
        | LeanWorkerError::TypedCommandRequestEncode { .. }
        | LeanWorkerError::TypedCommandResponseDecode { .. }
        | LeanWorkerError::TypedCommandRowDecode { .. }
        | LeanWorkerError::TypedCommandSummaryDecode { .. }
        | LeanWorkerError::LeaseInvalidated { .. }
        | LeanWorkerError::WorkerPoolExhausted { .. }
        | LeanWorkerError::WorkerPoolMemoryBudgetExceeded { .. }
        | LeanWorkerError::WorkerPoolQueueTimeout { .. }
        | LeanWorkerError::UnsupportedRequest { .. }
        | LeanWorkerError::Wait { .. }) => LeanWorkerBootstrapCheck::error(
            LeanWorkerBootstrapDiagnosticCode::WorkerStartupFailed,
            "worker bootstrap",
            other.to_string(),
            "run the bootstrap check in a deployment environment and rebuild the worker child or capability artifact",
        ),
    }
}

fn format_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "<none>".to_owned();
    }
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn bound_bootstrap_text(mut text: String) -> String {
    const LIMIT: usize = 1_024;
    if text.len() <= LIMIT {
        return text;
    }
    while !text.is_char_boundary(LIMIT) {
        text.pop();
    }
    text.truncate(LIMIT);
    text.push_str("...");
    text
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
        .join("lean-rs-worker-child")
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
        .args(["build", "-p", "lean-rs-worker-child", "--bin", "lean-rs-worker-child"])
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::{LeanWorkerChild, LeanWorkerModuleCacheLimits, apply_module_cache_limits};
    use crate::supervisor::LeanWorkerConfig;
    use std::path::PathBuf;

    #[test]
    fn for_toolchain_carries_sysroot_through_resolve() {
        let sysroot = PathBuf::from("/opt/some/lean/prefix");
        let child = LeanWorkerChild::for_toolchain("/opt/worker", &sysroot);
        let resolved = child.resolve_lean_sysroot().expect("explicit sysroot resolves");
        assert_eq!(resolved, sysroot);
    }

    #[test]
    fn lean_sysroot_setter_overrides_default() {
        let sysroot = PathBuf::from("/opt/override/lean");
        let child = LeanWorkerChild::path("/opt/worker").lean_sysroot(&sysroot);
        let resolved = child.resolve_lean_sysroot().expect("explicit sysroot resolves");
        assert_eq!(resolved, sysroot);
    }

    #[test]
    fn explicit_sysroot_bypasses_discovery_even_when_path_is_nonexistent() {
        // The supervisor only sets `LEAN_SYSROOT`; it does not validate that
        // the path exists. Validation is the spawned child's responsibility
        // (an invalid sysroot manifests as a typed handshake/abort error
        // carrying the child's bootstrap stderr).
        let sysroot = PathBuf::from("/definitely/not/a/real/sysroot");
        let child = LeanWorkerChild::for_toolchain("/opt/worker", &sysroot);
        let resolved = child
            .resolve_lean_sysroot()
            .expect("explicit sysroot resolves without filesystem checks");
        assert_eq!(resolved, sysroot);
    }

    #[test]
    fn module_cache_limits_map_to_typed_child_policy_env() {
        let limits = LeanWorkerModuleCacheLimits::default()
            .max_entries(7)
            .ttl(std::time::Duration::from_millis(250))
            .max_bytes(4096)
            .rss_guard_kib(8192);
        let config = apply_module_cache_limits(LeanWorkerConfig::new("/opt/worker"), &limits);
        let env = config.env_overrides();
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEAN_RS_MODULE_CACHE_MAX_ENTRIES" && v == "7")
        );
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEAN_RS_MODULE_CACHE_TTL_MILLIS" && v == "250")
        );
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEAN_RS_MODULE_CACHE_MAX_BYTES" && v == "4096")
        );
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB" && v == "8192")
        );
    }
}
