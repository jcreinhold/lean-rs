//! Worker-host adapter over the process-boundary supervisor.
//!
//! This module is intentionally narrower than `lean-rs-host::LeanSession`.
//! It exposes serializable outcomes that make sense across a child process:
//! declaration text, elaboration diagnostics, and kernel-check status. Runtime
//! handles such as `LeanExpr` and `LeanEvidence` stay inside the child.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::value::RawValue;

use lean_rs_worker_protocol::protocol::{DataRow, Diagnostic, StreamSummary};
use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationType, LeanWorkerDoctorReport, LeanWorkerElabOptions,
    LeanWorkerElabResult, LeanWorkerKernelResult, LeanWorkerMetaResult, LeanWorkerMetaTransparency,
    LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryOutcome,
    LeanWorkerModuleQuerySelector, LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerOutputBudgets,
    LeanWorkerRendered,
};

use crate::supervisor::{LeanWorker, LeanWorkerError};

/// Configuration for opening one host session inside a worker child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerSessionConfig {
    project_root: PathBuf,
    mode: LeanWorkerSessionMode,
    imports: Vec<String>,
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
        }
    }

    /// Create a session configuration backed only by the bundled host shims.
    #[must_use]
    pub fn shims_only(project_root: impl Into<PathBuf>, imports: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            project_root: project_root.into(),
            mode: LeanWorkerSessionMode::ShimsOnly,
            imports: imports.into_iter().map(Into::into).collect(),
        }
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

    pub(crate) fn with_imports(&self, imports: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            project_root: self.project_root.clone(),
            mode: self.mode.clone(),
            imports: imports.into_iter().map(Into::into).collect(),
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
/// worker progress frames while a request is in flight. In-flight cancellation
/// cycles the child process; it does not share an in-process
/// `LeanCancellationToken` with the child.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl LeanWorkerCancellationToken {
    /// Create a non-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

/// One downstream-owned JSON row delivered over a worker request.
///
/// `stream` is a caller-defined channel name. `sequence` starts at zero per
/// stream inside one request and is assigned by `lean-rs-worker`. `payload` is
/// owned JSON; callers may keep it after `LeanWorkerDataSink::report` returns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerDataRow {
    pub stream: String,
    pub sequence: u64,
    pub payload: Value,
}

impl TryFrom<DataRow> for LeanWorkerDataRow {
    type Error = LeanWorkerError;

    fn try_from(value: DataRow) -> Result<Self, Self::Error> {
        let payload = serde_json::from_str(value.payload.get()).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker data-row payload decode failed: {err}"),
        })?;
        Ok(Self {
            stream: value.stream,
            sequence: value.sequence,
            payload,
        })
    }
}

/// Parent-side sink for downstream data rows produced by one worker request.
///
/// A sink is borrowed for one request. It receives owned rows and may store
/// them. If `report` panics, the supervisor catches the panic and returns
/// `LeanWorkerError::DataSinkPanic`.
pub trait LeanWorkerDataSink: Send + Sync {
    fn report(&self, row: LeanWorkerDataRow);
}

pub(crate) struct LeanWorkerRawDataRow {
    pub(crate) stream: String,
    pub(crate) sequence: u64,
    pub(crate) payload: Box<RawValue>,
}

impl From<DataRow> for LeanWorkerRawDataRow {
    fn from(value: DataRow) -> Self {
        Self {
            stream: value.stream,
            sequence: value.sequence,
            payload: value.payload,
        }
    }
}

pub(crate) trait LeanWorkerRawDataSink: Send + Sync {
    fn report(&self, row: LeanWorkerRawDataRow);
}

#[derive(Clone, Copy)]
pub(crate) enum LeanWorkerDataSinkTarget<'a> {
    Value(&'a dyn LeanWorkerDataSink),
    Raw(&'a dyn LeanWorkerRawDataSink),
}

/// One diagnostic message delivered over a worker request.
///
/// Diagnostics are control/observability messages, not data rows. They are
/// delivered through `LeanWorkerDiagnosticSink` so row payloads remain
/// downstream-owned data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerDiagnosticEvent {
    pub code: String,
    pub message: String,
}

impl From<Diagnostic> for LeanWorkerDiagnosticEvent {
    fn from(value: Diagnostic) -> Self {
        Self {
            code: value.code,
            message: value.message,
        }
    }
}

/// Parent-side sink for diagnostics produced by one worker request.
pub trait LeanWorkerDiagnosticSink: Send + Sync {
    fn report(&self, diagnostic: LeanWorkerDiagnosticEvent);
}

