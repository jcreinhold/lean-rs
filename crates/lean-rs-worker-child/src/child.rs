use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lean_rs::error::host_internal;
use lean_rs::module::{LeanBuiltCapability, LeanCapability, LeanCheckedExportError, LeanExported, LeanIo};
use lean_rs::{
    LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanError, LeanResult, LeanRuntime, LeanStringEvent,
};
use lean_rs_host::host::process::{
    DeclarationTargetInfo, DeclarationTargetResult, GoalAtResult, LocalInfo, ModuleQuery,
    ModuleQueryBatchCachedOutcome, ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    ModuleQueryCacheFacts, ModuleQueryCachePolicy, ModuleQueryCacheStatus, ModuleQueryOutcome,
    ModuleQueryOutputBudgets, ModuleQueryResult, ModuleQuerySelector, ModuleQueryTimings,
    ModuleSnapshotCacheClearResult, ModuleSourceSpan, NameRefNode, ProofStateInfo, ProofStateResult, ReferencesResult,
    RenderedInfo, SurroundingDeclarationResult, TypeAtResult,
};
use lean_rs_host::meta::{self, LeanMetaOptions, LeanMetaResponse, LeanMetaTransparency};
use lean_rs_host::{
    LeanCapabilities, LeanDeclarationFilter, LeanElabFailure, LeanElabOptions, LeanHost, LeanKernelOutcome,
    LeanSession, LeanSeverity, LeanSourceRange,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use lean_rs_worker_protocol::protocol::{
    DataRowEmitter, Diagnostic, HostSessionMode, MAX_FRAME_BYTES, Message, ProgressTick, ProtocolError, Request,
    Response, StreamSummary, read_frame, write_frame,
};
use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationSummary, LeanWorkerDeclarationTargetInfo,
    LeanWorkerDeclarationTargetResult, LeanWorkerDeclarationType, LeanWorkerDiagnostic, LeanWorkerDoctorReport,
    LeanWorkerElabFailure, LeanWorkerElabOptions, LeanWorkerElabResult, LeanWorkerGoalAtResult, LeanWorkerKernelResult,
    LeanWorkerKernelStatus, LeanWorkerKernelSummary, LeanWorkerLocalInfo, LeanWorkerMetaResult,
    LeanWorkerMetaTransparency, LeanWorkerModuleCacheStatus, LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchEnvelope,
    LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult,
    LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQueryOutcome, LeanWorkerModuleQueryResult,
    LeanWorkerModuleQuerySelector, LeanWorkerModuleQueryTimings, LeanWorkerModuleSnapshotCacheClearResult,
    LeanWorkerModuleSourceSpan, LeanWorkerNameRef, LeanWorkerOutputBudgets, LeanWorkerProofStateInfo,
    LeanWorkerProofStateResult, LeanWorkerReferencesResult, LeanWorkerRendered, LeanWorkerRenderedInfo,
    LeanWorkerRendering, LeanWorkerSourceRange, LeanWorkerSurroundingDeclarationResult, LeanWorkerTypeAtResult,
};
use lean_rs_worker_protocol::worker_exports::WorkerExportOperation;

const DECLARATION_TYPE_MAX_BYTES: usize = 64 * 1024;
const MODULE_QUERY_CACHE_API_VERSION: &str = "lean-rs.module-query-cache.v1";
const MODULE_CACHE_DEFAULT_MAX_ENTRIES: u64 = 4;
const MODULE_CACHE_DEFAULT_TTL_MILLIS: u64 = 5 * 60 * 1000;
const MODULE_CACHE_DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const MODULE_CACHE_DEFAULT_RSS_GUARD_KIB: u64 = 1024 * 1024;

#[derive(Clone)]
struct ProtocolWriter {
    stdout: Arc<Mutex<std::io::Stdout>>,
    max_frame_bytes: Arc<AtomicU32>,
}

impl ProtocolWriter {
    fn new() -> Self {
        Self {
            stdout: Arc::new(Mutex::new(std::io::stdout())),
            max_frame_bytes: Arc::new(AtomicU32::new(MAX_FRAME_BYTES)),
        }
    }

    fn set_max_frame_bytes(&self, value: u32) {
        self.max_frame_bytes.store(value, Ordering::Release);
    }

    fn max_frame_bytes(&self) -> u32 {
        self.max_frame_bytes.load(Ordering::Acquire)
    }

    fn write(&self, message: Message) -> Result<(), ProtocolError> {
        let cap = self.max_frame_bytes();
        let mut stdout = self
            .stdout
            .lock()
            .map_err(|_| ProtocolError::Io(std::io::Error::other("worker stdout mutex was poisoned")))?;
        write_frame(&mut *stdout, message, cap)
    }
}

fn write_response(writer: &ProtocolWriter, response: Response) -> Result<(), ProtocolError> {
    match writer.write(Message::Response(response)) {
        Ok(()) => Ok(()),
        Err(ProtocolError::FrameTooLarge { len, max }) => writer.write(Message::Response(Response::Error {
            code: "lean_rs.worker.output_frame_too_large".to_owned(),
            message: format!("worker response frame too large: {len} bytes exceeds {max}"),
        })),
        Err(err) => Err(err),
    }
}

pub(crate) fn run_stdio() -> ExitCode {
    install_immediate_abort_exit();
    match serve_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("lean-rs-worker-child: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Convert any `SIGABRT` the worker child receives into an immediate
/// `_exit(134)`, bypassing kernel core-dump machinery and any libc/runtime
/// residual cleanup on `abort()`.
///
/// Lean internal panics with `LEAN_ABORT_ON_PANIC=1` (the worker child's
/// default) terminate the child via `abort()` → `SIGABRT`. The kernel's
/// default action for `SIGABRT` is *terminate with core dump*, and on
/// GitHub Actions `ubuntu-latest` the inherited `core_pattern` pipes the
/// image to `apport` (or `systemd-coredump`). The kernel holds the dying
/// child's file descriptors open while the handler drains the pipe; for a
/// child that has loaded `libleanshared.so` plus a capability dylib chain,
/// the observed delay is tens of seconds, long enough that the parent
/// supervisor's per-request timeout fires before it can see EOF on the
/// child's stdout and translate it to `LeanWorkerError::ChildPanicOrAbort`.
///
/// `setrlimit(RLIMIT_CORE, 0)` and `prctl(PR_SET_DUMPABLE, 0)` are
/// independently advertised as "no core dump" knobs; in practice on the
/// `ubuntu-latest` runner they reduce the delay substantially but do not
/// eliminate it (observed: ~107 s without either, ~23 s with `setrlimit`
/// alone — still above the supervisor's 30 s budget). The decisive fix is
/// to take over `SIGABRT` ourselves: a `sigaction` handler that calls
/// `_exit(134)` short-circuits the entire kernel signal-default path,
/// closes the pipes immediately, and lets the parent observe the fatal
/// exit on normal IPC timescales.
///
/// The diagnostic the parent surfaces to callers does not include a core
/// file in any supported configuration: typed errors (`ChildPanicOrAbort`,
/// `Worker { code, message }`) and the captured child stderr cover the
/// supported failure surface. Worker children therefore have no use for
/// core dumps, and suppressing them is the right boundary policy.
///
/// We also keep the `RLIMIT_CORE` and `PR_SET_DUMPABLE` knobs as a
/// defence-in-depth: if anything later in the child's lifetime overwrites
/// the `SIGABRT` handler (e.g. a future Lean runtime that installs its own
/// signal handler during init), the kernel default action then runs but
/// the core-dump step is still skipped, preserving the post-`setrlimit`
/// timing rather than regressing to the unfixed ~107 s.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "installing a signal handler and calling setrlimit/prctl require libc FFI"
)]
fn install_immediate_abort_exit() {
    extern "C" fn on_sigabrt(_sig: libc::c_int) {
        // SAFETY: `write` and `_exit` are async-signal-safe per POSIX.
        // The marker lets test stderr distinguish this exit path from a
        // raw kernel-default `SIGABRT` termination.
        const MARKER: &[u8] = b"lean-rs-worker child: SIGABRT, exiting immediately\n";
        unsafe {
            let _ = libc::write(libc::STDERR_FILENO, MARKER.as_ptr().cast(), MARKER.len());
            libc::_exit(134);
        }
    }

    // SAFETY: zero-initialised `sigaction` is valid; we then populate the
    // handler and flags fields. The call modifies process-global state only
    // and has no aliasing or lifetime concerns. The handler itself uses
    // only async-signal-safe calls.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigabrt as *const () as libc::sighandler_t;
        libc::sigemptyset(&raw mut action.sa_mask);
        action.sa_flags = libc::SA_RESETHAND;
        let _ = libc::sigaction(libc::SIGABRT, &raw const action, std::ptr::null_mut());
    }

    // SAFETY: defence-in-depth. `setrlimit` and `prctl` modify
    // process-global state only and have no aliasing or lifetime concerns.
    // Return values are deliberately ignored: the worst case is that the
    // OS does not honour the request and we fall back on the `sigaction`
    // handler installed above.
    unsafe {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let _ = libc::setrlimit(libc::RLIMIT_CORE, &raw const limit);
        #[cfg(target_os = "linux")]
        {
            let zero: libc::c_ulong = 0;
            let _ = libc::prctl(libc::PR_SET_DUMPABLE, zero, zero, zero, zero);
        }
    }
}

