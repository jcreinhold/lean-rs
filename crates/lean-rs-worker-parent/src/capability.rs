//! Builders for worker-backed host sessions.
//!
//! This module finds the worker child, starts it, and opens host sessions
//! that load only the bundled host shims.

use crate::process::LeanWorkerProcessTracker;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lean_rs_worker_protocol::types::{
    LeanWorkerDeclarationInspectionRequest, LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationVerificationBatchRequest,
    LeanWorkerDeclarationVerificationBatchResult, LeanWorkerDeclarationVerificationRequest,
    LeanWorkerDeclarationVerificationResult, LeanWorkerElabOptions, LeanWorkerHostRuntimeExpectation,
    LeanWorkerHostRuntimeMetadata, LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchOutcome,
    LeanWorkerModuleQueryOutcome, LeanWorkerModuleQuerySelector, LeanWorkerOutputBudgets,
    LeanWorkerProofAttemptRequest, LeanWorkerProofAttemptResult, LeanWorkerSessionImportProfile,
};

use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerProgressSink, LeanWorkerRuntimeMetadata, LeanWorkerSession,
    LeanWorkerSessionConfig,
};
use crate::supervisor::{
    LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING, LeanWorker, LeanWorkerConfig, LeanWorkerError,
    LeanWorkerLifecycleSnapshot, LeanWorkerRequestCheckpoint, LeanWorkerRequestStopReceipt, LeanWorkerRestartPolicy,
    LeanWorkerShutdownReport, LeanWorkerStats, LeanWorkerStatus,
};

const WORKER_CHILD_ENV: &str = "LEAN_RS_WORKER_CHILD";

/// Builder for a worker-backed host session that loads only bundled shims.
///
/// This is the bootstrap path for tools that use the standard host services
/// exposed through `lean-rs-host`: Meta queries, elaboration, kernel checking,
/// declaration listing, source ranges, and info trees. It deliberately has no
/// package/library fields and no metadata validation hook because no user
/// `@[export]` dylib is built or opened.
#[derive(Clone, Debug)]
pub struct LeanWorkerHostHandleBuilder {
    host_runtime_expectation: Option<LeanWorkerHostRuntimeExpectation>,
    project_root: PathBuf,
    imports: Vec<String>,
    import_profile: LeanWorkerSessionImportProfile,
    worker_child: Option<LeanWorkerChild>,
    startup_timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    shutdown_timeout: Option<Duration>,
    process_tracker: Option<LeanWorkerProcessTracker>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
    session_pool_capacity: Option<usize>,
    rss_hard_limit: Option<(u64, Duration)>,
    module_cache_limits: Option<LeanWorkerModuleCacheLimits>,
    num_threads: Option<u32>,
    lean_max_memory_kib: Option<u64>,
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
    verify_rss_taint_kib: Option<u64>,
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
    ///
    /// `0` disables the guard, matching the child, which treats a zero guard as
    /// "never clear". It is deliberately not clamped up to `1`: a 1 KiB guard is
    /// the most aggressive setting possible — every child is above it, so the
    /// snapshot cache would be cleared before every cacheable request — and that
    /// is the opposite of what a caller passing `0` asks for.
    #[must_use]
    pub fn rss_guard_kib(mut self, rss_guard_kib: u64) -> Self {
        self.rss_guard_kib = Some(rss_guard_kib);
        self
    }