/// Summary returned after a worker data-stream export completes.
///
/// Rows delivered to `LeanWorkerDataSink` are tentative until this summary is
/// returned successfully. Downstream callers that need atomic commit should
/// buffer rows in their sink and commit only after terminal success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerStreamSummary {
    /// Total number of rows delivered to the parent before terminal success.
    pub total_rows: u64,
    /// Per-stream row counts assigned by `lean-rs-worker`.
    pub per_stream_counts: BTreeMap<String, u64>,
    /// Elapsed time measured in the child for the streaming export.
    pub elapsed: Duration,
    /// Optional downstream-defined terminal metadata.
    pub metadata: Option<Value>,
}

impl From<StreamSummary> for LeanWorkerStreamSummary {
    fn from(value: StreamSummary) -> Self {
        Self {
            total_rows: value.total_rows,
            per_stream_counts: value.per_stream_counts,
            elapsed: Duration::from_micros(value.elapsed_micros),
            metadata: value.metadata,
        }
    }
}

/// A non-streaming downstream JSON command.
///
/// The command names a Lean export with ABI `String -> IO String`. `Req` and
/// `Resp` are downstream-owned serde types; `lean-rs-worker` owns request
/// transport, worker lifecycle, timeout, cancellation, and response decoding.
pub struct LeanWorkerJsonCommand<Req, Resp> {
    export: String,
    _types: PhantomData<fn(&Req) -> Resp>,
}

impl<Req, Resp> LeanWorkerJsonCommand<Req, Resp> {
    /// Create a typed JSON command for one Lean export.
    #[must_use]
    pub fn new(export: impl Into<String>) -> Self {
        Self {
            export: export.into(),
            _types: PhantomData,
        }
    }

    /// Return the Lean export name used by this command.
    #[must_use]
    pub fn export(&self) -> &str {
        &self.export
    }
}

impl<Req, Resp> Clone for LeanWorkerJsonCommand<Req, Resp> {
    fn clone(&self) -> Self {
        Self {
            export: self.export.clone(),
            _types: PhantomData,
        }
    }
}

impl<Req, Resp> fmt::Debug for LeanWorkerJsonCommand<Req, Resp> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeanWorkerJsonCommand")
            .field("export", &self.export)
            .finish()
    }
}

impl<Req, Resp> PartialEq for LeanWorkerJsonCommand<Req, Resp> {
    fn eq(&self, other: &Self) -> bool {
        self.export == other.export
    }
}

impl<Req, Resp> Eq for LeanWorkerJsonCommand<Req, Resp> {}

/// A streaming downstream JSON command.
///
/// The command names a Lean export with ABI
/// `String -> USize -> USize -> IO UInt8`. `Req`, `Row`, and `Summary` are
/// downstream-owned serde types. Row and terminal-summary JSON are decoded at
/// the parent boundary, after `lean-rs-worker` has handled process lifecycle,
/// framing, diagnostics, timeout, cancellation, and completion.
pub struct LeanWorkerStreamingCommand<Req, Row, Summary> {
    export: String,
    _types: PhantomData<fn(&Req) -> (Row, Summary)>,
}

impl<Req, Row, Summary> LeanWorkerStreamingCommand<Req, Row, Summary> {
    /// Create a typed streaming command for one Lean export.
    #[must_use]
    pub fn new(export: impl Into<String>) -> Self {
        Self {
            export: export.into(),
            _types: PhantomData,
        }
    }

    /// Return the Lean export name used by this command.
    #[must_use]
    pub fn export(&self) -> &str {
        &self.export
    }
}

impl<Req, Row, Summary> Clone for LeanWorkerStreamingCommand<Req, Row, Summary> {
    fn clone(&self) -> Self {
        Self {
            export: self.export.clone(),
            _types: PhantomData,
        }
    }
}

impl<Req, Row, Summary> fmt::Debug for LeanWorkerStreamingCommand<Req, Row, Summary> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeanWorkerStreamingCommand")
            .field("export", &self.export)
            .finish()
    }
}

impl<Req, Row, Summary> PartialEq for LeanWorkerStreamingCommand<Req, Row, Summary> {
    fn eq(&self, other: &Self) -> bool {
        self.export == other.export
    }
}

impl<Req, Row, Summary> Eq for LeanWorkerStreamingCommand<Req, Row, Summary> {}

/// One typed downstream row decoded from a worker data row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerTypedDataRow<Row> {
    pub stream: String,
    pub sequence: u64,
    pub payload: Row,
}

/// Parent-side sink for typed downstream data rows produced by one command.
///
/// The sink remains request-local. A panic from `report` is contained by the
/// worker supervisor and returned as `LeanWorkerError::DataSinkPanic`.
pub trait LeanWorkerTypedDataSink<Row>: Send + Sync {
    fn report(&self, row: LeanWorkerTypedDataRow<Row>);
}

