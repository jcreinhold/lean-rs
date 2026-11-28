//! Worker-host adapter over the process-boundary supervisor.
//!
//! This module is intentionally narrower than `lean-rs-host::LeanSession`.
//! It exposes serializable outcomes that make sense across a child process:
//! declaration text, elaboration diagnostics, and kernel-check status. Runtime
//! handles such as `LeanExpr` and `LeanEvidence` stay inside the child.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use serde_json::value::RawValue;

use lean_rs_worker_protocol::protocol::DataRow;
use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationBatchRequest,
    LeanWorkerDeclarationVerificationBatchResult, LeanWorkerDeclarationVerificationRequest,
    LeanWorkerDeclarationVerificationResult, LeanWorkerDoctorReport, LeanWorkerElabOptions, LeanWorkerElabResult,
    LeanWorkerHostRuntimeExpectation, LeanWorkerHostRuntimeMetadata, LeanWorkerKernelResult, LeanWorkerMetaResult,
    LeanWorkerMetaTransparency, LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryOutcome,
    LeanWorkerModuleQuerySelector, LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerOutputBudgets,
    LeanWorkerProofAttemptRequest, LeanWorkerProofAttemptResult, LeanWorkerRendered, LeanWorkerResourceExhaustedFacts,
    LeanWorkerSessionImportProfile,
};

use crate::supervisor::{LeanWorker, LeanWorkerError};

/// Configuration for opening one host session inside a worker child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerSessionConfig {
    project_root: PathBuf,
    mode: LeanWorkerSessionMode,
    imports: Vec<String>,
    import_profile: LeanWorkerSessionImportProfile,
    host_runtime_expectation: Option<LeanWorkerHostRuntimeExpectation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LeanWorkerSessionMode {
    Capability {
        package: String,
        lib_name: String,
        manifest_path: Option<PathBuf>,
    },
    ShimsOnly,
}

impl LeanWorkerSessionConfig {
    /// Create a session configuration for a Lake capability and import list.
    pub fn new(
        project_root: impl Into<PathBuf>,
        package: impl Into<String>,
        lib_name: impl Into<String>,
        imports: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            mode: LeanWorkerSessionMode::Capability {
                package: package.into(),
                lib_name: lib_name.into(),
                manifest_path: None,
            },
            imports: imports.into_iter().map(Into::into).collect(),
            import_profile: LeanWorkerSessionImportProfile::default(),
            host_runtime_expectation: None,
        }
    }

    /// Create a manifest-backed session configuration for a Lake capability.
    #[must_use]
    pub fn manifest_backed(
        project_root: impl Into<PathBuf>,
        package: impl Into<String>,
        lib_name: impl Into<String>,
        manifest_path: impl Into<PathBuf>,
        imports: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            mode: LeanWorkerSessionMode::Capability {
                package: package.into(),
                lib_name: lib_name.into(),
                manifest_path: Some(manifest_path.into()),
            },
            imports: imports.into_iter().map(Into::into).collect(),
            import_profile: LeanWorkerSessionImportProfile::default(),
            host_runtime_expectation: None,
        }
    }

    /// Create a session configuration backed only by the bundled host shims.
    #[must_use]
    pub fn shims_only(project_root: impl Into<PathBuf>, imports: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            project_root: project_root.into(),
            mode: LeanWorkerSessionMode::ShimsOnly,
            imports: imports.into_iter().map(Into::into).collect(),
            import_profile: LeanWorkerSessionImportProfile::default(),
            host_runtime_expectation: None,
        }
    }

    /// Select the full-session import profile for this worker session.
    #[must_use]
    pub fn with_import_profile(mut self, profile: LeanWorkerSessionImportProfile) -> Self {
        self.import_profile = profile;
        self
    }

    /// Require actual metadata on every session open, including after recycling.
    #[must_use]
    pub fn with_host_runtime_expectation(mut self, expected: LeanWorkerHostRuntimeExpectation) -> Self {
        self.host_runtime_expectation = Some(expected);
        self
    }

    pub(crate) fn host_runtime_expectation(&self) -> Option<&LeanWorkerHostRuntimeExpectation> {
        self.host_runtime_expectation.as_ref()
    }

    pub(crate) fn project_root_string(&self) -> String {
        self.project_root.to_string_lossy().into_owned()
    }

    pub(crate) fn mode(&self) -> &LeanWorkerSessionMode {
        &self.mode
    }

    pub(crate) fn imports(&self) -> &[String] {
        &self.imports
    }

    /// Return the full-session import profile used when opening this config.
    #[must_use]
    pub fn import_profile(&self) -> LeanWorkerSessionImportProfile {
        self.import_profile
    }

    /// Return a copy of this config with a different import list, preserving
    /// the loading mode and import profile.
    #[must_use]
    pub fn with_imports(&self, imports: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            project_root: self.project_root.clone(),
            mode: self.mode.clone(),
            imports: imports.into_iter().map(Into::into).collect(),
            import_profile: self.import_profile,
            host_runtime_expectation: self.host_runtime_expectation.clone(),
        }
    }
}