    /// Set the child RSS ceiling at or above which a non-positive
    /// `verify_declaration` verdict (e.g. `NotFound`) is relabeled to
    /// `BudgetExceeded`: near the cap the worker cannot distinguish a genuine
    /// "name absent" from an elaboration silently degraded by memory pressure.
    /// Leave unset, or pass `0`, to disable the taint; set it well above the
    /// warm mathlib baseline so genuine name-absent queries are not mislabeled.
    ///
    /// `0` is not clamped up to `1` for the same reason as
    /// [`rss_guard_kib`](Self::rss_guard_kib): the child disables the taint only
    /// at exactly zero, so clamping would turn "disable" into a 1 KiB ceiling
    /// that every child exceeds, relabeling every genuine `NotFound` as
    /// `BudgetExceeded`.
    #[must_use]
    pub fn verify_rss_taint_kib(mut self, verify_rss_taint_kib: u64) -> Self {
        self.verify_rss_taint_kib = Some(verify_rss_taint_kib);
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
            host_runtime_expectation: None,
            project_root: project_root.into(),
            imports: imports.into_iter().map(Into::into).collect(),
            import_profile: LeanWorkerSessionImportProfile::default(),
            worker_child: None,
            startup_timeout: None,
            request_timeout: None,
            shutdown_timeout: None,
            process_tracker: None,
            restart_policy: None,
            session_pool_capacity: None,
            rss_hard_limit: None,
            module_cache_limits: None,
            num_threads: None,
            lean_max_memory_kib: None,
            max_frame_bytes: None,
        }
    }

    /// Enforce actual checked host-session metadata before every body command.
    #[must_use]
    pub fn require_host_runtime(mut self, expected: LeanWorkerHostRuntimeExpectation) -> Self {
        self.host_runtime_expectation = Some(expected);
        self
    }

    /// Use an explicit `lean-rs-worker-child` executable.
    #[must_use]
    pub fn worker_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.worker_child = Some(LeanWorkerChild::path(path));
        self
    }

    /// Select the full-session import profile used for opened host sessions.
    #[must_use]
    pub fn import_profile(mut self, profile: LeanWorkerSessionImportProfile) -> Self {
        self.import_profile = profile;
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

    /// Set the maximum time to wait for graceful worker shutdown.
    #[must_use]
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = Some(timeout);
        self
    }

    /// Track all child generations, including failed startup and cleanup.
    #[must_use]
    pub fn process_tracker(mut self, tracker: LeanWorkerProcessTracker) -> Self {
        self.process_tracker = Some(tracker);
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

    /// Set how many imported environments the child may hold at once.
    ///
    /// Sized by what a held environment costs (~70 MiB), not by the restart
    /// bound: see [`LeanWorkerConfig::session_pool_capacity`]. Unset, it falls
    /// back to the restart policy's `max_imports`, and then to the child's own
    /// default.
    #[must_use]
    pub fn session_pool_capacity(mut self, capacity: usize) -> Self {
        self.session_pool_capacity = Some(capacity.max(1));
        self
    }

    /// Configure the parent-side hard RSS kill watchdog for in-flight worker
    /// requests.
    #[must_use]
    pub fn rss_hard_limit(mut self, limit_kib: u64, sample_interval: Duration) -> Self {
        self.rss_hard_limit = Some((limit_kib.max(1), sample_interval.max(Duration::from_millis(1))));
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

    /// Cap the worker child's Lean task-manager thread pool via `LEAN_RS_NUM_THREADS`.
    ///
    /// Scoped to this handle; does not mutate process-global environment variables.
    #[must_use]
    pub fn num_threads(mut self, threads: u32) -> Self {
        self.num_threads = Some(threads);
        self
    }

    /// Set an opt-in Lean runtime memory ceiling via `LEAN_RS_LEAN_MAX_MEMORY_KIB`.
    ///
    /// Contains an overrun to one aborted child instead of an OS OOM-kill of the machine; it does
    /// not make the overrun recoverable. See the builder's `lean_max_memory_kib` for why. Scoped to
    /// this handle.
    #[must_use]
    pub fn lean_max_memory_kib(mut self, limit_kib: u64) -> Self {
        self.lean_max_memory_kib = Some(limit_kib);
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
            Ok(mut handle) => {
                // `open` no longer opens a session eagerly, so this check must:
                // proving a shims-only session can open is the part of the
                // report that a static path cannot answer.
                if let Err(err) = handle.open_session(None, None) {
                    checks.push(check_from_open_error(&err));
                }
                drop(handle.shutdown());
            }
            Err(err) => checks.push(check_from_open_error(&err)),
        }
        LeanWorkerBootstrapReport::new(checks)
    }

    /// Start the worker and, if this builder was told what to import, open that
    /// session once.
    ///
    /// A builder given **no** imports opens no session. It has nothing to
    /// prepare: a caller who names its imports per call
    /// ([`open_session_with_imports`](LeanWorkerHostHandle::open_session_with_imports))
    /// never reuses the empty one, so opening it cost a full Lean import that
    /// was thrown away, plus one of the child's pooled session slots and one of
    /// `max_imports` for the life of the child. The deployment validation that
    /// open also performed is unchanged and available up front through
    /// [`check`](Self::check), which opens a session unconditionally; without
    /// that call, an unopenable session surfaces at the caller's first session
    /// instead of here.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker child cannot be resolved or
    /// spawned, startup/health fails, or a named import set cannot open. This
    /// method does not build a user Lake shared-library target.
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
            self.shutdown_timeout,
            self.process_tracker,
            self.restart_policy,
            self.session_pool_capacity,
            self.rss_hard_limit,
            self.module_cache_limits,
            self.max_frame_bytes,
            self.num_threads,
            self.lean_max_memory_kib,
        )?;
        let mut session_config = LeanWorkerSessionConfig::shims_only(self.project_root, self.imports)
            .with_import_profile(self.import_profile);
        if let Some(expected) = self.host_runtime_expectation {
            session_config = session_config.with_host_runtime_expectation(expected);
        }
        if !session_config.imports().is_empty() {
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
    /// The worker child did not complete the protocol handshake.
    WorkerHandshakeFailed,
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
            Self::WorkerHandshakeFailed => "lean_rs.worker.bootstrap.handshake_failed",
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

/// Structured result of worker bootstrap checks for one host-handle builder.
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

/// A worker-backed host session handle that is backed only by bundled shims.
///
/// It owns the worker supervisor and a shims-only session configuration. Each
/// session it opens can import project `.olean` files and call the standard
/// worker services.
#[derive(Debug)]
pub struct LeanWorkerHostHandle {
    worker: LeanWorker,
    session_config: LeanWorkerSessionConfig,
}

impl LeanWorkerHostHandle {
    /// Open a worker session for this host handle.
    ///
    /// The builder does not open one, so the first call here is also where a
    /// project that cannot produce a session first says so — run
    /// [`LeanWorkerHostHandleBuilder::check`] first to learn that up front.
    /// Later calls remain fallible because worker cycling, cancellation, or a
    /// child failure may require a fresh session.
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
        command: impl FnOnce(&mut LeanWorkerSession<'_>) -> Result<T, LeanWorkerError>,
    ) -> Result<T, LeanWorkerError> {
        let mut session = self.open_session_with_imports(imports, cancellation, progress)?;
        command(&mut session)
    }

    /// Open a session with `imports` and process one module query.
    ///
    /// If the child has lost the session by the time the query arrives, the
    /// supervisor replaces the child, opens the session again and resends the
    /// query once.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` for worker, protocol, cancellation, or
    /// progress-sink failures.
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

    /// Open a session with `imports` and process one module-query batch.
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

    /// Open a session with `imports` and verify several declarations in one
    /// in-memory source snapshot.
    ///
    /// # Errors
    ///
    /// Same as [`Self::process_module_query_with_imports`].
    pub fn verify_declaration_batch_with_imports(
        &mut self,
        imports: Vec<String>,
        request: &LeanWorkerDeclarationVerificationBatchRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationBatchResult, LeanWorkerError> {
        self.with_session_imports(imports, cancellation, progress, |session| {
            session.verify_declaration_batch(request, options, cancellation, progress)
        })
    }

    /// Require `expected` host runtime metadata for every later session open.
    /// A cached actual observation is checked before changing the configuration.
    ///
    /// # Errors
    /// Returns a protocol error if the current loaded bindings do not satisfy
    /// the new requirement. The previous configuration remains unchanged.
    pub fn set_host_runtime_expectation(
        &mut self,
        expected: LeanWorkerHostRuntimeExpectation,
    ) -> Result<(), LeanWorkerError> {
        if let Some(actual) = self.worker.host_runtime_metadata() {
            expected.validate(actual).map_err(|message| LeanWorkerError::Protocol {
                message: format!("host runtime compatibility: {message}"),
            })?;
        }
        self.session_config = self.session_config.clone().with_host_runtime_expectation(expected);
        Ok(())
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

    /// Actual metadata last validated in this child generation, never a cold
    /// status or a configured worker-sidecar claim.
    #[must_use]
    pub fn host_runtime_metadata(&self) -> Option<&LeanWorkerHostRuntimeMetadata> {
        self.worker.host_runtime_metadata()
    }

    /// Return a snapshot of worker lifecycle counters.
    #[must_use]
    pub fn stats(&self) -> LeanWorkerStats {
        self.worker.stats()
    }

    /// Return policy-facing lifecycle facts for this worker.
    #[must_use]
    pub fn lifecycle_snapshot(&self) -> LeanWorkerLifecycleSnapshot {
        self.worker.lifecycle_snapshot()
    }

    /// Checkpoint the request stream before a synchronous host operation.
    #[must_use]
    pub fn request_checkpoint(&self) -> LeanWorkerRequestCheckpoint {
        self.worker.request_checkpoint()
    }

    /// Retain an exact request's eventual owned-exit observation.
    #[must_use]
    pub fn pending_request_stop_since(
        &self,
        checkpoint: &LeanWorkerRequestCheckpoint,
    ) -> Option<crate::supervisor::LeanWorkerPendingRequestStop> {
        self.worker.pending_request_stop_since(checkpoint)
    }

    /// Confirm exit of the latest request's direct child since this checkpoint.
    ///
    /// See [`LeanWorker::confirmed_request_stop_since`] for the scope and limits
    /// of this evidence, including failed replacement startup.
    #[must_use]
    pub fn confirmed_request_stop_since(
        &self,
        checkpoint: &LeanWorkerRequestCheckpoint,
    ) -> Option<&LeanWorkerRequestStopReceipt> {
        self.worker.confirmed_request_stop_since(checkpoint)
    }

    /// Return the current worker lifecycle status.
    ///
    /// `Exited` is not final; see [`LeanWorker::status`].
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if checking the process status fails.
    pub fn status(&mut self) -> Result<LeanWorkerStatus, LeanWorkerError> {
        self.worker.status()
    }

    /// Whether owned cleanup is pending; see [`LeanWorker::cleanup_pending`].
    #[must_use]
    pub fn cleanup_pending(&self) -> bool {
        self.worker.cleanup_pending()
    }

    /// Exact pre-body terminal refusal, preserved before resource cleanup.
    #[must_use]
    pub fn host_session_refusal_since(&self, checkpoint: &LeanWorkerRequestCheckpoint) -> Option<&'static str> {
        self.worker.host_session_refusal_since(checkpoint)
    }

    /// Measure the current child RSS in KiB when supported by the platform.
    pub fn rss_kib(&mut self) -> Option<u64> {
        self.worker.rss_kib()
    }

    /// Measure the current child CPU time (user + system) in milliseconds.
    ///
    /// Cumulative since the child started; take a delta around a unit of work
    /// for a contention-immune cost measure. `None` means the platform did not
    /// provide a usable sample; it is not a worker failure.
    pub fn cumulative_cpu_millis(&mut self) -> Option<u64> {
        self.worker.cumulative_cpu_millis()
    }

    /// Explicitly cycle the worker process, as [`LeanWorker::cycle`] does. This
    /// also starts the worker again after [`Self::try_shutdown`].
    ///
    /// # Errors
    ///
    /// Returns [`LeanWorkerError::RestartLimitExceeded`] while abnormal
    /// restarts keep the worker down, or another `LeanWorkerError` if the
    /// worker cannot be replaced.
    pub fn cycle(&mut self) -> Result<(), LeanWorkerError> {
        self.worker.cycle()
    }

    /// Shut down the worker child and return structured shutdown status.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if shutdown escalation or wait/reap fails.
    pub fn shutdown(self) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        self.worker.shutdown()
    }

    /// Stop the worker while retaining its owner if cleanup fails.
    ///
    /// The worker then refuses requests with
    /// [`LeanWorkerError::ShutdownInProgress`] until [`Self::cycle`] starts a
    /// new child.
    ///
    /// # Errors
    /// Returns a shutdown error without discarding the child process handle.
    pub fn try_shutdown(&mut self) -> Result<LeanWorkerShutdownReport, LeanWorkerError> {
        self.worker.try_shutdown()
    }
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
    shutdown_timeout: Option<Duration>,
    process_tracker: Option<LeanWorkerProcessTracker>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
    session_pool_capacity: Option<usize>,
    rss_hard_limit: Option<(u64, Duration)>,
    module_cache_limits: Option<LeanWorkerModuleCacheLimits>,
    max_frame_bytes: Option<u32>,
    num_threads: Option<u32>,
    lean_max_memory_kib: Option<u64>,
) -> Result<LeanWorker, LeanWorkerError> {
    let worker_child = worker_child.unwrap_or_default();
    let worker_executable = worker_child.resolve()?;
    validate_worker_child_path(&worker_executable)?;
    let lean_sysroot = worker_child.resolve_lean_sysroot()?;

    let mut config = LeanWorkerConfig::new(worker_executable).env("LEAN_SYSROOT", lean_sysroot.as_os_str());
    config.application_launch_input = worker_child.application_launch_input;
    if worker_child.check_loader_environment {
        config = config.checked_native_loader_environment();
    }
    if let Some(artifacts) = &worker_child.prepared_host_artifacts {
        let encoded = serde_json::to_string(artifacts).map_err(|error| LeanWorkerError::Setup {
            message: format!("could not encode prepared host artifacts: {error}"),
        })?;
        if encoded.len() > 64 * 1024 {
            return Err(LeanWorkerError::Setup {
                message: "prepared host artifacts exceed the 64 KiB launch limit".to_owned(),
            });
        }
        config = config.env(lean_rs_worker_protocol::PREPARED_HOST_ARTIFACTS_ENV, encoded);
    }
    if let Some(tracker) = process_tracker {
        config = config.process_tracker(tracker);
    }
    if let Some(timeout) = startup_timeout {
        config = config.startup_timeout(timeout);
    }
    if let Some(timeout) = request_timeout {
        config = config.request_timeout(timeout);
    }
    if let Some(timeout) = shutdown_timeout {
        config = config.shutdown_timeout(timeout);
    }
    if let Some(policy) = restart_policy {
        config = config.restart_policy(policy);
    }
    if let Some(capacity) = session_pool_capacity {
        config = config.session_pool_capacity(capacity);
    }
    if let Some((limit_kib, sample_interval)) = rss_hard_limit {
        config = config.rss_hard_limit(limit_kib, sample_interval);
    }
    if let Some(limits) = module_cache_limits.as_ref() {
        config = apply_module_cache_limits(config, limits);
    }
    config = apply_runtime_limits(config, num_threads, lean_max_memory_kib);
    if let Some(cap) = max_frame_bytes {
        config = config.max_frame_bytes(cap);
    }

    let mut worker = LeanWorker::spawn(&config)?;
    worker.health()?;
    Ok(worker)
}

/// Apply the typed worker-child runtime caps as child environment variables.
///
/// `num_threads` caps the child's Lean task-manager pool (`LEAN_RS_NUM_THREADS`); the memory ceiling
/// arms the child's Lean runtime OOM guardrail (`LEAN_RS_LEAN_MAX_MEMORY_KIB`). Both are typed knobs
/// so callers need not know the child's environment-variable names.
fn apply_runtime_limits(
    mut config: LeanWorkerConfig,
    num_threads: Option<u32>,
    lean_max_memory_kib: Option<u64>,
) -> LeanWorkerConfig {
    if let Some(threads) = num_threads {
        config = config.env("LEAN_RS_NUM_THREADS", threads.to_string());
    }
    if let Some(limit_kib) = lean_max_memory_kib {
        config = config.env("LEAN_RS_LEAN_MAX_MEMORY_KIB", limit_kib.to_string());
    }
    config
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
    if let Some(value) = limits.verify_rss_taint_kib {
        config = config.env("LEAN_RS_VERIFY_RSS_TAINT_KIB", value.to_string());
    }
    config
}

/// Locator for an app-owned worker child executable.
///
/// Dependency binaries are not automatically installed with downstream
/// applications. Production apps should ship a tiny binary that calls
/// `lean_rs_worker_child::run_worker_child_stdio` and point the host-handle
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
/// `LeanWorkerHostHandleBuilder` and `LeanWorkerChild` deliberately do **not**
/// expose a general `env(key, value)` builder. Every environment variable the
/// worker child cares about has a typed method whose name describes the
/// invariant it enforces (e.g. [`Self::lean_sysroot`] enforces the
/// rpath/sysroot match). If a future env var needs to be plumbed through, add
/// a typed builder for it—do **not** add a generic `env(...)`. Generic
/// passthroughs leak implementation knowledge (env var names, framing
/// invariants) into every caller and erode the structural guarantee that
/// supported configurations cannot be misconstructed.
///
/// # Provisioning is the caller's job
///
/// This locator only *finds* a worker child; it does not install one.
/// Dependency binaries are not shipped with downstream applications, so a
/// downstream must provision its own worker child — ship it beside the host
/// binary (resolved by [`Self::sibling`]), point at it explicitly
/// ([`Self::path`] / [`Self::for_toolchain`]), or supply it through an
/// [`Self::env_override`] variable. The one exception is a developer
/// convenience internal to the `lean-rs` source tree: when the requested name
/// is the default `lean-rs-worker-child` *and* resolution runs from within the
/// `lean-rs` workspace, [`Self::sibling`] also falls back to building that
/// binary with `cargo build -p lean-rs-worker-child` (so the workspace's own
/// examples and profiling harness run with no extra steps). That fallback is
/// keyed to `lean-rs`'s own `CARGO_MANIFEST_DIR` and is **inert for every
/// downstream** — outside the source tree it finds no workspace to build and
/// no-ops. Do not rely on it from a consuming crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerChild {
    executable_name: Option<String>,
    explicit_path: Option<PathBuf>,
    env_var: Option<String>,
    lean_sysroot: Option<PathBuf>,
    application_launch_input: Option<String>,
    prepared_host_artifacts: Option<lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts>,
    check_loader_environment: bool,
}