/// Typed summary returned after a streaming command reaches terminal success.
///
/// Rows delivered to a typed sink remain tentative until this summary is
/// returned. `metadata` is decoded from the downstream terminal JSON metadata,
/// when the export provides it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerTypedStreamSummary<Summary> {
    pub total_rows: u64,
    pub per_stream_counts: BTreeMap<String, u64>,
    pub elapsed: Duration,
    pub metadata: Option<Summary>,
}

/// Narrow host-session adapter over a live `LeanWorker`.
///
/// Dropping this value does not stop the worker. If a request is cancelled
/// while in flight, the supervisor cycles the child process and this session is
/// invalidated; open a fresh session before issuing more host requests.
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
    /// A timeout is parent-enforced. If it fires, the supervisor kills and
    /// replaces the child process and invalidates this session.
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

    /// Run a downstream streaming export and deliver JSON rows to `rows`.
    ///
    /// The Lean export must have ABI
    /// `String -> USize -> USize -> IO UInt8`. The child supplies the
    /// callback handle and trampoline; the parent only sees validated
    /// `LeanWorkerDataRow` values.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the child reports a
    /// host or stream error, cancellation is observed, a sink panics, or
    /// protocol communication fails. In-flight cancellation cycles the child
    /// and invalidates this session.
    pub fn run_data_stream(
        &mut self,
        export: &str,
        request: &Value,
        rows: &dyn LeanWorkerDataSink,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerStreamSummary, LeanWorkerError> {
        self.with_session(|worker| {
            worker.worker_run_data_stream(export, request, rows, diagnostics, cancellation, progress)
        })
    }

    fn run_data_stream_raw(
        &mut self,
        export: &str,
        request: &Value,
        rows: &dyn LeanWorkerRawDataSink,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerStreamSummary, LeanWorkerError> {
        self.with_session(|worker| {
            worker.worker_run_data_stream_raw(export, request, rows, diagnostics, cancellation, progress)
        })
    }

    /// Run a typed non-streaming downstream JSON command.
    ///
    /// The Lean export must have ABI `String -> IO String`. The request is
    /// serialized from `Req`; the returned JSON string is decoded into `Resp`.
    /// Use this for commands that return one terminal JSON value and no rows.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if request encoding fails, the worker is
    /// dead, the session was invalidated, the export is missing, response
    /// decoding fails, cancellation or timeout is observed, a progress sink
    /// panics, or protocol communication fails.
    pub fn run_json_command<Req, Resp>(
        &mut self,
        command: &LeanWorkerJsonCommand<Req, Resp>,
        request: &Req,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Resp, LeanWorkerError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let request_json =
            serde_json::to_string(request).map_err(|err| LeanWorkerError::TypedCommandRequestEncode {
                export: command.export().to_owned(),
                message: err.to_string(),
            })?;
        let response_json = self.with_session(|worker| {
            worker.worker_json_command(command.export(), request_json, cancellation, progress)
        })?;
        serde_json::from_str(&response_json).map_err(|err| LeanWorkerError::TypedCommandResponseDecode {
            export: command.export().to_owned(),
            message: err.to_string(),
        })
    }

    /// Run a typed downstream streaming command.
    ///
    /// The Lean export must have ABI
    /// `String -> USize -> USize -> IO UInt8`. The request is serialized from
    /// `Req`; each row payload is decoded into `Row`; terminal metadata is
    /// decoded into `Summary` when present. Raw-row access remains available
    /// through `run_data_stream`.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if request encoding fails, row or summary
    /// decoding fails, the worker is dead, the session was invalidated, the
    /// export fails, cancellation or timeout is observed, a sink panics, or
    /// protocol communication fails. Row decode errors include the stream and
    /// sequence that identified the bad payload.
    pub fn run_streaming_command<Req, Row, Summary>(
        &mut self,
        command: &LeanWorkerStreamingCommand<Req, Row, Summary>,
        request: &Req,
        rows: &dyn LeanWorkerTypedDataSink<Row>,
        diagnostics: Option<&dyn LeanWorkerDiagnosticSink>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerTypedStreamSummary<Summary>, LeanWorkerError>
    where
        Req: Serialize,
        Row: DeserializeOwned,
        Summary: DeserializeOwned,
    {
        let request_value =
            serde_json::to_value(request).map_err(|err| LeanWorkerError::TypedCommandRequestEncode {
                export: command.export().to_owned(),
                message: err.to_string(),
            })?;
        let internal_cancellation = LeanWorkerCancellationToken::new();
        let cancellation_for_stream = cancellation.unwrap_or(&internal_cancellation);
        let typed_sink = TypedRawDataSink {
            export: command.export(),
            rows,
            cancellation: cancellation_for_stream,
            decode_error: std::sync::Mutex::new(None),
        };

        // `run_data_stream_raw` invalidates the session on Cancelled/Timeout
        // via `with_session`; we only reshape the result here.
        match self.run_data_stream_raw(
            command.export(),
            &request_value,
            &typed_sink,
            diagnostics,
            Some(cancellation_for_stream),
            progress,
        ) {
            Ok(summary) => {
                if let Some(err) = typed_sink.take_decode_error() {
                    return Err(err);
                }
                let metadata = summary
                    .metadata
                    .map(|metadata| {
                        serde_json::from_value(metadata).map_err(|err| LeanWorkerError::TypedCommandSummaryDecode {
                            export: command.export().to_owned(),
                            message: err.to_string(),
                        })
                    })
                    .transpose()?;
                Ok(LeanWorkerTypedStreamSummary {
                    total_rows: summary.total_rows,
                    per_stream_counts: summary.per_stream_counts,
                    elapsed: summary.elapsed,
                    metadata,
                })
            }
            Err(LeanWorkerError::Cancelled { .. }) => {
                if let Some(err) = typed_sink.take_decode_error() {
                    Err(err)
                } else {
                    Err(LeanWorkerError::Cancelled {
                        operation: "worker_run_data_stream",
                    })
                }
            }
            Err(err) => Err(err),
        }
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
    /// Centralizes the rule "Cancelled or Timeout from the worker invalidates
    /// this session" so every typed method delegates here instead of repeating
    /// the same `match` discriminator. Adding a new terminal-failure variant
    /// to the invalidation set is now a one-line edit.
    fn with_session<T>(
        &mut self,
        f: impl FnOnce(&mut LeanWorker) -> Result<T, LeanWorkerError>,
    ) -> Result<T, LeanWorkerError> {
        self.ensure_open()?;
        let result = f(self.worker);
        if matches!(
            result,
            Err(LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })
        ) {
            self.open = false;
        }
        result
    }
}

struct TypedRawDataSink<'a, Row> {
    export: &'a str,
    rows: &'a dyn LeanWorkerTypedDataSink<Row>,
    cancellation: &'a LeanWorkerCancellationToken,
    decode_error: std::sync::Mutex<Option<LeanWorkerError>>,
}

impl<Row> TypedRawDataSink<'_, Row> {
    fn take_decode_error(&self) -> Option<LeanWorkerError> {
        self.decode_error.lock().ok().and_then(|mut guard| guard.take())
    }
}

