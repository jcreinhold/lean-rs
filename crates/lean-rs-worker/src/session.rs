//! Worker-host adapter over the process-boundary supervisor.
//!
//! This module is intentionally narrower than `lean-rs-host::LeanSession`.
//! It exposes serializable outcomes that make sense across a child process:
//! declaration text, elaboration diagnostics, and kernel-check status. Runtime
//! handles such as `LeanExpr` and `LeanEvidence` stay inside the child.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::protocol::{
    DataRow, Diagnostic, StreamSummary, WorkerCapabilityFact, WorkerCapabilityMetadata, WorkerCommandMetadata,
    WorkerDiagnostic, WorkerDoctorDiagnostic, WorkerDoctorReport, WorkerDoctorSeverity, WorkerElabOptions,
    WorkerElabOutcome, WorkerKernelOutcome, WorkerKernelStatus,
};
use crate::supervisor::{LeanWorker, LeanWorkerError};

/// Configuration for opening one host session inside a worker child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerSessionConfig {
    project_root: PathBuf,
    package: String,
    lib_name: String,
    imports: Vec<String>,
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
            package: package.into(),
            lib_name: lib_name.into(),
            imports: imports.into_iter().map(Into::into).collect(),
        }
    }

    pub(crate) fn project_root_string(&self) -> String {
        self.project_root.to_string_lossy().into_owned()
    }

    pub(crate) fn package(&self) -> &str {
        &self.package
    }

    pub(crate) fn lib_name(&self) -> &str {
        &self.lib_name
    }

    pub(crate) fn imports(&self) -> &[String] {
        &self.imports
    }
}

/// Bounded elaboration options for worker-session requests.
///
/// This mirrors the stable knobs from `lean-rs-host::LeanElabOptions` without
/// exposing the in-child host object itself across the process boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerElabOptions {
    namespace_context: String,
    file_label: String,
    heartbeat_limit: u64,
    diagnostic_byte_limit: usize,
}

impl LeanWorkerElabOptions {
    /// Create worker elaboration options with `lean-rs-host` defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the namespace context.
    #[must_use]
    pub fn namespace_context(mut self, namespace: &str) -> Self {
        namespace.clone_into(&mut self.namespace_context);
        self
    }

    /// Replace the diagnostic file label.
    #[must_use]
    pub fn file_label(mut self, label: &str) -> Self {
        label.clone_into(&mut self.file_label);
        self
    }

    /// Replace the heartbeat limit. The child applies the host ceiling.
    #[must_use]
    pub fn heartbeat_limit(mut self, heartbeats: u64) -> Self {
        self.heartbeat_limit = heartbeats;
        self
    }

    /// Replace the diagnostic byte limit. The child applies the host ceiling.
    #[must_use]
    pub fn diagnostic_byte_limit(mut self, bytes: usize) -> Self {
        self.diagnostic_byte_limit = bytes;
        self
    }

    pub(crate) fn wire(&self) -> WorkerElabOptions {
        WorkerElabOptions {
            namespace_context: self.namespace_context.clone(),
            file_label: self.file_label.clone(),
            heartbeat_limit: self.heartbeat_limit,
            diagnostic_byte_limit: self.diagnostic_byte_limit,
        }
    }
}

impl Default for LeanWorkerElabOptions {
    fn default() -> Self {
        Self {
            namespace_context: String::new(),
            file_label: "<elaborate>".to_owned(),
            heartbeat_limit: lean_rs_host::LEAN_HEARTBEAT_LIMIT_DEFAULT,
            diagnostic_byte_limit: lean_rs_host::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT,
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

/// Generic metadata reported by one downstream capability package.
///
/// Command names, capability names, versions, and `extra` JSON are downstream
/// semantics. `lean-rs-worker` transports and validates the envelope; it does
/// not decide which values affect caches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerCapabilityMetadata {
    pub commands: Vec<LeanWorkerCommandMetadata>,
    pub capabilities: Vec<LeanWorkerCapabilityFact>,
    pub lean_version: Option<String>,
    pub extra: Option<Value>,
}

impl From<WorkerCapabilityMetadata> for LeanWorkerCapabilityMetadata {
    fn from(value: WorkerCapabilityMetadata) -> Self {
        Self {
            commands: value.commands.into_iter().map(Into::into).collect(),
            capabilities: value.capabilities.into_iter().map(Into::into).collect(),
            lean_version: value.lean_version,
            extra: value.extra,
        }
    }
}

/// One downstream command advertised by capability metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerCommandMetadata {
    pub name: String,
    pub version: String,
}

impl From<WorkerCommandMetadata> for LeanWorkerCommandMetadata {
    fn from(value: WorkerCommandMetadata) -> Self {
        Self {
            name: value.name,
            version: value.version,
        }
    }
}

/// One named capability advertised by capability metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerCapabilityFact {
    pub name: String,
    pub version: String,
}

impl From<WorkerCapabilityFact> for LeanWorkerCapabilityFact {
    fn from(value: WorkerCapabilityFact) -> Self {
        Self {
            name: value.name,
            version: value.version,
        }
    }
}

/// Severity for a capability doctor diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeanWorkerDoctorSeverity {
    Pass,
    Warning,
    Error,
}

impl From<WorkerDoctorSeverity> for LeanWorkerDoctorSeverity {
    fn from(value: WorkerDoctorSeverity) -> Self {
        match value {
            WorkerDoctorSeverity::Pass => Self::Pass,
            WorkerDoctorSeverity::Warning => Self::Warning,
            WorkerDoctorSeverity::Error => Self::Error,
        }
    }
}