#[cfg(not(unix))]
fn install_immediate_abort_exit() {}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the child owns stdin/stdout for the full protocol loop"
)]
fn serve_stdio() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = LeanRuntime::init()?;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let writer = ProtocolWriter::new();
    let mut host_session: Option<HostSessionState> = None;

    writer.write(Message::Handshake {
        worker_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_version: lean_rs_worker_protocol::protocol::PROTOCOL_VERSION,
    })?;

    // The first frame from the parent after the handshake must be a
    // ConfigureFrameLimit announcing the per-connection cap the parent has
    // clamped. Both directions use that cap for the remainder of this
    // connection. Read it with the default cap (every clamped value is
    // already well below `MAX_FRAME_BYTES`).
    let configure_frame = read_frame(&mut reader, MAX_FRAME_BYTES)?;
    let Message::ConfigureFrameLimit { max_frame_bytes } = configure_frame.message else {
        return Err(Box::new(std::io::Error::other(format!(
            "worker child expected ConfigureFrameLimit, got {:?}",
            configure_frame.message
        ))));
    };
    writer.set_max_frame_bytes(max_frame_bytes);

    loop {
        let frame = read_frame(&mut reader, writer.max_frame_bytes())?;
        let Message::Request(request) = frame.message else {
            write_response(
                &writer,
                Response::Error {
                    code: "lean_rs.worker.protocol.unexpected_frame".to_owned(),
                    message: "child expected request frame".to_owned(),
                },
            )?;
            continue;
        };

        match request {
            Request::Health => {
                write_response(&writer, Response::HealthOk)?;
            }
            Request::LoadFixtureCapability { manifest_path } => {
                let response = match load_fixture_capability(runtime, Path::new(&manifest_path)) {
                    Ok(()) => Response::CapabilityLoaded,
                    Err(err) => error_response(&err),
                };
                write_response(&writer, response)?;
            }
            Request::CallFixtureMul {
                manifest_path,
                lhs,
                rhs,
            } => {
                let response = match call_fixture_mul(runtime, Path::new(&manifest_path), lhs, rhs) {
                    Ok(value) => Response::U64 { value },
                    Err(WorkerCallError::Host(err)) => error_response(&err),
                    Err(WorkerCallError::Binding(err)) => binding_error_response(&err),
                };
                write_response(&writer, response)?;
            }
            Request::TriggerLeanPanic { manifest_path } => {
                let response = match trigger_lean_panic(runtime, Path::new(&manifest_path)) {
                    Ok(()) => Response::Error {
                        code: "lean_rs.worker.panic_fixture_returned".to_owned(),
                        message: "Lean panic fixture returned instead of terminating the child".to_owned(),
                    },
                    Err(WorkerCallError::Host(err)) => error_response(&err),
                    Err(WorkerCallError::Binding(err)) => binding_error_response(&err),
                };
                write_response(&writer, response)?;
            }
            Request::OpenHostSession {
                project_root,
                mode,
                imports,
            } => {
                let response = match HostSessionState::open(runtime, &project_root, &mode, &imports) {
                    Ok(mut state) => {
                        drop(state.clear_module_snapshot_cache());
                        host_session = Some(state);
                        Response::HostSessionOpened
                    }
                    Err(err) => error_response(&err),
                };
                write_response(&writer, response)?;
            }
            Request::Elaborate { source, options } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.elaborate(&source, &options) {
                        Ok(outcome) => Response::Elaboration { outcome },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::KernelCheck {
                source,
                options,
                progress,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.kernel_check(&source, &options, progress, &writer) {
                        Ok(outcome) => Response::KernelCheck { outcome },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::DeclarationKinds { names, progress } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.declaration_kinds(&names, progress, &writer) {
                        Ok(values) => Response::Strings { values },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::DeclarationNames { names, progress } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.declaration_names(&names, progress, &writer) {
                        Ok(values) => Response::Strings { values },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::RunDataStream {
                export,
                request_json,
                progress,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.run_data_stream(&export, request_json, progress, &writer) {
                        Ok(summary) => Response::StreamComplete { summary },
                        Err(StreamRunError::Host(err)) => error_response(&err),
                        Err(StreamRunError::Binding(err)) => binding_error_response(&err),
                        Err(StreamRunError::ExportStatus(status)) => {
                            Response::StreamExportFailed { status_byte: status }
                        }
                        Err(StreamRunError::CallbackStatus(status)) => Response::StreamCallbackFailed {
                            status_byte: status.as_abi(),
                            description: status.description().to_owned(),
                        },
                        Err(StreamRunError::MalformedRow(message)) => Response::StreamRowMalformed { message },
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::CapabilityMetadata { export, request_json } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.capability_metadata(&export, request_json) {
                        Ok(metadata) => Response::CapabilityMetadata { metadata },
                        Err(CapabilityJsonError::Binding(err)) => binding_error_response(&err),
                        Err(CapabilityJsonError::Host(err)) => error_response(&err),
                        Err(CapabilityJsonError::Malformed(message)) => {
                            Response::CapabilityMetadataMalformed { message }
                        }
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::CapabilityDoctor { export, request_json } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.capability_doctor(&export, request_json) {
                        Ok(report) => Response::CapabilityDoctor { report },
                        Err(CapabilityJsonError::Binding(err)) => binding_error_response(&err),
                        Err(CapabilityJsonError::Host(err)) => error_response(&err),
                        Err(CapabilityJsonError::Malformed(message)) => Response::CapabilityDoctorMalformed { message },
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::JsonCommand { export, request_json } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.json_command(&export, request_json) {
                        Ok(response_json) => Response::JsonCommand { response_json },
                        Err(WorkerCallError::Host(err)) => error_response(&err),
                        Err(WorkerCallError::Binding(err)) => binding_error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::InferType { source, options } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.infer_type(&source, &options) {
                        Ok(result) => Response::MetaExpr { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::Whnf { source, options } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.whnf(&source, &options) {
                        Ok(result) => Response::MetaExpr { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::IsDefEq {
                lhs,
                rhs,
                transparency,
                options,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.is_def_eq(&lhs, &rhs, transparency, &options) {
                        Ok(result) => Response::MetaBool { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::Describe { name } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.describe(&name) {
                        Ok(row) => Response::Declaration { row },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::SearchDeclarations { search } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.search_declarations(&search) {
                        Ok(result) => Response::DeclarationSearch { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::DeclarationType { name, max_bytes } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.declaration_type(&name, max_bytes) {
                        Ok(row) => Response::DeclarationType { row },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::ListDeclarationsStrings { filter, progress } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.list_declarations_strings(filter, progress, &writer) {
                        Ok(count) => Response::RowsComplete { count },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::DescribeBulk { names, progress } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.describe_bulk(&names, progress, &writer) {
                        Ok(rows) => Response::DeclarationBulk { rows },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::ProcessModuleQuery { source, query, options } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.process_module_query(&source, query, &options) {
                        Ok(outcome) => Response::ProcessModuleQuery { outcome },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::ProcessModuleQueryBatch {
                source,
                selectors,
                budgets,
                options,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.process_module_query_batch(&source, &selectors, &budgets, &options) {
                        Ok(outcome) => Response::ProcessModuleQueryBatch { outcome },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::ClearModuleSnapshotCache => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.clear_module_snapshot_cache() {
                        Ok(result) => Response::ModuleSnapshotCacheCleared { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::EmitTestRows { streams } => {
                let count = emit_test_rows(&writer, &streams)?;
                write_response(&writer, Response::RowsComplete { count })?;
            }
            Request::EmitTestRowsThenExit => {
                let _count = emit_test_rows(&writer, &["rows".to_owned()])?;
                return Ok(());
            }
            Request::EmitTestRowsThenPanic => {
                let _count = emit_test_rows(&writer, &["rows".to_owned()])?;
                std::process::abort();
            }
            Request::Terminate => {
                write_response(&writer, Response::Terminating)?;
                return Ok(());
            }
            #[allow(
                clippy::wildcard_enum_match_arm,
                reason = "Request is #[non_exhaustive] across the lean-rs-worker-protocol crate boundary; unrecognised variants come from a newer parent than this child supports and are surfaced as a protocol error"
            )]
            _ => {
                write_response(
                    &writer,
                    Response::Error {
                        code: "lean_rs.worker.protocol.unknown_request".to_owned(),
                        message: "child received a request variant it does not recognise".to_owned(),
                    },
                )?;
            }
        }
    }
}

fn load_fixture_capability(runtime: &'static LeanRuntime, manifest_path: &Path) -> LeanResult<()> {
    let _capability = LeanCapability::from_build_manifest(runtime, LeanBuiltCapability::manifest_path(manifest_path))?;
    Ok(())
}

fn call_fixture_mul(
    runtime: &'static LeanRuntime,
    manifest_path: &Path,
    lhs: u64,
    rhs: u64,
) -> Result<u64, WorkerCallError> {
    let capability = Box::leak(Box::new(LeanCapability::from_build_manifest(
        runtime,
        LeanBuiltCapability::manifest_path(manifest_path),
    )?));
    let mut bindings = WorkerCapabilityBindings::new(capability);
    bindings
        .fixture_mul("lean_rs_fixture_u64_mul")?
        .call(lhs, rhs)
        .map_err(WorkerCallError::Host)
}

fn trigger_lean_panic(runtime: &'static LeanRuntime, manifest_path: &Path) -> Result<(), WorkerCallError> {
    let capability = Box::leak(Box::new(LeanCapability::from_build_manifest(
        runtime,
        LeanBuiltCapability::manifest_path(manifest_path),
    )?));
    let mut bindings = WorkerCapabilityBindings::new(capability);
    bindings
        .fixture_panic("lean_rs_fixture_panic_unit")?
        .call(0)
        .map_err(WorkerCallError::Host)
}

fn error_response(err: &LeanError) -> Response {
    Response::Error {
        code: err.code().as_str().to_owned(),
        message: err.to_string(),
    }
}

fn binding_error_response(err: &WorkerBindingError) -> Response {
    Response::Error {
        code: "lean_rs.worker.checked_binding".to_owned(),
        message: err.to_string(),
    }
}

fn missing_session_response() -> Response {
    Response::Error {
        code: "lean_rs.worker.session_missing".to_owned(),
        message: "open a LeanWorkerSession before sending host-session requests".to_owned(),
    }
}

struct HostSessionState {
    #[allow(dead_code, reason = "leaked host anchors the capability and session lifetimes")]
    host: &'static LeanHost<'static>,
    #[allow(dead_code, reason = "leaked capabilities anchor the session borrow")]
    capabilities: &'static LeanCapabilities<'static, 'static>,
    worker_bindings: Option<WorkerCapabilityBindings>,
    session: LeanSession<'static, 'static>,
    imports: Vec<String>,
}

struct WorkerCapabilityBindings {
    capability: &'static LeanCapability<'static>,
    string_io: HashMap<String, LeanExported<'static, 'static, (String,), LeanIo<String>>>,
    streams: HashMap<String, LeanExported<'static, 'static, (String, usize, usize), LeanIo<u8>>>,
    fixture_mul: HashMap<String, LeanExported<'static, 'static, (u64, u64), u64>>,
    fixture_panic: HashMap<String, LeanExported<'static, 'static, (u8,), ()>>,
}

impl WorkerCapabilityBindings {
    fn new(capability: &'static LeanCapability<'static>) -> Self {
        Self {
            capability,
            string_io: HashMap::new(),
            streams: HashMap::new(),
            fixture_mul: HashMap::new(),
            fixture_panic: HashMap::new(),
        }
    }

    fn string_io(
        &mut self,
        operation: WorkerExportOperation,
        export: &str,
    ) -> Result<&LeanExported<'static, 'static, (String,), LeanIo<String>>, WorkerCallError> {
        if !self.string_io.contains_key(export) {
            let binding = self
                .capability
                .exported::<(String,), LeanIo<String>>(export)
                .map_err(|source| WorkerBindingError::checked(operation, export, source))?;
            self.string_io.insert(export.to_owned(), binding);
        }
        self.string_io.get(export).ok_or_else(|| {
            WorkerCallError::Binding(WorkerBindingError::internal(
                operation,
                export,
                "checked worker String -> IO String binding cache missed after insertion",
            ))
        })
    }

    fn stream(
        &mut self,
        export: &str,
    ) -> Result<&LeanExported<'static, 'static, (String, usize, usize), LeanIo<u8>>, WorkerCallError> {
        if !self.streams.contains_key(export) {
            let binding = self
                .capability
                .exported::<(String, usize, usize), LeanIo<u8>>(export)
                .map_err(|source| {
                    WorkerBindingError::checked(WorkerExportOperation::StreamingCommand, export, source)
                })?;
            self.streams.insert(export.to_owned(), binding);
        }
        self.streams.get(export).ok_or_else(|| {
            WorkerCallError::Binding(WorkerBindingError::internal(
                WorkerExportOperation::StreamingCommand,
                export,
                "checked worker streaming binding cache missed after insertion",
            ))
        })
    }

    fn fixture_mul(
        &mut self,
        export: &str,
    ) -> Result<&LeanExported<'static, 'static, (u64, u64), u64>, WorkerCallError> {
        if !self.fixture_mul.contains_key(export) {
            let binding = self
                .capability
                .exported::<(u64, u64), u64>(export)
                .map_err(|source| WorkerBindingError::checked(WorkerExportOperation::FixtureMul, export, source))?;
            self.fixture_mul.insert(export.to_owned(), binding);
        }
        self.fixture_mul.get(export).ok_or_else(|| {
            WorkerCallError::Binding(WorkerBindingError::internal(
                WorkerExportOperation::FixtureMul,
                export,
                "checked fixture multiplication binding cache missed after insertion",
            ))
        })
    }

    fn fixture_panic(&mut self, export: &str) -> Result<&LeanExported<'static, 'static, (u8,), ()>, WorkerCallError> {
        if !self.fixture_panic.contains_key(export) {
            let binding = self
                .capability
                .exported::<(u8,), ()>(export)
                .map_err(|source| WorkerBindingError::checked(WorkerExportOperation::FixturePanic, export, source))?;
            self.fixture_panic.insert(export.to_owned(), binding);
        }
        self.fixture_panic.get(export).ok_or_else(|| {
            WorkerCallError::Binding(WorkerBindingError::internal(
                WorkerExportOperation::FixturePanic,
                export,
                "checked fixture panic binding cache missed after insertion",
            ))
        })
    }
}

#[derive(Debug)]
struct WorkerBindingError {
    operation: WorkerExportOperation,
    export: String,
    source: WorkerBindingErrorSource,
}

#[derive(Debug)]
enum WorkerBindingErrorSource {
    Checked(LeanCheckedExportError),
    Internal(String),
}

impl WorkerBindingError {
    fn checked(operation: WorkerExportOperation, export: &str, source: LeanCheckedExportError) -> WorkerCallError {
        WorkerCallError::Binding(Self {
            operation,
            export: export.to_owned(),
            source: WorkerBindingErrorSource::Checked(source),
        })
    }

    fn internal(operation: WorkerExportOperation, export: &str, message: impl Into<String>) -> Self {
        Self {
            operation,
            export: export.to_owned(),
            source: WorkerBindingErrorSource::Internal(message.into()),
        }
    }
}

impl std::fmt::Display for WorkerBindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            WorkerBindingErrorSource::Checked(source) => write!(
                f,
                "failed to resolve checked worker export '{}' for {}: {}",
                self.export,
                self.operation.as_str(),
                source
            ),
            WorkerBindingErrorSource::Internal(message) => write!(
                f,
                "failed to resolve checked worker export '{}' for {}: {}",
                self.export,
                self.operation.as_str(),
                message
            ),
        }
    }
}

enum WorkerCallError {
    Host(LeanError),
    Binding(WorkerBindingError),
}

impl From<LeanError> for WorkerCallError {
    fn from(value: LeanError) -> Self {
        Self::Host(value)
    }
}

impl HostSessionState {
    fn open(
        runtime: &'static LeanRuntime,
        project_root: &str,
        mode: &HostSessionMode,
        imports: &[String],
    ) -> LeanResult<Self> {
        let host = Box::leak(Box::new(LeanHost::from_lake_project(runtime, Path::new(project_root))?));
        let (capabilities, worker_bindings) = match mode {
            HostSessionMode::Capability {
                package,
                lib_name,
                manifest_path,
            } => {
                let worker_bindings = match manifest_path {
                    Some(path) => {
                        let capability = Box::leak(Box::new(LeanCapability::from_build_manifest(
                            runtime,
                            LeanBuiltCapability::manifest_path(path),
                        )?));
                        Some(WorkerCapabilityBindings::new(capability))
                    }
                    None => None,
                };
                let capabilities = if worker_bindings.is_some() {
                    Box::leak(Box::new(host.load_shims_only()?))
                } else {
                    Box::leak(Box::new(host.load_capabilities(package, lib_name)?))
                };
                (capabilities, worker_bindings)
            }
            HostSessionMode::ShimsOnly => (Box::leak(Box::new(host.load_shims_only()?)), None),
            _ => {
                return Err(host_internal(
                    "worker child received an unsupported host session loading mode".to_owned(),
                ));
            }
        };
        let import_refs: Vec<&str> = imports.iter().map(String::as_str).collect();
        let session = capabilities.session(&import_refs, None, None)?;
        Ok(Self {
            host,
            capabilities,
            worker_bindings,
            session,
            imports: imports.to_vec(),
        })
    }

    fn worker_bindings_for(
        &mut self,
        operation: WorkerExportOperation,
        export: &str,
    ) -> Result<&mut WorkerCapabilityBindings, WorkerCallError> {
        self.worker_bindings.as_mut().ok_or_else(|| {
            WorkerCallError::Binding(WorkerBindingError::internal(
                operation,
                export,
                "worker request requires a manifest-backed capability identity, but this session has no capability manifest",
            ))
        })
    }

    fn elaborate(&mut self, source: &str, options: &LeanWorkerElabOptions) -> LeanResult<LeanWorkerElabResult> {
        let options = elab_options_to_host(options);
        let outcome = self.session.elaborate(source, None, &options, None)?;
        Ok(match outcome {
            Ok(_expr) => LeanWorkerElabResult {
                success: true,
                diagnostics: Vec::new(),
                truncated: false,
            },
            Err(failure) => elab_failure_outcome(&failure),
        })
    }

    fn kernel_check(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<LeanWorkerKernelResult> {
        if progress {
            emit_progress(writer, "kernel_check", 0, Some(1));
        }
        let options = elab_options_to_host(options);
        let outcome = self.session.kernel_check(source, &options, None, None)?;
        if progress {
            emit_progress(writer, "kernel_check", 1, Some(1));
        }
        Ok(match outcome {
            LeanKernelOutcome::Checked(evidence) => {
                let summary = self.session.summarize_evidence(&evidence, None)?;
                LeanWorkerKernelResult {
                    status: LeanWorkerKernelStatus::Checked,
                    diagnostics: Vec::new(),
                    truncated: false,
                    summary: Some(LeanWorkerKernelSummary {
                        declaration_name: summary.declaration_name().to_owned(),
                        kind: summary.kind().to_owned(),
                        type_signature: summary.type_signature().to_owned(),
                    }),
                }
            }
            LeanKernelOutcome::Rejected(failure) => kernel_failure_outcome(LeanWorkerKernelStatus::Rejected, &failure),
            LeanKernelOutcome::Unavailable(failure) => {
                kernel_failure_outcome(LeanWorkerKernelStatus::Unavailable, &failure)
            }
            LeanKernelOutcome::Unsupported(failure) => {
                kernel_failure_outcome(LeanWorkerKernelStatus::Unsupported, &failure)
            }
        })
    }

    fn declaration_kinds(
        &mut self,
        names: &[String],
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<Vec<String>> {
        if progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(names.len());
            for (idx, name) in names.iter().enumerate() {
                out.push(self.session.declaration_kind(name, None)?);
                emit_progress(
                    writer,
                    "declaration_kind_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                );
            }
            Ok(out)
        } else {
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.session.declaration_kind_bulk(&refs, None, None)
        }
    }

    fn declaration_names(
        &mut self,
        names: &[String],
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<Vec<String>> {
        if progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(names.len());
            for (idx, name) in names.iter().enumerate() {
                out.push(self.session.declaration_name(name, None)?);
                emit_progress(
                    writer,
                    "declaration_name_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                );
            }
            Ok(out)
        } else {
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.session.declaration_name_bulk(&refs, None, None)
        }
    }

    fn run_data_stream(
        &mut self,
        export: &str,
        request_json: String,
        progress: bool,
        writer: &ProtocolWriter,
    ) -> Result<StreamSummary, StreamRunError> {
        if progress {
            emit_progress(writer, "data_stream", 0, None);
        }

        let started = Instant::now();
        let forwarder = Arc::new(Mutex::new(StreamForwarder::new(writer.clone(), progress)));
        let row_error = Arc::new(Mutex::new(None::<StreamCallbackError>));
        let callback_forwarder = Arc::clone(&forwarder);
        let callback_error = Arc::clone(&row_error);
        let callback = LeanCallbackHandle::<LeanStringEvent>::register(move |event| {
            if callback_error.lock().map_or(true, |guard| guard.is_some()) {
                return LeanCallbackFlow::Stop;
            }
            match parse_row_envelope(&event.value) {
                Ok(StreamCallbackEvent::Row(row)) => match callback_forwarder.lock() {
                    Ok(mut guard) => match guard.emit_row(row) {
                        Ok(()) => LeanCallbackFlow::Continue,
                        Err(err) => {
                            if let Ok(mut guard) = callback_error.lock() {
                                *guard = Some(StreamCallbackError::Write(err.to_string()));
                            }
                            LeanCallbackFlow::Stop
                        }
                    },
                    Err(_) => {
                        if let Ok(mut guard) = callback_error.lock() {
                            *guard = Some(StreamCallbackError::Malformed(
                                "stream forwarder mutex was poisoned".to_owned(),
                            ));
                        }
                        LeanCallbackFlow::Stop
                    }
                },
                Ok(StreamCallbackEvent::Diagnostic(diagnostic)) => match callback_forwarder.lock() {
                    Ok(guard) => match guard.emit_diagnostic(diagnostic) {
                        Ok(()) => LeanCallbackFlow::Continue,
                        Err(err) => {
                            if let Ok(mut guard) = callback_error.lock() {
                                *guard = Some(StreamCallbackError::Write(err.to_string()));
                            }
                            LeanCallbackFlow::Stop
                        }
                    },
                    Err(_) => {
                        if let Ok(mut guard) = callback_error.lock() {
                            *guard = Some(StreamCallbackError::Malformed(
                                "stream forwarder mutex was poisoned".to_owned(),
                            ));
                        }
                        LeanCallbackFlow::Stop
                    }
                },
                Ok(StreamCallbackEvent::Progress(progress)) => match callback_forwarder.lock() {
                    Ok(guard) => match guard.emit_progress(progress) {
                        Ok(()) => LeanCallbackFlow::Continue,
                        Err(err) => {
                            if let Ok(mut guard) = callback_error.lock() {
                                *guard = Some(StreamCallbackError::Write(err.to_string()));
                            }
                            LeanCallbackFlow::Stop
                        }
                    },
                    Err(_) => {
                        if let Ok(mut guard) = callback_error.lock() {
                            *guard = Some(StreamCallbackError::Malformed(
                                "stream forwarder mutex was poisoned".to_owned(),
                            ));
                        }
                        LeanCallbackFlow::Stop
                    }
                },
                Ok(StreamCallbackEvent::Metadata(metadata)) => match callback_forwarder.lock() {
                    Ok(mut guard) => {
                        guard.set_metadata(metadata);
                        LeanCallbackFlow::Continue
                    }
                    Err(_) => {
                        if let Ok(mut guard) = callback_error.lock() {
                            *guard = Some(StreamCallbackError::Malformed(
                                "stream forwarder mutex was poisoned".to_owned(),
                            ));
                        }
                        LeanCallbackFlow::Stop
                    }
                },
                Err(message) => {
                    if let Ok(mut guard) = callback_error.lock() {
                        *guard = Some(StreamCallbackError::Malformed(message));
                    }
                    LeanCallbackFlow::Stop
                }
            }
        })
        .map_err(StreamRunError::Host)?;

        let (handle, trampoline) = callback.abi_parts();
        let status = self
            .worker_bindings_for(WorkerExportOperation::StreamingCommand, export)
            .map_err(StreamRunError::from_worker_call)?
            .stream(export)
            .map_err(StreamRunError::from_worker_call)?
            .call(request_json, handle, trampoline)
            .map_err(StreamRunError::Host)?;

        if let Some(error) = row_error.lock().ok().and_then(|mut guard| guard.take()) {
            return Err(match error {
                StreamCallbackError::Malformed(message) => StreamRunError::MalformedRow(message),
                StreamCallbackError::Write(message) => {
                    StreamRunError::Host(host_internal(format!("worker stream frame write failed: {message}")))
                }
            });
        }

        match LeanCallbackStatus::from_abi(status) {
            Some(LeanCallbackStatus::Ok) => {}
            Some(status) => return Err(StreamRunError::CallbackStatus(status)),
            None => return Err(StreamRunError::ExportStatus(status)),
        }

        let guard = forwarder
            .lock()
            .map_err(|_| StreamRunError::MalformedRow("stream forwarder mutex was poisoned".to_owned()))?;
        Ok(guard.summary(started.elapsed()))
    }

    fn capability_metadata(
        &mut self,
        export: &str,
        request_json: String,
    ) -> Result<LeanWorkerCapabilityMetadata, CapabilityJsonError> {
        let raw = self
            .worker_bindings_for(WorkerExportOperation::Metadata, export)
            .map_err(CapabilityJsonError::from_worker_call)?
            .string_io(WorkerExportOperation::Metadata, export)
            .map_err(CapabilityJsonError::from_worker_call)?
            .call(request_json)
            .map_err(CapabilityJsonError::Host)?;
        serde_json::from_str(&raw).map_err(|err| CapabilityJsonError::Malformed(err.to_string()))
    }

    fn capability_doctor(
        &mut self,
        export: &str,
        request_json: String,
    ) -> Result<LeanWorkerDoctorReport, CapabilityJsonError> {
        let raw = self
            .worker_bindings_for(WorkerExportOperation::Doctor, export)
            .map_err(CapabilityJsonError::from_worker_call)?
            .string_io(WorkerExportOperation::Doctor, export)
            .map_err(CapabilityJsonError::from_worker_call)?
            .call(request_json)
            .map_err(CapabilityJsonError::Host)?;
        serde_json::from_str(&raw).map_err(|err| CapabilityJsonError::Malformed(err.to_string()))
    }

    fn json_command(&mut self, export: &str, request_json: String) -> Result<String, WorkerCallError> {
        self.worker_bindings_for(WorkerExportOperation::JsonCommand, export)?
            .string_io(WorkerExportOperation::JsonCommand, export)?
            .call(request_json)
            .map_err(WorkerCallError::Host)
    }

    fn infer_type(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
    ) -> LeanResult<LeanWorkerMetaResult<LeanWorkerRendered>> {
        let elab_options = elab_options_to_host(options);
        let elab_outcome = self.session.elaborate(source, None, &elab_options, None)?;
        let expr = match elab_outcome {
            Ok(expr) => expr,
            Err(failure) => return Ok(meta_failure_from_elab(&failure)),
        };
        let meta_options = elab_options_to_host_meta(options, LeanMetaTransparency::Default);
        let response = self.session.run_meta(&meta::infer_type(), expr, &meta_options, None)?;
        meta_render_expr(&mut self.session, response, &meta_options)
    }

    fn whnf(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
    ) -> LeanResult<LeanWorkerMetaResult<LeanWorkerRendered>> {
        let elab_options = elab_options_to_host(options);
        let elab_outcome = self.session.elaborate(source, None, &elab_options, None)?;
        let expr = match elab_outcome {
            Ok(expr) => expr,
            Err(failure) => return Ok(meta_failure_from_elab(&failure)),
        };
        let meta_options = elab_options_to_host_meta(options, LeanMetaTransparency::Default);
        let response = self.session.run_meta(&meta::whnf(), expr, &meta_options, None)?;
        meta_render_expr(&mut self.session, response, &meta_options)
    }

    fn is_def_eq(
        &mut self,
        lhs: &str,
        rhs: &str,
        transparency: LeanWorkerMetaTransparency,
        options: &LeanWorkerElabOptions,
    ) -> LeanResult<LeanWorkerMetaResult<bool>> {
        let elab_options = elab_options_to_host(options);
        let lhs_outcome = self.session.elaborate(lhs, None, &elab_options, None)?;
        let lhs_expr = match lhs_outcome {
            Ok(expr) => expr,
            Err(failure) => return Ok(meta_failure_from_elab(&failure)),
        };
        let rhs_outcome = self.session.elaborate(rhs, None, &elab_options, None)?;
        let rhs_expr = match rhs_outcome {
            Ok(expr) => expr,
            Err(failure) => return Ok(meta_failure_from_elab(&failure)),
        };
        let transparency_host = meta_transparency_to_host(transparency);
        let meta_options = elab_options_to_host_meta(options, transparency_host);
        let response = self.session.run_meta(
            &meta::is_def_eq(),
            (lhs_expr, rhs_expr, transparency_host),
            &meta_options,
            None,
        )?;
        match response {
            LeanMetaResponse::Ok(value) => Ok(LeanWorkerMetaResult::Ok { value }),
            LeanMetaResponse::Failed(failure) => Ok(LeanWorkerMetaResult::Failed {
                failure: elab_failure_wire(&failure),
            }),
            LeanMetaResponse::TimeoutOrHeartbeat(failure) => Ok(LeanWorkerMetaResult::TimeoutOrHeartbeat {
                failure: elab_failure_wire(&failure),
            }),
            LeanMetaResponse::Unsupported(failure) => Ok(LeanWorkerMetaResult::Unsupported {
                failure: elab_failure_wire(&failure),
            }),
        }
    }

    fn describe(&mut self, name: &str) -> LeanResult<Option<LeanWorkerDeclarationRow>> {
        let kind = self.session.declaration_kind(name, None)?;
        if kind == "missing" {
            return Ok(None);
        }
        let type_signature = match self.session.declaration_type(name, None)? {
            Some(expr) => Some(self.session.expr_to_string_raw(&expr, None)?),
            None => None,
        };
        let source = self
            .session
            .declaration_source_range(name, None)?
            .map(source_range_wire);
        Ok(Some(LeanWorkerDeclarationRow {
            name: name.to_owned(),
            kind,
            type_signature,
            source,
        }))
    }

    fn search_declarations(
        &mut self,
        search: &LeanWorkerDeclarationSearch,
    ) -> LeanResult<LeanWorkerDeclarationSearchResult> {
        let host_filter = LeanDeclarationFilter {
            include_private: search.filter.include_private,
            include_generated: search.filter.include_generated,
            include_internal: search.filter.include_internal,
        };
        let needle = search.query.to_lowercase();
        let kind_filter = search.kind.as_deref();
        let limit = search.limit.clamp(1, 100);
        let names = self.session.list_declarations_strings(&host_filter, None, None)?;
        let mut declarations = Vec::with_capacity(limit);
        let mut truncated = false;
        for name in names {
            if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
                continue;
            }
            let kind = self.session.declaration_kind(&name, None)?;
            if kind_filter.is_some_and(|wanted| kind != wanted) {
                continue;
            }
            if declarations.len() >= limit {
                truncated = true;
                break;
            }
            let source = if search.include_source {
                self.session
                    .declaration_source_range(&name, None)?
                    .map(source_range_wire)
            } else {
                None
            };
            declarations.push(LeanWorkerDeclarationSummary { name, kind, source });
        }
        Ok(LeanWorkerDeclarationSearchResult {
            declarations,
            truncated,
        })
    }

    fn declaration_type(&mut self, name: &str, max_bytes: usize) -> LeanResult<Option<LeanWorkerDeclarationType>> {
        let kind = self.session.declaration_kind(name, None)?;
        if kind == "missing" {
            return Ok(None);
        }
        let type_signature = match self.session.declaration_type(name, None)? {
            Some(expr) => {
                let rendered = self.session.expr_to_string_raw(&expr, None)?;
                Some(bound_rendered_info(rendered, max_bytes.min(DECLARATION_TYPE_MAX_BYTES)))
            }
            None => None,
        };
        let source = self
            .session
            .declaration_source_range(name, None)?
            .map(source_range_wire);
        Ok(Some(LeanWorkerDeclarationType {
            name: name.to_owned(),
            kind,
            type_signature,
            source,
        }))
    }

    fn list_declarations_strings(
        &mut self,
        filter: LeanWorkerDeclarationFilter,
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<u64> {
        let host_filter = LeanDeclarationFilter {
            include_private: filter.include_private,
            include_generated: filter.include_generated,
            include_internal: filter.include_internal,
        };
        if progress {
            emit_progress(writer, "list_declarations_strings", 0, None);
        }
        let names = self.session.list_declarations_strings(&host_filter, None, None)?;
        let total = u64::try_from(names.len()).unwrap_or(u64::MAX);
        let mut emitter = DataRowEmitter::default();
        for name in names {
            let payload = serde_json::value::to_raw_value(&name)
                .map_err(|err| host_internal(format!("list_declarations_strings row payload encode failed: {err}")))?;
            let row = emitter.next("rows", payload);
            writer
                .write(Message::DataRow(row))
                .map_err(|err| host_internal(format!("list_declarations_strings row frame write failed: {err}")))?;
        }
        if progress {
            emit_progress(writer, "list_declarations_strings", total, Some(total));
        }
        Ok(emitter.count())
    }

    fn describe_bulk(
        &mut self,
        names: &[String],
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<Vec<LeanWorkerDeclarationRow>> {
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let kinds = self.session.declaration_kind_bulk(&refs, None, None)?;
        let types = self.session.declaration_type_bulk(&refs, None, None)?;
        let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
        let mut rows = Vec::with_capacity(names.len());
        for (idx, name) in names.iter().enumerate() {
            let kind = kinds.get(idx).cloned().unwrap_or_else(|| "missing".to_owned());
            let row = if kind == "missing" {
                LeanWorkerDeclarationRow {
                    name: name.clone(),
                    kind,
                    type_signature: None,
                    source: None,
                }
            } else {
                let type_signature = match types.get(idx).and_then(Option::as_ref) {
                    Some(expr) => Some(self.session.expr_to_string_raw(expr, None)?),
                    None => None,
                };
                let source = self
                    .session
                    .declaration_source_range(name, None)?
                    .map(source_range_wire);
                LeanWorkerDeclarationRow {
                    name: name.clone(),
                    kind,
                    type_signature,
                    source,
                }
            };
            rows.push(row);
            if progress {
                emit_progress(
                    writer,
                    "describe_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                );
            }
        }
        Ok(rows)
    }

    fn process_module_query(
        &mut self,
        source: &str,
        query: LeanWorkerModuleQuery,
        options: &LeanWorkerElabOptions,
    ) -> LeanResult<LeanWorkerModuleQueryOutcome> {
        let options = elab_options_to_host(options);
        let query = module_query_host(query)?;
        Ok(
            match self.session.process_module_query(source, &query, &options, None)? {
                ModuleQueryOutcome::Ok { result, imports } => LeanWorkerModuleQueryOutcome::Ok {
                    result: module_query_result_wire(result),
                    imports,
                },
                ModuleQueryOutcome::MissingImports {
                    result,
                    imports,
                    missing,
                } => LeanWorkerModuleQueryOutcome::MissingImports {
                    result: module_query_result_wire(result),
                    imports,
                    missing,
                },
                ModuleQueryOutcome::HeaderParseFailed { diagnostics } => {
                    LeanWorkerModuleQueryOutcome::HeaderParseFailed {
                        diagnostics: elab_failure_wire(&diagnostics),
                    }
                }
                ModuleQueryOutcome::Unsupported => LeanWorkerModuleQueryOutcome::Unsupported,
            },
        )
    }

    fn process_module_query_batch(
        &mut self,
        source: &str,
        selectors: &[LeanWorkerModuleQuerySelector],
        budgets: &LeanWorkerOutputBudgets,
        options: &LeanWorkerElabOptions,
    ) -> LeanResult<LeanWorkerModuleQueryBatchOutcome> {
        let policy = self.module_query_cache_policy(source, options);
        let options = elab_options_to_host(options);
        let selectors = selectors
            .iter()
            .cloned()
            .map(module_query_selector_host)
            .collect::<LeanResult<Vec<_>>>()?;
        let budgets = module_query_budgets_host(budgets);
        self.clear_module_snapshot_cache_for_rss_guard()?;
        let cached = self
            .session
            .process_module_query_batch_cached(source, &selectors, &budgets, &options, &policy, None)?;
        if !matches!(cached, ModuleQueryBatchCachedOutcome::Unsupported) {
            return Ok(module_query_batch_cached_outcome_wire(cached));
        }
        Ok(
            match self
                .session
                .process_module_query_batch(source, &selectors, &budgets, &options, None)?
            {
                ModuleQueryBatchOutcome::Ok { result, imports } => {
                    let result = module_query_batch_envelope_wire(result);
                    let output_bytes = approx_json_bytes(&result);
                    LeanWorkerModuleQueryBatchOutcome::Ok {
                        result,
                        imports,
                        facts: LeanWorkerModuleQueryCacheFacts::uncached(output_bytes),
                    }
                }
                ModuleQueryBatchOutcome::MissingImports {
                    result,
                    imports,
                    missing,
                } => {
                    let result = module_query_batch_envelope_wire(result);
                    let output_bytes = approx_json_bytes(&result);
                    LeanWorkerModuleQueryBatchOutcome::MissingImports {
                        result,
                        imports,
                        missing,
                        facts: LeanWorkerModuleQueryCacheFacts::uncached(output_bytes),
                    }
                }
                ModuleQueryBatchOutcome::HeaderParseFailed { diagnostics } => {
                    let diagnostics = elab_failure_wire(&diagnostics);
                    let output_bytes = approx_json_bytes(&diagnostics);
                    LeanWorkerModuleQueryBatchOutcome::HeaderParseFailed {
                        diagnostics,
                        facts: LeanWorkerModuleQueryCacheFacts::uncached(output_bytes),
                    }
                }
                ModuleQueryBatchOutcome::Unsupported => LeanWorkerModuleQueryBatchOutcome::Unsupported,
            },
        )
    }

    fn clear_module_snapshot_cache(&mut self) -> LeanResult<LeanWorkerModuleSnapshotCacheClearResult> {
        let result = self.session.clear_module_snapshot_cache()?;
        Ok(module_snapshot_cache_clear_result_wire(&result))
    }

    fn clear_module_snapshot_cache_for_rss_guard(&mut self) -> LeanResult<()> {
        let guard_kib = module_cache_env_u64("LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB", MODULE_CACHE_DEFAULT_RSS_GUARD_KIB);
        if guard_kib == 0 {
            return Ok(());
        }
        match current_rss_kib() {
            Some(current) if current >= guard_kib => {
                let _cleared = self.session.clear_module_snapshot_cache()?;
            }
            None => {
                let _cleared = self.session.clear_module_snapshot_cache()?;
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn module_query_cache_policy(&self, source: &str, options: &LeanWorkerElabOptions) -> ModuleQueryCachePolicy {
        let file_identity = options.file_label.clone();
        let max_entries = module_cache_env_u64("LEAN_RS_MODULE_CACHE_MAX_ENTRIES", MODULE_CACHE_DEFAULT_MAX_ENTRIES);
        let ttl_millis = module_cache_env_u64("LEAN_RS_MODULE_CACHE_TTL_MILLIS", MODULE_CACHE_DEFAULT_TTL_MILLIS);
        let max_bytes = module_cache_env_u64("LEAN_RS_MODULE_CACHE_MAX_BYTES", MODULE_CACHE_DEFAULT_MAX_BYTES);
        ModuleQueryCachePolicy {
            file_identity: file_identity.clone(),
            key: module_query_cache_key(source, &self.imports, options, &file_identity),
            max_entries,
            ttl_millis,
            max_bytes,
        }
    }
}

#[derive(Clone, Debug)]
struct PendingDataRow {
    stream: String,
    payload: Box<RawValue>,
}

enum StreamCallbackEvent {
    Row(PendingDataRow),
    Diagnostic(Diagnostic),
    Progress(ProgressTick),
    Metadata(serde_json::Value),
}

enum StreamCallbackError {
    Malformed(String),
    Write(String),
}

struct StreamForwarder {
    writer: ProtocolWriter,
    emitter: DataRowEmitter,
    progress: bool,
    metadata: Option<serde_json::Value>,
}

impl StreamForwarder {
    fn new(writer: ProtocolWriter, progress: bool) -> Self {
        Self {
            writer,
            emitter: DataRowEmitter::default(),
            progress,
            metadata: None,
        }
    }

    fn emit_row(&mut self, row: PendingDataRow) -> Result<(), ProtocolError> {
        let row = self.emitter.next(row.stream, row.payload);
        self.writer.write(Message::DataRow(row))?;
        if self.progress {
            emit_progress(&self.writer, "data_stream", self.emitter.count(), None);
        }
        Ok(())
    }

    fn emit_diagnostic(&self, diagnostic: Diagnostic) -> Result<(), ProtocolError> {
        self.writer.write(Message::Diagnostic(diagnostic))
    }

    fn emit_progress(&self, progress: ProgressTick) -> Result<(), ProtocolError> {
        self.writer.write(Message::ProgressTick(progress))
    }

    fn set_metadata(&mut self, metadata: serde_json::Value) {
        self.metadata = Some(metadata);
    }

    fn summary(&self, elapsed: std::time::Duration) -> StreamSummary {
        StreamSummary::new(
            self.emitter.count(),
            self.emitter.per_stream_counts(),
            elapsed,
            self.metadata.clone(),
        )
    }
}

#[derive(Debug)]
enum StreamRunError {
    Host(LeanError),
    Binding(WorkerBindingError),
    ExportStatus(u8),
    CallbackStatus(LeanCallbackStatus),
    MalformedRow(String),
}

enum CapabilityJsonError {
    Host(LeanError),
    Binding(WorkerBindingError),
    Malformed(String),
}

impl StreamRunError {
    fn from_worker_call(value: WorkerCallError) -> Self {
        match value {
            WorkerCallError::Host(err) => Self::Host(err),
            WorkerCallError::Binding(err) => Self::Binding(err),
        }
    }
}

impl CapabilityJsonError {
    fn from_worker_call(value: WorkerCallError) -> Self {
        match value {
            WorkerCallError::Host(err) => Self::Host(err),
            WorkerCallError::Binding(err) => Self::Binding(err),
        }
    }
}

impl From<ProtocolError> for StreamRunError {
    fn from(value: ProtocolError) -> Self {
        Self::Host(host_internal(format!("worker data-row frame write failed: {value}")))
    }
}

fn parse_row_envelope(raw: &str) -> Result<StreamCallbackEvent, String> {
    let envelope: RowCallbackEnvelope =
        serde_json::from_str(raw).map_err(|err| format!("row callback payload is not valid JSON: {err}"))?;
    if let Some(diagnostic) = envelope.diagnostic {
        let code = diagnostic
            .code
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "diagnostic callback payload must contain a non-empty string field `code`".to_owned())?;
        let message = diagnostic
            .message
            .ok_or_else(|| "diagnostic callback payload must contain a string field `message`".to_owned())?;
        return Ok(StreamCallbackEvent::Diagnostic(Diagnostic::new(code, message)));
    }
    if let Some(progress) = envelope.progress {
        let phase = progress
            .phase
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "progress callback payload must contain a non-empty string field `phase`".to_owned())?;
        return Ok(StreamCallbackEvent::Progress(ProgressTick::new(
            phase,
            progress.current,
            progress.total,
        )));
    }
    if let Some(metadata) = envelope.metadata {
        let metadata = serde_json::from_str(metadata.get())
            .map_err(|err| format!("metadata callback payload is not valid JSON: {err}"))?;
        return Ok(StreamCallbackEvent::Metadata(metadata));
    }
    let stream = envelope
        .stream
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "row callback payload must contain a non-empty string field `stream`".to_owned())?;
    let payload = envelope
        .payload
        .ok_or_else(|| "row callback payload must contain field `payload`".to_owned())?;
    Ok(StreamCallbackEvent::Row(PendingDataRow { stream, payload }))
}

#[derive(Deserialize)]
struct RowCallbackEnvelope {
    stream: Option<String>,
    payload: Option<Box<RawValue>>,
    diagnostic: Option<RowCallbackDiagnostic>,
    progress: Option<RowCallbackProgress>,
    metadata: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct RowCallbackDiagnostic {
    code: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct RowCallbackProgress {
    phase: Option<String>,
    current: u64,
    total: Option<u64>,
}

fn elab_options_to_host(options: &LeanWorkerElabOptions) -> LeanElabOptions {
    LeanElabOptions::new()
        .namespace_context(&options.namespace_context)
        .file_label(&options.file_label)
        .heartbeat_limit(options.heartbeat_limit)
        .diagnostic_byte_limit(options.diagnostic_byte_limit)
}

fn elab_options_to_host_meta(options: &LeanWorkerElabOptions, transparency: LeanMetaTransparency) -> LeanMetaOptions {
    LeanMetaOptions::new()
        .namespace_context(&options.namespace_context)
        .heartbeat_limit(options.heartbeat_limit)
        .diagnostic_byte_limit(options.diagnostic_byte_limit)
        .transparency(transparency)
}

fn meta_transparency_to_host(value: LeanWorkerMetaTransparency) -> LeanMetaTransparency {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "LeanWorkerMetaTransparency is #[non_exhaustive] across the lean-rs-worker-protocol crate boundary; an unrecognised variant from a newer parent falls back to the host's Default transparency, matching the host crate's own default"
    )]
    match value {
        LeanWorkerMetaTransparency::Default => LeanMetaTransparency::Default,
        LeanWorkerMetaTransparency::Reducible => LeanMetaTransparency::Reducible,
        LeanWorkerMetaTransparency::Instances => LeanMetaTransparency::Instances,
        LeanWorkerMetaTransparency::All => LeanMetaTransparency::All,
        _ => LeanMetaTransparency::Default,
    }
}

fn elab_failure_wire(failure: &LeanElabFailure) -> LeanWorkerElabFailure {
    LeanWorkerElabFailure {
        diagnostics: diagnostics(failure),
        truncated: failure.truncated(),
    }
}

fn meta_failure_from_elab<T>(failure: &LeanElabFailure) -> LeanWorkerMetaResult<T> {
    LeanWorkerMetaResult::Failed {
        failure: elab_failure_wire(failure),
    }
}

fn meta_render_expr(
    session: &mut LeanSession<'static, 'static>,
    response: LeanMetaResponse<lean_rs::LeanExpr<'static>>,
    meta_options: &LeanMetaOptions,
) -> LeanResult<LeanWorkerMetaResult<LeanWorkerRendered>> {
    let expr = match response {
        LeanMetaResponse::Ok(expr) => expr,
        LeanMetaResponse::Failed(failure) => {
            return Ok(LeanWorkerMetaResult::Failed {
                failure: elab_failure_wire(&failure),
            });
        }
        LeanMetaResponse::TimeoutOrHeartbeat(failure) => {
            return Ok(LeanWorkerMetaResult::TimeoutOrHeartbeat {
                failure: elab_failure_wire(&failure),
            });
        }
        LeanMetaResponse::Unsupported(failure) => {
            return Ok(LeanWorkerMetaResult::Unsupported {
                failure: elab_failure_wire(&failure),
            });
        }
    };
    let pp_response = session.run_meta(&meta::pp_expr(), expr.clone(), meta_options, None)?;
    Ok(match pp_response {
        LeanMetaResponse::Ok(rendered) => LeanWorkerMetaResult::Ok {
            value: LeanWorkerRendered {
                value: rendered,
                rendering: LeanWorkerRendering::Pretty,
            },
        },
        LeanMetaResponse::Unsupported(_) => LeanWorkerMetaResult::Ok {
            value: LeanWorkerRendered {
                value: session.expr_to_string_raw(&expr, None)?,
                rendering: LeanWorkerRendering::Raw,
            },
        },
        LeanMetaResponse::Failed(failure) => LeanWorkerMetaResult::Failed {
            failure: elab_failure_wire(&failure),
        },
        LeanMetaResponse::TimeoutOrHeartbeat(failure) => LeanWorkerMetaResult::TimeoutOrHeartbeat {
            failure: elab_failure_wire(&failure),
        },
    })
}

fn source_range_wire(range: LeanSourceRange) -> LeanWorkerSourceRange {
    LeanWorkerSourceRange {
        file: range.file,
        start_line: range.start_line,
        start_column: range.start_column,
        end_line: range.end_line,
        end_column: range.end_column,
    }
}

fn module_query_host(query: LeanWorkerModuleQuery) -> LeanResult<ModuleQuery> {
    Ok(match query {
        LeanWorkerModuleQuery::Diagnostics => ModuleQuery::Diagnostics,
        LeanWorkerModuleQuery::TypeAt { line, column } => ModuleQuery::TypeAt { line, column },
        LeanWorkerModuleQuery::GoalAt { line, column } => ModuleQuery::GoalAt { line, column },
        LeanWorkerModuleQuery::References { name } => ModuleQuery::References { name },
        _ => return Err(host_internal("unsupported module query variant")),
    })
}

fn module_query_selector_host(selector: LeanWorkerModuleQuerySelector) -> LeanResult<ModuleQuerySelector> {
    Ok(match selector {
        LeanWorkerModuleQuerySelector::Diagnostics { id } => ModuleQuerySelector::Diagnostics { id },
        LeanWorkerModuleQuerySelector::ProofState { id, line, column } => {
            ModuleQuerySelector::ProofState { id, line, column }
        }
        LeanWorkerModuleQuerySelector::TypeAt { id, line, column } => ModuleQuerySelector::TypeAt { id, line, column },
        LeanWorkerModuleQuerySelector::References { id, name } => ModuleQuerySelector::References { id, name },
        LeanWorkerModuleQuerySelector::DeclarationTarget { id, name, line, column } => {
            ModuleQuerySelector::DeclarationTarget { id, name, line, column }
        }
        LeanWorkerModuleQuerySelector::SurroundingDeclaration { id, line, column } => {
            ModuleQuerySelector::SurroundingDeclaration { id, line, column }
        }
        _ => return Err(host_internal("unsupported module query selector variant")),
    })
}

fn module_query_budgets_host(budgets: &LeanWorkerOutputBudgets) -> ModuleQueryOutputBudgets {
    ModuleQueryOutputBudgets {
        per_field_bytes: budgets.per_field_bytes,
        total_bytes: budgets.total_bytes,
    }
}

fn module_cache_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
}

fn approx_json_bytes<T: serde::Serialize>(value: &T) -> u64 {
    serde_json::to_vec(value).map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
}

fn module_query_cache_key(
    source: &str,
    imports: &[String],
    options: &LeanWorkerElabOptions,
    file_identity: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(MODULE_QUERY_CACHE_API_VERSION.as_bytes());
    hasher.update(b"\0file\0");
    hasher.update(file_identity.as_bytes());
    hasher.update(b"\0source\0");
    hasher.update(source.as_bytes());
    hasher.update(b"\0imports\0");
    for import in imports {
        hasher.update(import.as_bytes());
        hasher.update(b"\0");
    }
    let toolchain = lean_toolchain::ToolchainFingerprint::current();
    hasher.update(b"\0toolchain\0");
    hasher.update(toolchain.lean_version.as_bytes());
    hasher.update(b"\0");
    hasher.update(toolchain.resolved_version.as_bytes());
    hasher.update(b"\0");
    hasher.update(toolchain.header_sha256.as_bytes());
    hasher.update(b"\0");
    hasher.update(toolchain.fixture_sha256.as_bytes());
    hasher.update(b"\0");
    hasher.update(toolchain.host_triple.as_bytes());
    hasher.update(b"\0options\0");
    hasher.update(options.namespace_context.as_bytes());
    hasher.update(b"\0");
    hasher.update(options.file_label.as_bytes());
    hasher.update(b"\0");
    hasher.update(options.heartbeat_limit.to_le_bytes());
    hasher.update(options.diagnostic_byte_limit.to_le_bytes());
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut key, byte| {
            let _ = write!(key, "{byte:02x}");
            key
        })
}

#[cfg(target_os = "linux")]
fn current_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

#[cfg(not(target_os = "linux"))]
fn current_rss_kib() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<u64>().ok().filter(|value| *value > 0)
}

fn module_source_span_wire(span: &ModuleSourceSpan) -> LeanWorkerModuleSourceSpan {
    LeanWorkerModuleSourceSpan {
        start_line: span.start_line,
        start_column: span.start_column,
        end_line: span.end_line,
        end_column: span.end_column,
    }
}

fn rendered_info_wire(info: RenderedInfo) -> LeanWorkerRenderedInfo {
    LeanWorkerRenderedInfo {
        value: info.value,
        truncated: info.truncated,
    }
}

fn bound_rendered_info(value: String, max_bytes: usize) -> LeanWorkerRenderedInfo {
    if value.len() <= max_bytes {
        return LeanWorkerRenderedInfo {
            value,
            truncated: false,
        };
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    LeanWorkerRenderedInfo {
        value: value[..end].to_owned(),
        truncated: true,
    }
}

fn type_at_result_wire(result: TypeAtResult) -> LeanWorkerTypeAtResult {
    match result {
        TypeAtResult::Term {
            span,
            expr,
            type_str,
            expected_type,
        } => LeanWorkerTypeAtResult::Term {
            span: module_source_span_wire(&span),
            expr: rendered_info_wire(expr),
            type_str: rendered_info_wire(type_str),
            expected_type: expected_type.map(rendered_info_wire),
        },
        TypeAtResult::NoTerm => LeanWorkerTypeAtResult::NoTerm,
    }
}

fn goal_at_result_wire(result: GoalAtResult) -> LeanWorkerGoalAtResult {
    match result {
        GoalAtResult::Goal {
            span,
            goals_before,
            goals_after,
            truncated,
        } => LeanWorkerGoalAtResult::Goal {
            span: module_source_span_wire(&span),
            goals_before,
            goals_after,
            truncated,
        },
        GoalAtResult::NoTacticContext => LeanWorkerGoalAtResult::NoTacticContext,
    }
}

fn references_result_wire(result: ReferencesResult) -> LeanWorkerReferencesResult {
    LeanWorkerReferencesResult {
        references: result.references.into_iter().map(name_ref_wire).collect(),
        truncated: result.truncated,
    }
}

fn local_info_wire(info: LocalInfo) -> LeanWorkerLocalInfo {
    LeanWorkerLocalInfo {
        name: info.name,
        binder_info: info.binder_info,
        type_str: rendered_info_wire(info.type_str),
        value: info.value.map(rendered_info_wire),
    }
}

fn declaration_target_info_wire(info: DeclarationTargetInfo) -> LeanWorkerDeclarationTargetInfo {
    LeanWorkerDeclarationTargetInfo {
        short_name: info.short_name,
        declaration_name: info.declaration_name,
        namespace_name: info.namespace_name,
        declaration_kind: info.declaration_kind,
        declaration_span: module_source_span_wire(&info.declaration_span),
        name_span: module_source_span_wire(&info.name_span),
        body_span: module_source_span_wire(&info.body_span),
    }
}

fn declaration_target_result_wire(result: DeclarationTargetResult) -> LeanWorkerDeclarationTargetResult {
    match result {
        DeclarationTargetResult::Target(info) => LeanWorkerDeclarationTargetResult::Target {
            info: declaration_target_info_wire(info),
        },
        DeclarationTargetResult::NotFound => LeanWorkerDeclarationTargetResult::NotFound,
        DeclarationTargetResult::Ambiguous(candidates) => LeanWorkerDeclarationTargetResult::Ambiguous {
            candidates: candidates.into_iter().map(declaration_target_info_wire).collect(),
        },
    }
}

fn proof_state_info_wire(info: ProofStateInfo) -> LeanWorkerProofStateInfo {
    LeanWorkerProofStateInfo {
        declaration_name: info.declaration_name,
        namespace_name: info.namespace_name,
        safe_edit: info.safe_edit.map(declaration_target_info_wire),
        span: module_source_span_wire(&info.span),
        goals_before: info.goals_before,
        goals_after: info.goals_after,
        locals: info.locals.into_iter().map(local_info_wire).collect(),
        expected_type: info.expected_type.map(rendered_info_wire),
        truncated: info.truncated,
    }
}

fn proof_state_result_wire(result: ProofStateResult) -> LeanWorkerProofStateResult {
    match result {
        ProofStateResult::State(info) => LeanWorkerProofStateResult::State {
            info: Box::new(proof_state_info_wire(*info)),
        },
        ProofStateResult::Unavailable { message } => LeanWorkerProofStateResult::Unavailable { message },
    }
}

fn surrounding_declaration_result_wire(result: SurroundingDeclarationResult) -> LeanWorkerSurroundingDeclarationResult {
    match result {
        SurroundingDeclarationResult::Declaration(info) => LeanWorkerSurroundingDeclarationResult::Declaration {
            info: declaration_target_info_wire(info),
        },
        SurroundingDeclarationResult::None => LeanWorkerSurroundingDeclarationResult::None,
    }
}

fn module_query_result_wire(result: ModuleQueryResult) -> LeanWorkerModuleQueryResult {
    match result {
        ModuleQueryResult::Diagnostics(failure) => {
            LeanWorkerModuleQueryResult::Diagnostics(elab_failure_wire(&failure))
        }
        ModuleQueryResult::TypeAt(result) => LeanWorkerModuleQueryResult::TypeAt(type_at_result_wire(result)),
        ModuleQueryResult::GoalAt(result) => LeanWorkerModuleQueryResult::GoalAt(goal_at_result_wire(result)),
        ModuleQueryResult::References(result) => {
            LeanWorkerModuleQueryResult::References(references_result_wire(result))
        }
    }
}

fn module_query_batch_result_wire(result: ModuleQueryBatchResult) -> LeanWorkerModuleQueryBatchResult {
    match result {
        ModuleQueryBatchResult::Diagnostics(failure) => {
            LeanWorkerModuleQueryBatchResult::Diagnostics(elab_failure_wire(&failure))
        }
        ModuleQueryBatchResult::ProofState(result) => {
            LeanWorkerModuleQueryBatchResult::ProofState(proof_state_result_wire(result))
        }
        ModuleQueryBatchResult::TypeAt(result) => LeanWorkerModuleQueryBatchResult::TypeAt(type_at_result_wire(result)),
        ModuleQueryBatchResult::References(result) => {
            LeanWorkerModuleQueryBatchResult::References(references_result_wire(result))
        }
        ModuleQueryBatchResult::DeclarationTarget(result) => {
            LeanWorkerModuleQueryBatchResult::DeclarationTarget(declaration_target_result_wire(result))
        }
        ModuleQueryBatchResult::SurroundingDeclaration(result) => {
            LeanWorkerModuleQueryBatchResult::SurroundingDeclaration(surrounding_declaration_result_wire(result))
        }
    }
}

fn module_query_batch_item_wire(item: ModuleQueryBatchItem) -> LeanWorkerModuleQueryBatchItem {
    match item {
        ModuleQueryBatchItem::Ok { id, result } => LeanWorkerModuleQueryBatchItem::Ok {
            id,
            result: Box::new(module_query_batch_result_wire(*result)),
        },
        ModuleQueryBatchItem::Unavailable { id, message } => {
            LeanWorkerModuleQueryBatchItem::Unavailable { id, message }
        }
        ModuleQueryBatchItem::BudgetExceeded { id, message } => {
            LeanWorkerModuleQueryBatchItem::BudgetExceeded { id, message }
        }
    }
}

fn module_query_batch_envelope_wire(
    result: lean_rs_host::host::process::ModuleQueryBatchEnvelope,
) -> LeanWorkerModuleQueryBatchEnvelope {
    LeanWorkerModuleQueryBatchEnvelope {
        items: result.items.into_iter().map(module_query_batch_item_wire).collect(),
        total_truncated: result.total_truncated,
    }
}

fn module_cache_status_wire(status: ModuleQueryCacheStatus) -> LeanWorkerModuleCacheStatus {
    match status {
        ModuleQueryCacheStatus::Hit => LeanWorkerModuleCacheStatus::Hit,
        ModuleQueryCacheStatus::Miss => LeanWorkerModuleCacheStatus::Miss,
        ModuleQueryCacheStatus::Rebuilt => LeanWorkerModuleCacheStatus::Rebuilt,
        ModuleQueryCacheStatus::Evicted => LeanWorkerModuleCacheStatus::Evicted,
    }
}

fn module_query_timings_wire(timings: &ModuleQueryTimings) -> LeanWorkerModuleQueryTimings {
    LeanWorkerModuleQueryTimings {
        header_import_micros: timings.header_import_micros,
        elaboration_micros: timings.elaboration_micros,
        projection_micros: timings.projection_micros,
        rendering_micros: timings.rendering_micros,
    }
}

fn module_query_cache_facts_wire(facts: &ModuleQueryCacheFacts) -> LeanWorkerModuleQueryCacheFacts {
    LeanWorkerModuleQueryCacheFacts {
        cache_status: module_cache_status_wire(facts.cache_status),
        timings: module_query_timings_wire(&facts.timings),
        output_bytes: facts.output_bytes,
        cache_entry_count: facts.cache_entry_count,
        cache_approx_bytes: facts.cache_approx_bytes,
    }
}

fn module_query_batch_cached_outcome_wire(outcome: ModuleQueryBatchCachedOutcome) -> LeanWorkerModuleQueryBatchOutcome {
    match outcome {
        ModuleQueryBatchCachedOutcome::Ok { result, imports, facts } => LeanWorkerModuleQueryBatchOutcome::Ok {
            result: module_query_batch_envelope_wire(result),
            imports,
            facts: module_query_cache_facts_wire(&facts),
        },
        ModuleQueryBatchCachedOutcome::MissingImports {
            result,
            imports,
            missing,
            facts,
        } => LeanWorkerModuleQueryBatchOutcome::MissingImports {
            result: module_query_batch_envelope_wire(result),
            imports,
            missing,
            facts: module_query_cache_facts_wire(&facts),
        },
        ModuleQueryBatchCachedOutcome::HeaderParseFailed { diagnostics, facts } => {
            LeanWorkerModuleQueryBatchOutcome::HeaderParseFailed {
                diagnostics: elab_failure_wire(&diagnostics),
                facts: module_query_cache_facts_wire(&facts),
            }
        }
        ModuleQueryBatchCachedOutcome::Unsupported => LeanWorkerModuleQueryBatchOutcome::Unsupported,
    }
}

fn module_snapshot_cache_clear_result_wire(
    result: &ModuleSnapshotCacheClearResult,
) -> LeanWorkerModuleSnapshotCacheClearResult {
    LeanWorkerModuleSnapshotCacheClearResult {
        entries_cleared: result.entries_cleared,
        approx_bytes_cleared: result.approx_bytes_cleared,
    }
}

fn name_ref_wire(node: NameRefNode) -> LeanWorkerNameRef {
    LeanWorkerNameRef {
        start_line: node.start_line,
        start_column: node.start_column,
        end_line: node.end_line,
        end_column: node.end_column,
        name: node.name,
        is_binder: node.is_binder,
    }
}

fn elab_failure_outcome(failure: &LeanElabFailure) -> LeanWorkerElabResult {
    LeanWorkerElabResult {
        success: false,
        diagnostics: diagnostics(failure),
        truncated: failure.truncated(),
    }
}

fn kernel_failure_outcome(status: LeanWorkerKernelStatus, failure: &LeanElabFailure) -> LeanWorkerKernelResult {
    LeanWorkerKernelResult {
        status,
        diagnostics: diagnostics(failure),
        truncated: failure.truncated(),
        summary: None,
    }
}

fn diagnostics(failure: &LeanElabFailure) -> Vec<LeanWorkerDiagnostic> {
    failure
        .diagnostics()
        .iter()
        .map(|diagnostic| {
            let (line, column, end_line, end_column) =
                diagnostic.position().map_or((None, None, None, None), |position| {
                    (
                        Some(position.line()),
                        Some(position.column()),
                        position.end_line(),
                        position.end_column(),
                    )
                });
            LeanWorkerDiagnostic {
                severity: match diagnostic.severity() {
                    LeanSeverity::Info => "info",
                    LeanSeverity::Warning => "warning",
                    LeanSeverity::Error => "error",
                }
                .to_owned(),
                message: diagnostic.message().to_owned(),
                file_label: diagnostic.file_label().to_owned(),
                line,
                column,
                end_line,
                end_column,
            }
        })
        .collect()
}

fn emit_progress(writer: &ProtocolWriter, phase: &str, current: u64, total: Option<u64>) {
    drop(writer.write(Message::ProgressTick(ProgressTick::new(
        phase.to_owned(),
        current,
        total,
    ))));
}

fn emit_test_rows(writer: &ProtocolWriter, streams: &[String]) -> Result<u64, ProtocolError> {
    let mut emitter = DataRowEmitter::default();
    for (idx, stream) in streams.iter().enumerate() {
        let payload = serde_json::value::to_raw_value(&serde_json::json!({
            "stream": stream,
            "index": idx,
        }))?;
        let row = emitter.next(stream.clone(), payload);
        writer.write(Message::DataRow(row))?;
    }
    Ok(emitter.count())
}

#[allow(dead_code, reason = "reserved for future worker configuration paths")]
fn _path_for_diagnostics(path: &Path) -> PathBuf {
    path.to_path_buf()
}