impl LeanWorkerChild {
    /// Locate a worker child beside the current executable, or beside the
    /// Cargo profile directory during tests and `cargo run`.
    ///
    /// For the default name `lean-rs-worker-child`, resolution additionally
    /// falls back to building the binary from source — but only inside the
    /// `lean-rs` workspace itself. See the [type-level provisioning
    /// note](Self#provisioning-is-the-callers-job): that fallback is an
    /// in-tree developer convenience and is inert for downstream callers, which
    /// must ship or point at their own worker child.
    #[must_use]
    pub fn sibling(executable_name: impl Into<String>) -> Self {
        Self {
            executable_name: Some(with_exe_suffix(executable_name.into())),
            explicit_path: None,
            env_var: None,
            lean_sysroot: None,
            application_launch_input: None,
            prepared_host_artifacts: None,
            check_loader_environment: false,
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
            application_launch_input: None,
            prepared_host_artifacts: None,
            check_loader_environment: false,
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
            application_launch_input: None,
            prepared_host_artifacts: None,
            check_loader_environment: false,
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

    /// Set an immutable application-defined UTF-8 value for this child and its replacements.
    ///
    /// The child reads it from [`lean_rs_worker_protocol::WORKER_APPLICATION_INPUT_ENV`].
    /// The supervisor removes any ambient value when the input is omitted. The application
    /// defines and validates its contents. For an external file, the application can carry
    /// its locator and expected digest here and retain custody itself.
    ///
    /// # Errors
    /// Returns a setup error for an empty value, a NUL byte, or more than 4096 UTF-8 bytes.
    pub fn application_launch_input(mut self, value: &str) -> Result<Self, LeanWorkerError> {
        if value.is_empty() || value.len() > 4096 || value.as_bytes().contains(&0) {
            return Err(LeanWorkerError::Setup {
                message: "application launch input requires 1 to 4096 UTF-8 bytes without NUL".to_owned(),
            });
        }
        self.application_launch_input = Some(value.to_owned());
        Ok(self)
    }

    /// Select the exact prepared host shims for this worker generation.
    ///
    /// The child passes the snapshot to its application's host loader. The
    /// application must verify the actual artifacts and keep them unchanged
    /// while the worker uses them. An explicit descriptor error fails startup
    /// or session admission.
    #[must_use]
    pub fn prepared_host_artifacts(
        mut self,
        artifacts: lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts,
    ) -> Self {
        self.prepared_host_artifacts = Some(artifacts);
        self
    }

    /// Refuse native loader environment overrides before each worker launch or restart.
    /// This is off by default and does not attest the native images.
    #[must_use]
    pub fn checked_native_loader_environment(mut self) -> Self {
        self.check_loader_environment = true;
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
        // In-tree convenience only: `try_build_workspace_worker_child` can build
        // exactly the `lean-rs-worker-child` binary and only when run from the
        // `lean-rs` workspace, so it is gated on that name (a differently-named
        // sibling would otherwise build the wrong binary) and no-ops downstream.
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
        other @ (LeanWorkerError::Spawn { .. }
        | LeanWorkerError::CapabilityBuild { .. }
        | LeanWorkerError::Setup { .. }
        | LeanWorkerError::Protocol { .. }
        | LeanWorkerError::Worker { .. }
        | LeanWorkerError::ChildExited { .. }
        | LeanWorkerError::ChildPanicOrAbort { .. }
        | LeanWorkerError::Timeout { .. }
        | LeanWorkerError::RssHardLimitExceeded { .. }
        | LeanWorkerError::Cancelled { .. }
        | LeanWorkerError::ProgressPanic { .. }
        | LeanWorkerError::DataSinkPanic { .. }
        | LeanWorkerError::CapabilityMetadataMalformed { .. }
        | LeanWorkerError::CapabilityDoctorMalformed { .. }
        | LeanWorkerError::RestartLimitExceeded { .. }
        | LeanWorkerError::UnsupportedRequest { .. }
        | LeanWorkerError::Wait { .. }
        | LeanWorkerError::Kill { .. }
        | LeanWorkerError::WaitTimeout { .. }
        | LeanWorkerError::ShutdownInProgress { .. }) => LeanWorkerBootstrapCheck::error(
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

/// Build `lean-rs-worker-child` from source as an in-tree developer
/// convenience for the workspace's own examples, tests, and profiling harness.
///
/// `CARGO_MANIFEST_DIR` is baked at compile time of this crate, so the derived
/// `workspace` only contains `crates/lean-rs-worker-child/Cargo.toml` when this
/// crate is being built *as part of the `lean-rs` source tree*. For a
/// downstream that depends on the published crate, `CARGO_MANIFEST_DIR` points
/// into the registry cache, the guard below fails, and this returns `None`
/// without side effects. Downstreams must therefore provision their own worker
/// child (see [`LeanWorkerChild`]).
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
    use super::{LeanWorkerChild, LeanWorkerModuleCacheLimits, apply_module_cache_limits, apply_runtime_limits};
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

    #[test]
    fn zero_disables_rather_than_becoming_the_strictest_setting() {
        // The child spells "off" as exactly 0 for both knobs. Clamping 0 up to
        // 1 would invert each one into its most aggressive form: a 1 KiB cache
        // guard clears before every request, and a 1 KiB verify ceiling
        // relabels every genuine `NotFound` as `BudgetExceeded`.
        let limits = LeanWorkerModuleCacheLimits::default()
            .rss_guard_kib(0)
            .verify_rss_taint_kib(0);
        let config = apply_module_cache_limits(LeanWorkerConfig::new("/opt/worker"), &limits);
        let env = config.env_overrides();
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB" && v == "0")
        );
        assert!(env.iter().any(|(k, v)| k == "LEAN_RS_VERIFY_RSS_TAINT_KIB" && v == "0"));
    }

    #[test]
    fn runtime_limits_map_to_typed_child_env() {
        let config = apply_runtime_limits(LeanWorkerConfig::new("/opt/worker"), Some(1), Some(16_777_216));
        let env = config.env_overrides();
        assert!(env.iter().any(|(k, v)| k == "LEAN_RS_NUM_THREADS" && v == "1"));
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEAN_RS_LEAN_MAX_MEMORY_KIB" && v == "16777216")
        );
    }

    #[test]
    fn runtime_limits_are_absent_when_unset() {
        let config = apply_runtime_limits(LeanWorkerConfig::new("/opt/worker"), None, None);
        let env = config.env_overrides();
        assert!(!env.iter().any(|(k, _)| k == "LEAN_RS_NUM_THREADS"));
        assert!(!env.iter().any(|(k, _)| k == "LEAN_RS_LEAN_MAX_MEMORY_KIB"));
    }
}