/// One structured capability health diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerDoctorDiagnostic {
    pub severity: LeanWorkerDoctorSeverity,
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl From<WorkerDoctorDiagnostic> for LeanWorkerDoctorDiagnostic {
    fn from(value: WorkerDoctorDiagnostic) -> Self {
        Self {
            severity: value.severity.into(),
            code: value.code,
            message: value.message,
            details: value.details,
        }
    }
}

/// Capability health report returned by a downstream doctor export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerDoctorReport {
    pub diagnostics: Vec<LeanWorkerDoctorDiagnostic>,
    pub metadata: Option<Value>,
}

impl From<WorkerDoctorReport> for LeanWorkerDoctorReport {
    fn from(value: WorkerDoctorReport) -> Self {
        Self {
            diagnostics: value.diagnostics.into_iter().map(Into::into).collect(),
            metadata: value.metadata,
        }
    }
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

impl From<DataRow> for LeanWorkerDataRow {
    fn from(value: DataRow) -> Self {
        Self {
            stream: value.stream,
            sequence: value.sequence,
            payload: value.payload,
        }
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

/// Serializable elaboration result returned over the worker boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerElabResult {
    pub success: bool,
    pub diagnostics: Vec<LeanWorkerDiagnostic>,
    pub truncated: bool,
}

impl From<WorkerElabOutcome> for LeanWorkerElabResult {
    fn from(value: WorkerElabOutcome) -> Self {
        Self {
            success: value.success,
            diagnostics: value.diagnostics.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
        }
    }
}

/// Kernel-check status returned over the worker boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeanWorkerKernelStatus {
    Checked,
    Rejected,
    Unavailable,
    Unsupported,
}

/// Serializable kernel-check result returned over the worker boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerKernelResult {
    pub status: LeanWorkerKernelStatus,
    pub diagnostics: Vec<LeanWorkerDiagnostic>,
    pub truncated: bool,
}

impl From<WorkerKernelOutcome> for LeanWorkerKernelResult {
    fn from(value: WorkerKernelOutcome) -> Self {
        Self {
            status: match value.status {
                WorkerKernelStatus::Checked => LeanWorkerKernelStatus::Checked,
                WorkerKernelStatus::Rejected => LeanWorkerKernelStatus::Rejected,
                WorkerKernelStatus::Unavailable => LeanWorkerKernelStatus::Unavailable,
                WorkerKernelStatus::Unsupported => LeanWorkerKernelStatus::Unsupported,
            },
            diagnostics: value.diagnostics.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
        }
    }
}

/// Serializable diagnostic returned by worker elaboration and kernel checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerDiagnostic {
    pub severity: String,
    pub message: String,
    pub file_label: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub end_line: Option<u32>,
    pub end_column: Option<u32>,
}

impl From<WorkerDiagnostic> for LeanWorkerDiagnostic {
    fn from(value: WorkerDiagnostic) -> Self {
        Self {
            severity: value.severity,
            message: value.message,
            file_label: value.file_label,
            line: value.line,
            column: value.column,
            end_line: value.end_line,
            end_column: value.end_column,
        }
    }
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
        self.ensure_open()?;
        match self.worker.worker_elaborate(source, options, cancellation, progress) {
            Ok(value) => Ok(value),
            Err(err @ (LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })) => {
                self.open = false;
                Err(err)
            }
            Err(err) => Err(err),
        }
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
        self.ensure_open()?;
        match self.worker.worker_kernel_check(source, options, cancellation, progress) {
            Ok(value) => Ok(value),
            Err(err @ (LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })) => {
                self.open = false;
                Err(err)
            }
            Err(err) => Err(err),
        }
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
        self.ensure_open()?;
        match self.worker.worker_declaration_kinds(names, cancellation, progress) {
            Ok(value) => Ok(value),
            Err(err @ (LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })) => {
                self.open = false;
                Err(err)
            }
            Err(err) => Err(err),
        }
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
        self.ensure_open()?;
        match self.worker.worker_declaration_names(names, cancellation, progress) {
            Ok(value) => Ok(value),
            Err(err @ (LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })) => {
                self.open = false;
                Err(err)
            }
            Err(err) => Err(err),
        }
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
        self.ensure_open()?;
        match self
            .worker
            .worker_run_data_stream(export, request, rows, diagnostics, cancellation, progress)
        {
            Ok(value) => Ok(value),
            Err(err @ (LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })) => {
                self.open = false;
                Err(err)
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
        self.ensure_open()?;
        match self
            .worker
            .worker_capability_metadata(export, request, cancellation, progress)
        {
            Ok(value) => Ok(value),
            Err(err @ (LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })) => {
                self.open = false;
                Err(err)
            }
            Err(err) => Err(err),
        }
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
        self.ensure_open()?;
        match self
            .worker
            .worker_capability_doctor(export, request, cancellation, progress)
        {
            Ok(value) => Ok(value),
            Err(err @ (LeanWorkerError::Cancelled { .. } | LeanWorkerError::Timeout { .. })) => {
                self.open = false;
                Err(err)
            }
            Err(err) => Err(err),
        }
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
    sink: Option<&dyn LeanWorkerDataSink>,
    row: LeanWorkerDataRow,
) -> Result<(), LeanWorkerError> {
    let Some(sink) = sink else {
        return Err(LeanWorkerError::Protocol {
            message: "worker sent data row for a request without a row sink".to_owned(),
        });
    };
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