/// Protocol/runtime facts reported by the worker child during handshake.
///
/// These facts describe the `lean-rs-worker` process and framing contract.
/// They are separate from downstream capability metadata returned by
/// `LeanWorkerSession::capability_metadata`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerRuntimeMetadata {
    pub worker_version: String,
    pub protocol_version: u16,
    pub lean_version: Option<String>,
}

/// Parent-side cancellation token for worker-session requests.
///
/// Cancellation is observed by the supervisor before a request is sent and at
/// least every 50 ms while awaiting IPC, even when a worker sends no progress.
/// In-flight cancellation cycles the child process; it does not share an in-process
/// `LeanCancellationToken` with the child.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerCancellationToken {
    cancelled: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl LeanWorkerCancellationToken {
    /// Create a non-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scope a worker operation to an absolute monotonic deadline. The returned
    /// token shares explicit cancellation with this token and its clones.
    /// A nested scope can shorten an existing deadline. The supervisor observes
    /// expiry while running and reports a timeout; cleanup has its own limits.
    #[must_use]
    pub fn with_deadline(&self, deadline: Instant) -> Self {
        Self {
            cancelled: Arc::clone(&self.cancelled),
            deadline: Some(self.deadline.map_or(deadline, |existing| existing.min(deadline))),
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// One progress event observed by the parent from a worker request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerProgressEvent {
    pub phase: String,
    pub current: u64,
    pub total: Option<u64>,
    pub elapsed: Duration,
}

/// Parent-side sink for worker progress events.
pub trait LeanWorkerProgressSink: Send + Sync {
    fn report(&self, event: LeanWorkerProgressEvent);
}

/// The payload of one worker data row. The parent's one row consumer, the
/// declaration-name collector, reads nothing else.
pub(crate) struct LeanWorkerRawDataRow {
    pub(crate) payload: Box<RawValue>,
}

impl From<DataRow> for LeanWorkerRawDataRow {
    fn from(value: DataRow) -> Self {
        Self { payload: value.payload }
    }
}

pub(crate) trait LeanWorkerRawDataSink: Send + Sync {
    fn report(&self, row: LeanWorkerRawDataRow);
}

/// Narrow host-session adapter over a live `LeanWorker`.
///
/// Dropping this value does not stop the worker. If a request times out, is
/// cancelled, or loses its child, the supervisor replaces the child process and
/// this session is invalidated, even when the request returned a degraded
/// verdict; open a fresh session before issuing more host requests. A planned
/// restart before a request opens the session again in the new child, and so
/// does the recovery from a child that lost its session.
pub struct LeanWorkerSession<'worker> {
    worker: &'worker mut LeanWorker,
    open: bool,
}

impl LeanWorker {
    /// Open a host session inside the worker child.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child cannot open
    /// the Lake project/capability/imports, cancellation is already requested,
    /// or protocol communication fails.
    pub fn open_session<'worker>(
        &'worker mut self,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerSession<'worker>, LeanWorkerError> {
        self.open_worker_session(config, cancellation, progress)?;
        Ok(LeanWorkerSession {
            worker: self,
            open: true,
        })
    }
}

impl LeanWorkerSession<'_> {
    /// Return the timeout used for subsequent requests on this session.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.worker.request_timeout()
    }

    /// Change the timeout for subsequent requests on this session.
    ///
    /// A timeout is parent-enforced. If it fires, the supervisor kills the
    /// child process and invalidates this session. The child is replaced at
    /// once, or by the next request when the call's own deadline has passed.
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.worker.set_request_timeout(timeout);
    }

    /// Elaborate one term and return only process-safe success/diagnostic data.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or protocol
    /// communication fails.
    pub fn elaborate(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerElabResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_elaborate(source, options, cancellation, progress))
    }

    /// Kernel-check one declaration and return only process-safe status/diagnostics.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or protocol
    /// communication fails.
    pub fn kernel_check(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerKernelResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_kernel_check(source, options, cancellation, progress))
    }

    /// Query declaration kinds in bulk.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or protocol
    /// communication fails.
    pub fn declaration_kinds(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_declaration_kinds(names, cancellation, progress))
    }

    /// Render declaration names in bulk.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or protocol
    /// communication fails.
    pub fn declaration_names(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_declaration_names(names, cancellation, progress))
    }

    /// Elaborate `source` and infer the resulting expression's type.
    ///
    /// The child attempts notation-aware rendering via the optional
    /// `meta_pp_expr` shim (`Lean.PrettyPrinter.ppExpr`) and falls back to
    /// `Expr.toString` when the shim is absent or reports `Unsupported`. The
    /// returned [`LeanWorkerRendered::rendering`] reports which path produced
    /// the value.
    ///
    /// Heartbeat budgeting: each `MetaM` pass (the primary `inferType` call
    /// and the pretty-printer) runs under the same
    /// [`LeanWorkerElabOptions::heartbeat_limit`] value, independently
    /// bounded—the pretty-printer does not consume budget left over from
    /// the primary call. A `Failed` or `TimeoutOrHeartbeat` reported by the
    /// pretty-printer surfaces as the *whole* call's failure (matching
    /// in-process behaviour); there is no path that returns the inferred
    /// expression alongside a pretty-printer failure. Only `Unsupported`
    /// from `pp_expr` triggers the raw fallback.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or
    /// protocol communication fails. Lean-side failures (type errors,
    /// heartbeat exhaustion, missing capability) surface inside the returned
    /// [`LeanWorkerMetaResult`] rather than as `Err`.
    pub fn infer_type(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<LeanWorkerRendered>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_infer_type(source, options, cancellation, progress))
    }

    /// Elaborate `source` and reduce it to weak head normal form.
    ///
    /// Rendering and heartbeat-budgeting semantics match [`Self::infer_type`]:
    /// the child attempts notation-aware rendering via `meta_pp_expr` and
    /// falls back to `Expr.toString` when the shim reports `Unsupported`.
    /// Each `MetaM` pass is independently bounded by `heartbeat_limit`; a
    /// `Failed` or `TimeoutOrHeartbeat` from the pretty-printer surfaces as
    /// the whole call's failure.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::infer_type`]. Lean-side failures surface inside the returned
    /// [`LeanWorkerMetaResult`].
    pub fn whnf(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<LeanWorkerRendered>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_whnf(source, options, cancellation, progress))
    }

    /// Elaborate `lhs` and `rhs` and ask Lean whether they are definitionally
    /// equal at the supplied transparency.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::infer_type`]. Lean-side failures surface inside the returned
    /// [`LeanWorkerMetaResult`].
    pub fn is_def_eq(
        &mut self,
        lhs: &str,
        rhs: &str,
        transparency: LeanWorkerMetaTransparency,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<bool>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_is_def_eq(lhs, rhs, transparency, options, cancellation, progress))
    }

    /// Describe a declaration: its kind, rendered type, and source range.
    ///
    /// Returns `Ok(None)` when the name is not in the session's open
    /// environment.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or
    /// protocol communication fails.
    pub fn describe(
        &mut self,
        name: &str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Option<LeanWorkerDeclarationRow>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_describe(name, cancellation, progress))
    }

    /// Search declarations and return bounded metadata-only rows plus facts.
    ///
    /// The worker applies structured name, kind, required-constant,
    /// conclusion-head, and scope filters inside Lean while scanning the
    /// imported environment. Rows are capped by `search.limit` (clamped to
    /// `1..=100` in the child) and contain no type signatures; use
    /// [`Self::declaration_type`] for explicit one-name type rendering.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::describe`].
    pub fn search_declarations(
        &mut self,
        search: &LeanWorkerDeclarationSearch,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationSearchResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_search_declarations(search, cancellation, progress))
    }

    /// Render one declaration type under a byte cap.
    ///
    /// The returned type text is never longer than `max_bytes`, except that
    /// the worker also applies a 64 KiB upper ceiling. Passing `0` requests an
    /// empty truncated rendering.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::describe`].
    pub fn declaration_type(
        &mut self,
        name: &str,
        max_bytes: usize,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Option<LeanWorkerDeclarationType>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_declaration_type(name, max_bytes, cancellation, progress))
    }

    /// Inspect one selected declaration under explicit output budgets.
    ///
    /// Declaration search intentionally returns metadata-only rows. Use this
    /// method after selecting one declaration name whose rendered statement
    /// and docstring are worth paying for.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::describe`].
    pub fn inspect_declaration(
        &mut self,
        request: &LeanWorkerDeclarationInspectionRequest,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationInspectionResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_inspect_declaration(request, cancellation, progress))
    }

    /// Try proof snippets against an in-memory source overlay.
    ///
    /// The worker never writes source files. Each candidate returns a normal
    /// status row, including failed proofs and budget exhaustion.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::process_module_query_batch`].
    pub fn attempt_proof(
        &mut self,
        request: &LeanWorkerProofAttemptRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerProofAttemptResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_attempt_proof(request, options, cancellation, progress))
    }

    /// Verify one declaration in an in-memory source snapshot.
    ///
    /// Policy failures, missing declarations, and unsupported shim support are
    /// returned as structured result statuses rather than worker errors.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::process_module_query_batch`].
    pub fn verify_declaration(
        &mut self,
        request: &LeanWorkerDeclarationVerificationRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_verify_declaration(request, options, cancellation, progress))
    }

    /// Verify several declarations in one in-memory source snapshot.
    ///
    /// The whole batch is one worker request for lifecycle, timeout, and
    /// progress purposes. Header/import state is returned once at the batch
    /// level; rows preserve the input target order.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::verify_declaration`].
    pub fn verify_declaration_batch(
        &mut self,
        request: &LeanWorkerDeclarationVerificationBatchRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationBatchResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_verify_declaration_batch(request, options, cancellation, progress))
    }

    /// Enumerate the session's open environment and return the matching
    /// declaration names as dotted strings.
    ///
    /// The child streams names one per protocol frame so total payload size
    /// is unbounded; any single Lean name fits well under the per-frame cap.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::describe`].
    pub fn list_declarations_strings(
        &mut self,
        filter: &LeanWorkerDeclarationFilter,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_list_declarations_strings(*filter, cancellation, progress))
    }

    /// Describe a batch of declarations in one IPC round-trip.
    ///
    /// Each input name produces one row in the returned vector, in the same
    /// order. Absent names keep their slot with `kind == "missing"`,
    /// `type_signature: None`, and `source: None` so callers can correlate
    /// rows back to inputs positionally.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` under the same conditions as
    /// [`Self::describe`].
    pub fn describe_bulk(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<LeanWorkerDeclarationRow>, LeanWorkerError> {
        self.with_session(|worker| worker.worker_describe_bulk(names, cancellation, progress))
    }

    /// Parse and elaborate a Lean module, returning only the requested
    /// bounded projection.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or
    /// protocol communication fails. Header-parse failures, missing imports,
    /// and missing capability shims surface as variants of
    /// [`LeanWorkerModuleQueryOutcome`].
    pub fn process_module_query(
        &mut self,
        source: &str,
        query: LeanWorkerModuleQuery,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryOutcome, LeanWorkerError> {
        self.with_session(|worker| worker.worker_process_module_query(source, query, options, cancellation, progress))
    }

    /// Parse and elaborate a Lean module once, returning several bounded
    /// selector projections keyed by selector id.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host error, cancellation is observed, a progress sink panics, or
    /// protocol communication fails. Header-parse failures, missing imports,
    /// selector unavailability, budget exhaustion, and missing capability
    /// shims surface in the returned [`LeanWorkerModuleQueryBatchOutcome`].
    pub fn process_module_query_batch(
        &mut self,
        source: &str,
        selectors: &[LeanWorkerModuleQuerySelector],
        budgets: &LeanWorkerOutputBudgets,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryBatchOutcome, LeanWorkerError> {
        self.with_session(|worker| {
            worker.worker_process_module_query_batch(source, selectors, budgets, options, cancellation, progress)
        })
    }

    /// Clear the worker child's private module snapshot cache.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, no session is open,
    /// cancellation is observed, or protocol communication fails.
    pub fn clear_module_snapshot_cache(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerError> {
        self.with_session(|worker| worker.worker_clear_module_snapshot_cache(cancellation, progress))
    }

    /// Query generic metadata from a downstream capability export.
    ///
    /// The Lean export must have ABI `String -> IO String`. The request and
    /// response strings are JSON, but callers receive a typed metadata
    /// envelope rather than private protocol frames.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the session was
    /// invalidated, the export is missing, request or response JSON is
    /// malformed, cancellation or timeout is observed, a progress sink panics,
    /// or protocol communication fails.
    pub fn capability_metadata(
        &mut self,
        export: &str,
        request: &Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerCapabilityMetadata, LeanWorkerError> {
        self.with_session(|worker| worker.worker_capability_metadata(export, request, cancellation, progress))
    }

    /// Inspect actual checked host bindings without calling a downstream export.
    ///
    /// # Errors
    /// Returns a worker/session, timeout, protocol, or loaded-shim metadata error.
    pub fn host_runtime_metadata(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerHostRuntimeMetadata, LeanWorkerError> {
        self.with_session(|worker| worker.worker_host_runtime_metadata(cancellation, progress))
    }

    /// Run a generic doctor check from a downstream capability export.
    ///
    /// The Lean export must have ABI `String -> IO String`. Doctor diagnostics
    /// are capability-layer facts; data rows remain reserved for downstream
    /// streaming payloads.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the session was
    /// invalidated, the export is missing, request or response JSON is
    /// malformed, cancellation or timeout is observed, a progress sink panics,
    /// or protocol communication fails.
    pub fn capability_doctor(
        &mut self,
        export: &str,
        request: &Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDoctorReport, LeanWorkerError> {
        self.with_session(|worker| worker.worker_capability_doctor(export, request, cancellation, progress))
    }

    fn ensure_open(&self) -> Result<(), LeanWorkerError> {
        if self.open {
            Ok(())
        } else {
            Err(LeanWorkerError::UnsupportedRequest {
                operation: "worker_session_invalidated",
            })
        }
    }

    /// Run an operation against the underlying worker, applying the session
    /// invalidation policy uniformly.
    ///
    /// Centralizes the rule that this session ends with the child that held
    /// it, so every typed method delegates here. An error that stopped that
    /// child invalidates the session, and so does a replacement that did not
    /// open the session again.
    fn with_session<T>(
        &mut self,
        f: impl FnOnce(&mut LeanWorker) -> Result<T, LeanWorkerError>,
    ) -> Result<T, LeanWorkerError> {
        self.ensure_open()?;
        let result = f(self.worker);
        if !self.worker.holds_session()
            || matches!(
                result,
                Err(LeanWorkerError::Cancelled { .. }
                    | LeanWorkerError::Timeout { .. }
                    | LeanWorkerError::ChildExited { .. }
                    | LeanWorkerError::ChildPanicOrAbort { .. }
                    | LeanWorkerError::RssHardLimitExceeded { .. }
                    | LeanWorkerError::RestartLimitExceeded { .. })
            )
        {
            self.open = false;
        }
        result
    }
}

pub(crate) fn check_cancelled(
    operation: &'static str,
    token: Option<&LeanWorkerCancellationToken>,
) -> Result<(), LeanWorkerError> {
    if token.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
        Err(LeanWorkerError::Cancelled {
            operation,
            resource: Box::new(pre_dispatch_resource(operation, "worker_cancelled")),
        })
    } else if token
        .and_then(LeanWorkerCancellationToken::deadline)
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        Err(LeanWorkerError::Timeout {
            operation,
            duration: Duration::ZERO,
            resource: Box::new(LeanWorkerResourceExhaustedFacts {
                duration_ms: Some(0),
                ..pre_dispatch_resource(operation, "worker_timeout")
            }),
        })
    } else {
        Ok(())
    }
}

fn pre_dispatch_resource(operation: &'static str, cause: &str) -> LeanWorkerResourceExhaustedFacts {
    LeanWorkerResourceExhaustedFacts {
        cause: cause.to_owned(),
        work_entered_child: false,
        operation: Some(operation.to_owned()),
        current_rss_kib: None,
        limit_kib: None,
        import_count: None,
        worker_generation: None,
        restart_reason: None,
        queue_wait_ms: None,
        duration_ms: None,
        cold_open_attempts: None,
        cold_open_admitted: None,
        cold_open_refusals: None,
        import_like_requests: None,
        import_like_admitted: None,
        last_import_stats: None,
    }
}

pub(crate) fn report_parent_progress(
    sink: Option<&dyn LeanWorkerProgressSink>,
    event: LeanWorkerProgressEvent,
) -> Result<(), LeanWorkerError> {
    let Some(sink) = sink else {
        return Ok(());
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.report(event))).map_err(|payload| {
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_owned()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "worker progress sink panicked".to_owned()
        };
        LeanWorkerError::ProgressPanic { message }
    })
}

pub(crate) fn report_parent_data_row(
    sink: Option<&dyn LeanWorkerRawDataSink>,
    row: DataRow,
) -> Result<(), LeanWorkerError> {
    let Some(sink) = sink else {
        return Err(LeanWorkerError::Protocol {
            message: "worker sent data row for a request without a row sink".to_owned(),
        });
    };
    report_raw_data_row(sink, row.into())
}

fn report_raw_data_row(sink: &dyn LeanWorkerRawDataSink, row: LeanWorkerRawDataRow) -> Result<(), LeanWorkerError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.report(row))).map_err(|payload| {
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_owned()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "worker data sink panicked".to_owned()
        };
        LeanWorkerError::DataSinkPanic { message }
    })
}

pub(crate) fn elapsed_event(
    phase: String,
    current: u64,
    total: Option<u64>,
    started: Instant,
) -> LeanWorkerProgressEvent {
    LeanWorkerProgressEvent {
        phase,
        current,
        total,
        elapsed: started.elapsed(),
    }
}