impl<Row> LeanWorkerRawDataSink for TypedRawDataSink<'_, Row>
where
    Row: DeserializeOwned,
{
    fn report(&self, row: LeanWorkerRawDataRow) {
        match serde_json::from_str(row.payload.get()) {
            Ok(payload) => self.rows.report(LeanWorkerTypedDataRow {
                stream: row.stream,
                sequence: row.sequence,
                payload,
            }),
            Err(err) => {
                if let Ok(mut guard) = self.decode_error.lock() {
                    *guard = Some(LeanWorkerError::TypedCommandRowDecode {
                        export: self.export.to_owned(),
                        stream: row.stream,
                        sequence: row.sequence,
                        message: err.to_string(),
                    });
                }
                self.cancellation.cancel();
            }
        }
    }
}

pub(crate) fn check_cancelled(
    operation: &'static str,
    token: Option<&LeanWorkerCancellationToken>,
) -> Result<(), LeanWorkerError> {
    if token.is_some_and(LeanWorkerCancellationToken::is_cancelled) {
        Err(LeanWorkerError::Cancelled { operation })
    } else {
        Ok(())
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
    sink: Option<LeanWorkerDataSinkTarget<'_>>,
    row: DataRow,
) -> Result<(), LeanWorkerError> {
    let Some(sink) = sink else {
        return Err(LeanWorkerError::Protocol {
            message: "worker sent data row for a request without a row sink".to_owned(),
        });
    };
    match sink {
        LeanWorkerDataSinkTarget::Value(sink) => {
            let row = LeanWorkerDataRow::try_from(row)?;
            report_value_data_row(sink, row)
        }
        LeanWorkerDataSinkTarget::Raw(sink) => report_raw_data_row(sink, row.into()),
    }
}

fn report_value_data_row(sink: &dyn LeanWorkerDataSink, row: LeanWorkerDataRow) -> Result<(), LeanWorkerError> {
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

pub(crate) fn report_parent_diagnostic(
    sink: Option<&dyn LeanWorkerDiagnosticSink>,
    diagnostic: LeanWorkerDiagnosticEvent,
) -> Result<(), LeanWorkerError> {
    let Some(sink) = sink else {
        return Ok(());
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.report(diagnostic))).map_err(|payload| {
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_owned()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "worker diagnostic sink panicked".to_owned()
        };
        LeanWorkerError::DiagnosticSinkPanic { message }
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
