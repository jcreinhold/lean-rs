use crate::authority::{WorkerAuthority, WorkerSourceContext, WorkerStringCall, worker_authority};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::artifact_reads::ArtifactReadScope;
use crate::host_artifacts::SessionAdmission;

use lean_rs::error::host_internal;
use lean_rs::module::{
    TrustedHostImports, TrustedWorkerCapability, WorkerBinaryBinding, WorkerBindingFailure, WorkerStreamBinding,
    WorkerStringBinding, WorkerUnitBinding,
};
use lean_rs::{
    LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanError, LeanResult, LeanRuntime, LeanStringEvent,
};
use lean_rs_host::host::LeanPreparedHostArtifacts;
use lean_rs_host::host::process::{
    DeclarationOutlineResult, DeclarationTargetInfo, DeclarationTargetResult, DeclarationVerificationBatchItem,
    DeclarationVerificationBatchOutcome, DeclarationVerificationBatchRequest, DeclarationVerificationBatchRow,
    DeclarationVerificationFacts, DeclarationVerificationOutcome, DeclarationVerificationRequest,
    DeclarationVerificationStatus, DeclarationVerificationTarget, GoalAtResult, LocalInfo, ModuleQuery,
    ModuleQueryBatchCachedOutcome, ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult,
    ModuleQueryCacheFacts, ModuleQueryCachePolicy, ModuleQueryCacheStatus, ModuleQueryOutcome,
    ModuleQueryOutputBudgets, ModuleQueryResult, ModuleQuerySelector, ModuleQueryTimings,
    ModuleSnapshotCacheClearResult, ModuleSourceSpan, NameRefNode, ProofAttemptEnvelope, ProofAttemptOutcome,
    ProofAttemptRequest, ProofAttemptRow, ProofAttemptStatus, ProofBoundaryCandidate, ProofCandidate, ProofEditTarget,
    ProofStateInfo, ProofStateResult, ReferencesResult, RenderedInfo, SorryPolicy, SurroundingDeclarationResult,
    TypeAtResult,
};
use lean_rs_host::meta::{self, LeanMetaOptions, LeanMetaResponse, LeanMetaTransparency};
use lean_rs_host::{
    DeclarationFlags, DeclarationInspection, DeclarationInspectionBudgets, DeclarationInspectionFields,
    DeclarationInspectionRequest, DeclarationInspectionResult, DeclarationNameMatch, DeclarationProofSearchFacts,
    DeclarationRenderedInfo, DeclarationSearchBias, DeclarationSearchRequest, DeclarationSearchResult,
    DeclarationSearchRow, DeclarationSearchScope, LeanCapabilities, LeanDeclarationFilter, LeanDerivedWorkFacts,
    LeanElabFailure, LeanElabOptions, LeanHost, LeanImportStats, LeanKernelOutcome, LeanSession,
    LeanSessionImportProfile, LeanSeverity, LeanSourceRange,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use lean_rs_worker_protocol::protocol::{
    DataRowEmitter, Diagnostic, HostSessionMode, MAX_FRAME_BYTES, Message, ProgressTick, ProtocolError, Request,
    Response, StreamSummary, read_frame, write_frame,
};
use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationFlags,
    LeanWorkerDeclarationInspection, LeanWorkerDeclarationInspectionFields, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationNameMatch, LeanWorkerDeclarationOutlineResult,
    LeanWorkerDeclarationProofSearchFacts, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchBias, LeanWorkerDeclarationSearchFacts, LeanWorkerDeclarationSearchPruning,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationSearchRow, LeanWorkerDeclarationSearchScope,
    LeanWorkerDeclarationSearchTimings, LeanWorkerDeclarationTargetInfo, LeanWorkerDeclarationTargetResult,
    LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationBatchItem,
    LeanWorkerDeclarationVerificationBatchRequest, LeanWorkerDeclarationVerificationBatchResult,
    LeanWorkerDeclarationVerificationBatchRow, LeanWorkerDeclarationVerificationFacts,
    LeanWorkerDeclarationVerificationRequest, LeanWorkerDeclarationVerificationResult,
    LeanWorkerDeclarationVerificationStatus, LeanWorkerDeclarationVerificationTarget, LeanWorkerDerivedWorkFacts,
    LeanWorkerDiagnostic, LeanWorkerDoctorReport, LeanWorkerElabFailure, LeanWorkerElabOptions, LeanWorkerElabResult,
    LeanWorkerGoalAtResult, LeanWorkerHostRuntimeMetadata, LeanWorkerImportStats, LeanWorkerKernelResult,
    LeanWorkerKernelStatus, LeanWorkerKernelSummary, LeanWorkerLocalInfo, LeanWorkerMetaResult,
    LeanWorkerMetaTransparency, LeanWorkerModuleCacheStatus, LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchEnvelope,
    LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult,
    LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQueryOutcome, LeanWorkerModuleQueryResult,
    LeanWorkerModuleQuerySelector, LeanWorkerModuleQueryTimings, LeanWorkerModuleSnapshotCacheClearResult,
    LeanWorkerModuleSourceSpan, LeanWorkerNameRef, LeanWorkerOutputBudgets, LeanWorkerProofAttemptEnvelope,
    LeanWorkerProofAttemptRequest, LeanWorkerProofAttemptResult, LeanWorkerProofAttemptRow,
    LeanWorkerProofAttemptStatus, LeanWorkerProofBoundaryCandidate, LeanWorkerProofEditTarget,
    LeanWorkerProofPositionSelector, LeanWorkerProofPositionSummary, LeanWorkerProofStateInfo,
    LeanWorkerProofStateResult, LeanWorkerReferencesResult, LeanWorkerRendered, LeanWorkerRenderedInfo,
    LeanWorkerRendering, LeanWorkerSessionImportProfile, LeanWorkerSorryPolicy, LeanWorkerSourceCoordinateSpace,
    LeanWorkerSourceRange, LeanWorkerSurroundingDeclarationResult, LeanWorkerTypeAtResult,
};
use lean_rs_worker_protocol::worker_exports::WorkerExportOperation;

const DECLARATION_TYPE_MAX_BYTES: usize = 64 * 1024;
/// Bumped whenever the key's *inputs* change, so a stale digest can never be
/// read as a fresh one. Free to bump: the cache is a process-local `IO.Ref` in
/// the child and never outlives it. v3 added `HostSessionMode`.
const MODULE_QUERY_CACHE_API_VERSION: &str = "lean-rs.module-query-cache.v3";
/// How many imported environments one child may hold at once, counting the one
/// it is currently serving.
///
/// The embedder normally does not set this: `LeanWorker::spawn` injects
/// `LEAN_RS_WORKER_SESSION_POOL_CAPACITY` from `LeanWorkerConfig`, which sizes
/// it by what a held environment costs — about 70 MiB measured against process
/// `phys_footprint`, against 2–4 GiB for the import that would otherwise have
/// to run again.
///
/// It used to be derived from the parent's `max_imports` restart bound instead,
/// which made capacity eviction unreachable: the parent cycled the child at
/// exactly the point the pool would have begun evicting, so this file's LRU
/// path never ran. The two are now independent, and eviction is a live path.
/// [`evict_stale_host_sessions`] can additionally drop entries below capacity,
/// which only frees slots. This default covers an embedder that configures
/// neither.
const SESSION_POOL_DEFAULT_CAPACITY: usize = 4;

/// A capacity below 1 is meaningless (the child must be able to hold the session
/// it is serving), so it clamps rather than erroring: this is a tuning knob read
/// from the environment, not a request to validate.
fn session_pool_capacity() -> usize {
    std::env::var("LEAN_RS_WORKER_SESSION_POOL_CAPACITY")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(SESSION_POOL_DEFAULT_CAPACITY)
        .max(1)
}

/// Entries are effectively per *file*, and this consumer derives a call's
/// imports from that file's own header, so one file belongs to one import
/// profile. The pool holding several environments therefore does not multiply
/// the number of distinct snapshots in flight, and this does not scale with
/// [`SESSION_POOL_DEFAULT_CAPACITY`].
const MODULE_CACHE_DEFAULT_MAX_ENTRIES: u64 = 4;
const MODULE_CACHE_DEFAULT_TTL_MILLIS: u64 = 5 * 60 * 1000;
const MODULE_CACHE_DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const MODULE_CACHE_DEFAULT_RSS_GUARD_KIB: u64 = 3 * 1024 * 1024;

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
        write_worker_frame(&mut *stdout, message, cap)
    }
}

fn copy_import_roots_for_frame(roots: &[String], cap: u32) -> Option<Vec<String>> {
    // Even before JSON escaping, each entry needs its bytes and two quotes.
    // Discard obviously oversized orders before duplicating the anchor's storage.
    let mut remaining = cap as usize;
    for root in roots {
        remaining = remaining.checked_sub(2)?.checked_sub(root.len())?;
    }
    Some(roots.to_vec())
}

/// Optional observations must fit as a whole. A size refusal from the codec
/// happens before it writes the length prefix, so only one frame reaches the parent.
fn write_worker_frame(writer: &mut impl std::io::Write, message: Message, cap: u32) -> Result<(), ProtocolError> {
    let Message::Response(Response::HostRuntimeMetadata { mut metadata }) = message else {
        return write_frame(writer, message, cap);
    };
    if let Some(roots) = metadata.effective_import_roots.take() {
        let mut full = metadata.clone();
        full.effective_import_roots = Some(roots);
        match write_frame(
            writer,
            Message::Response(Response::HostRuntimeMetadata { metadata: full }),
            cap,
        ) {
            Err(ProtocolError::FrameTooLarge { .. }) => {}
            result => return result,
        }
    }
    write_frame(
        writer,
        Message::Response(Response::HostRuntimeMetadata { metadata }),
        cap,
    )
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

fn write_source_response(
    writer: &ProtocolWriter,
    state: Option<&HostSessionState>,
    response: Response,
) -> Result<(), ProtocolError> {
    if let Some(session) = state {
        match session.session.header_import_state() {
            Ok(accounting) => {
                writer.write(Message::HeaderImportState {
                    bytes: accounting.non_memory_mapped_bytes,
                    imports: accounting.imports,
                    accounting_unknown: accounting.accounting_unknown,
                })?;
                if accounting.recycle_requested {
                    return write_response(writer, Response::HeaderImportRecycle);
                }
            }
            Err(_) => {
                // Preserve the original result but retire before any later work.
                writer.write(Message::HeaderImportState {
                    bytes: 0,
                    imports: 0,
                    accounting_unknown: true,
                })?;
            }
        }
    }
    write_response(writer, response)
}

fn worker_import_stats(stats: &LeanImportStats) -> LeanWorkerImportStats {
    LeanWorkerImportStats {
        direct_import_names: stats.direct_import_names.clone(),
        effective_module_count: stats.effective_module_count,
        compacted_region_count: stats.compacted_region_count,
        memory_mapped_region_count: stats.memory_mapped_region_count,
        compacted_region_bytes: stats.compacted_region_bytes,
        memory_mapped_region_bytes: stats.memory_mapped_region_bytes,
        non_memory_mapped_region_bytes: stats.non_memory_mapped_region_bytes,
        imported_bytes: stats.imported_bytes,
        imported_constant_count: stats.imported_constant_count,
        extension_count: stats.extension_count,
        total_imported_extension_entries: stats.total_imported_extension_entries,
        import_level: stats.import_level.clone(),
        import_all: stats.import_all,
        load_exts: stats.load_exts,
    }
}

fn host_import_profile(profile: LeanWorkerSessionImportProfile) -> LeanResult<LeanSessionImportProfile> {
    Ok(match profile {
        LeanWorkerSessionImportProfile::ExportedPublic => LeanSessionImportProfile::ExportedPublic,
        LeanWorkerSessionImportProfile::Server => LeanSessionImportProfile::Server,
        LeanWorkerSessionImportProfile::Private => LeanSessionImportProfile::Private,
        LeanWorkerSessionImportProfile::FullPrivateCompat => LeanSessionImportProfile::FullPrivateCompat,
        _ => {
            return Err(host_internal(
                "worker child received an unsupported host session import profile".to_owned(),
            ));
        }
    })
}

pub(crate) fn run_stdio() -> ExitCode {
    install_immediate_abort_exit();
    if install_parent_death_signal() {
        return ExitCode::SUCCESS;
    }
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
/// alone—still above the supervisor's 30 s budget). The decisive fix is
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

/// Ask Linux to terminate this child if its current parent process dies.
///
/// This is best-effort hardening around the portable protocol contract below:
/// the child still exits deterministically when stdin reaches EOF or stdout
/// writes fail. `PR_SET_PDEATHSIG` covers the narrower Linux case where a
/// parent dies while inherited control file descriptors remain open.
#[cfg(target_os = "linux")]
#[allow(
    unsafe_code,
    reason = "installing a Linux parent-death signal requires libc prctl/getppid"
)]
fn install_parent_death_signal() -> bool {
    // SAFETY: `prctl(PR_SET_PDEATHSIG, SIGTERM)` modifies only this process'
    // parent-death setting. If the call fails, the portable pipe EOF/write
    // failure paths below still provide the baseline contract.
    let installed = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == 0 };

    if installed {
        // SAFETY: `getppid` is a pure process query. Checking for PID 1 closes
        // the race where the original parent dies between `fork/exec` and the
        // `prctl` call above.
        let parent_pid = unsafe { libc::getppid() };
        if parent_pid == 1 {
            return true;
        }
    }

    false
}

#[cfg(not(target_os = "linux"))]
fn install_parent_death_signal() -> bool {
    false
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the child owns stdin/stdout for the full protocol loop"
)]
fn serve_stdio() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    let parent_socket = std::env::var_os("LEAN_RS_WORKER_PARENT_SOCKET").map(PathBuf::from);
    #[cfg(unix)]
    let mut guardian = crate::parent_watchdog::install_parent_watchdog()?;
    let shared_reads = std::env::var_os("LEAN_RS_WORKER_SHARED_ARTIFACT_READS").is_some_and(|value| value == "1");
    #[cfg(unix)]
    let artifact_ipc_root = match std::env::var_os("LEAN_RS_WORKER_ARTIFACT_IPC_ROOT") {
        Some(root) => {
            let root = PathBuf::from(root);
            let lifeline_root = parent_socket
                .as_deref()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .ok_or_else(|| std::io::Error::other("artifact IPC root requires a parent lifeline"))?;
            if guardian.is_none() || root != lifeline_root {
                return Err(
                    std::io::Error::other("artifact IPC root differs from the guarded parent lifeline root").into(),
                );
            }
            Some(root)
        }
        None => None,
    };
    let descriptor = crate::host_artifacts::from_launch(
        std::env::var_os(crate::host_artifacts::PREPARED_ENV).as_deref(),
        shared_reads,
    )?;
    let (guarded_prepared, mut ordinary_prepared) = if shared_reads {
        (descriptor, None)
    } else {
        (None, descriptor)
    };
    let runtime = LeanRuntime::init()?;
    configure_lean_runtime_memory_limit_from_env();
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let writer = ProtocolWriter::new();
    let mut host_session: Option<HostSessionState> = None;
    let mut host_read: Option<ArtifactReadScope> = None;
    let mut prepared_read_root: Option<String> = None;
    let mut read_admission_failed = false;
    let mut artifact_epochs: Option<Vec<lean_rs_worker_protocol::types::LeanWorkerArtifactReadEpoch>> = None;
    // Imported environments this child holds but is not currently serving, MRU
    // at the back. Bounded by `session_pool_capacity` together with
    // `host_session`; a linear scan for the same reason `anchors` is one.
    let mut parked: Vec<HostSessionState> = Vec::new();
    let session_pool_capacity = session_pool_capacity();
    // One leaked anchor per distinct `(project_root, mode)` this child is
    // asked for. A linear scan, not a map: `HostSessionMode` is not `Hash`,
    // and a child serves one key in practice, so the list is length 1.
    let mut anchors: Vec<&'static HostAnchor> = Vec::new();
    let plugin = GenerationPlugin::default();
    let mut session_import_bytes = 0_u64;
    let mut session_imports = 0_u64;
    let import_count_limit = std::env::var("LEAN_RS_WORKER_MAX_IMPORTS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let header_budget = std::env::var("LEAN_RS_WORKER_IMPORT_RESIDUE_BYTES")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

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

    let mut last_managed_inspection: Option<lean_rs_worker_protocol::protocol::ManagedInspectionCommand> = None;
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

        if shared_reads
            && matches!(
                &request,
                Request::LoadFixtureCapability { .. }
                    | Request::CallFixtureMul { .. }
                    | Request::TriggerLeanPanic { .. }
            )
        {
            write_response(
                &writer,
                Response::Error {
                    code: "lean_rs.worker.artifact_read_required".to_owned(),
                    message: "direct fixture loaders are unavailable in a guarded host worker".to_owned(),
                },
            )?;
            continue;
        }
        if shared_reads
            && host_read.is_none()
            && !matches!(
                &request,
                Request::Health
                    | Request::Terminate
                    | Request::OpenHostSession { .. }
                    | Request::PrepareHostRead { .. }
                    | Request::ReleaseHostRead
            )
        {
            write_response(
                &writer,
                Response::Error {
                    code: "lean_rs.worker.artifact_read_required".to_owned(),
                    message: "host body requires actual shared artifact read custody".to_owned(),
                },
            )?;
            continue;
        }

        if prepared_read_root.is_some()
            && !matches!(
                &request,
                Request::OpenHostSession { .. } | Request::ReleaseHostRead | Request::Terminate
            )
        {
            write_response(
                &writer,
                Response::Error {
                    code: "lean_rs.worker.artifact_read_active".to_owned(),
                    message: "prepared artifact custody requires its session open or retirement".to_owned(),
                },
            )?;
            continue;
        }

        if !matches!(&request, Request::PrepareHostRead { .. })
            && let Some(state) = host_session.as_mut()
            && let Err(err) = state
                .session
                .configure_header_import_budget(session_import_bytes, header_budget)
        {
            write_response(&writer, error_response(&err))?;
            continue;
        }
        let (request, prepare_only) = if let Request::PrepareHostRead { project_root } = request {
            (
                Request::OpenHostSession {
                    project_root,
                    mode: HostSessionMode::ShimsOnly,
                    imports: Vec::new(),
                    import_profile: lean_rs_worker_protocol::types::LeanWorkerSessionImportProfile::default(),
                },
                true,
            )
        } else {
            (request, false)
        };
        match request {
            Request::Health => {
                write_response(&writer, Response::HealthOk)?;
            }
            Request::ReleaseHostRead => {
                // Closing aliases leaves inherited body descriptors locked.
                host_read = None;
                prepared_read_root = None;
                write_response(&writer, Response::HostReadReleased)?;
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
            Request::PrepareHostRead { .. } => {
                return Err(std::io::Error::other("untranslated read preparation request").into());
            }
            Request::OpenHostSession {
                project_root,
                mode,
                imports,
                import_profile,
            } => {
                if prepare_only && !shared_reads {
                    write_response(
                        &writer,
                        Response::Error {
                            code: "lean_rs.worker.artifact_read_required".to_owned(),
                            message: "managed read preparation requires a guarded prepared worker".to_owned(),
                        },
                    )?;
                    continue;
                }
                if read_admission_failed {
                    // A failed guardian exchange may already have retained
                    // custody. Retire this generation before another admission.
                    write_response(&writer, Response::HostArtifactEpochChanged)?;
                    continue;
                }
                let uses_prepared_read = !prepare_only
                    && mode == HostSessionMode::ShimsOnly
                    && prepared_read_root.as_deref() == Some(project_root.as_str())
                    && host_read.is_some();
                if host_read.is_some() && !uses_prepared_read {
                    write_response(
                        &writer,
                        Response::Error {
                            code: "lean_rs.worker.artifact_read_active".to_owned(),
                            message: "finish the previous host read scope before reopening".to_owned(),
                        },
                    )?;
                    continue;
                }
                // Capture storage facts only. No dylib/initializer may run
                // before the actual FD union is ACKed by the independent guardian.
                let admission = match guarded_prepared
                    .as_ref()
                    .map(|prepared| SessionAdmission::capture(&project_root, &mode, prepared.clone()))
                    .transpose()
                {
                    Ok(admission) => admission,
                    Err(error) => {
                        write_response(
                            &writer,
                            Response::Error {
                                code: "lean_rs.worker.artifact_read_unavailable".to_owned(),
                                message: error.to_string(),
                            },
                        )?;
                        continue;
                    }
                };
                if let (Some(previous), Some(admission)) = (&artifact_epochs, &admission) {
                    let previous_roots: std::collections::BTreeSet<_> = previous
                        .iter()
                        .map(|entry: &lean_rs_worker_protocol::types::LeanWorkerArtifactReadEpoch| {
                            PathBuf::from(&entry.project_root)
                        })
                        .collect();
                    if previous_roots != admission.roots().iter().cloned().collect() {
                        // Guardians retain this generation's whole union. Never
                        // expand it while another writer could hold a later lock.
                        write_response(&writer, Response::HostArtifactEpochChanged)?;
                        continue;
                    }
                }
                if shared_reads && !uses_prepared_read {
                    let allow_wait = artifact_epochs.is_none();
                    let read = std::env::var_os("LEAN_HOST_MCP_WRITER_EXECUTABLE")
                        .ok_or_else(|| std::io::Error::other("shared artifact helper is not configured"))
                        .and_then(|helper| {
                            ArtifactReadScope::acquire(
                                Path::new(&helper),
                                admission.as_ref().map_or(&[], SessionAdmission::roots),
                                Duration::from_secs(30),
                                allow_wait,
                                guardian.as_mut(),
                                artifact_ipc_root.as_deref(),
                            )
                        });
                    let read = match read {
                        Ok(read) => read,
                        Err(error) if !allow_wait && lean_toolchain::shared_reader::is_admission_contention(&error) => {
                            // Only the one-pass pre-transfer collector creates
                            // this exact typed error. No body or new guardian
                            // retain ran. The parent confirms the old generation
                            // stopped, then permits one cold session attempt.
                            write_response(&writer, Response::HostArtifactEpochChanged)?;
                            continue;
                        }
                        Err(error) => {
                            read_admission_failed = true;
                            write_response(
                                &writer,
                                Response::Error {
                                    code: "lean_rs.worker.artifact_read_unavailable".to_owned(),
                                    message: error.to_string(),
                                },
                            )?;
                            continue;
                        }
                    };
                    if artifact_epochs
                        .as_ref()
                        .is_some_and(|previous| previous != read.epochs())
                    {
                        drop(read);
                        write_response(&writer, Response::HostArtifactEpochChanged)?;
                        continue;
                    }
                    artifact_epochs = Some(read.epochs().to_vec());
                    host_read = Some(read);
                }
                if let Some(admission) = &admission
                    && let Err(error) = admission.validate_after_admission()
                {
                    read_admission_failed = true;
                    host_read = None;
                    write_response(
                        &writer,
                        Response::Error {
                            code: "lean_rs.worker.artifact_read_unavailable".to_owned(),
                            message: error.to_string(),
                        },
                    )?;
                    continue;
                }
                if prepare_only {
                    prepared_read_root = Some(project_root);
                    write_response(&writer, Response::HostReadPrepared)?;
                    continue;
                }
                prepared_read_root = None;
                let anchor = match ready_host_anchor(
                    runtime,
                    &mut anchors,
                    &project_root,
                    &mode,
                    admission.as_ref(),
                    &mut ordinary_prepared,
                ) {
                    Ok(anchor) => anchor,
                    Err(error) => {
                        read_admission_failed = shared_reads;
                        host_read = None;
                        write_response(&writer, error_response(&error))?;
                        continue;
                    }
                };
                let header_bytes = host_session
                    .as_ref()
                    .and_then(|state| state.session.header_import_state().ok())
                    .map_or(0, |state| state.non_memory_mapped_bytes);
                let import_budget_exhausted = (header_budget != 0
                    && session_import_bytes.saturating_add(header_bytes) >= header_budget)
                    || (import_count_limit != 0 && session_imports >= import_count_limit);
                let response = open_or_reuse_host_session(
                    anchor,
                    &plugin,
                    &mut host_session,
                    &mut parked,
                    session_pool_capacity,
                    import_budget_exhausted,
                    HostSessionKey {
                        project_root,
                        mode,
                        imports,
                        import_profile,
                    },
                );
                if !matches!(
                    &response,
                    Response::HostSessionOpened { .. } | Response::HostSessionReused { .. }
                ) {
                    // A known import refusal ran no later body. Unknown exits
                    // never reach this point and retain their actual FD custody.
                    host_read = None;
                }
                if let Response::HostSessionOpened { import_stats } = &response {
                    session_imports = session_imports.saturating_add(1);
                    session_import_bytes =
                        session_import_bytes.saturating_add(import_stats.non_memory_mapped_region_bytes);
                }
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
            Request::HostRuntimeMetadata => {
                let response = match host_session.as_ref() {
                    Some(state) => match state.session.checked_host_runtime_metadata() {
                        Ok(actual) => {
                            let checked_host_bindings = actual
                                .checked_binding_signatures
                                .into_iter()
                                .map(|(name, signature)| {
                                    (
                                        name,
                                        lean_toolchain::host_shim_abi::checked_binding_fingerprint(&signature),
                                    )
                                })
                                .collect();
                            Response::HostRuntimeMetadata {
                                metadata: LeanWorkerHostRuntimeMetadata {
                                    compiled_lean_version: lean_toolchain::LEAN_VERSION.to_owned(),
                                    compiled_header_sha256: lean_toolchain::LEAN_HEADER_DIGEST.to_owned(),
                                    runtime_lean_version: actual.runtime_lean_version,
                                    checked_host_bindings,
                                    artifact_read_epochs: host_read.as_ref().map(|read| read.epochs().to_vec()),
                                    effective_import_roots: host_read
                                        .as_ref()
                                        .and(state.observed_roots)
                                        .and_then(|roots| copy_import_roots_for_frame(roots, writer.max_frame_bytes())),
                                },
                            }
                        }
                        Err(error) => error_response(&error),
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
            Request::InspectDeclarationFenced { command, request } => {
                let advances = last_managed_inspection.as_ref().map_or_else(
                    || command.command == 1,
                    |prior| {
                        prior.generation == command.generation
                            && command.command > prior.command
                            && (prior.attempt != command.attempt || prior.stage != command.stage)
                    },
                );
                if !shared_reads || command.generation.is_empty() || !advances {
                    return Err(std::io::Error::other("managed inspection command binding differs").into());
                }
                last_managed_inspection = Some(command.clone());
                let response = match host_session.as_mut() {
                    Some(state) => match state.inspect_declaration(&request) {
                        Ok(result) => Response::DeclarationInspectionFinished {
                            command,
                            result: Box::new(result),
                        },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::InspectDeclaration { request } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.inspect_declaration(&request) {
                        Ok(result) => Response::DeclarationInspection { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_response(&writer, response)?;
            }
            Request::AttemptProof {
                request,
                options,
                progress,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.attempt_proof(&request, &options, progress, &writer) {
                        Ok(result) => Response::ProofAttempt { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_source_response(&writer, host_session.as_ref(), response)?;
            }
            Request::VerifyDeclaration {
                request,
                options,
                progress,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.verify_declaration(&request, &options, progress, &writer) {
                        Ok(result) => Response::DeclarationVerification { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_source_response(&writer, host_session.as_ref(), response)?;
            }
            Request::VerifyDeclarationBatch {
                request,
                options,
                progress,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.verify_declaration_batch(&request, &options, progress, &writer) {
                        Ok(result) => Response::DeclarationVerificationBatch { result },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_source_response(&writer, host_session.as_ref(), response)?;
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
                write_source_response(&writer, host_session.as_ref(), response)?;
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
                write_source_response(&writer, host_session.as_ref(), response)?;
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

fn load_worker_manifest(runtime: &'static LeanRuntime, path: &Path) -> LeanResult<TrustedWorkerCapability<'static>> {
    let authority = worker_authority()?;
    authority.manifest_read(path)?.read_for(path, |path, bytes| {
        load_worker_snapshot(runtime, path, bytes, |p, b| authority.worker(runtime, p, b))
    })
}

fn load_fixture_capability(runtime: &'static LeanRuntime, manifest_path: &Path) -> LeanResult<()> {
    let _capability = load_worker_manifest(runtime, manifest_path)?;
    Ok(())
}

fn call_fixture_mul(
    runtime: &'static LeanRuntime,
    manifest_path: &Path,
    lhs: u64,
    rhs: u64,
) -> Result<u64, WorkerCallError> {
    let capability = load_worker_manifest(runtime, manifest_path)?;
    let mut bindings = WorkerCapabilityBindings::new(&capability);
    bindings
        .fixture_mul("lean_rs_fixture_u64_mul")?
        .call(lhs, rhs)
        .map_err(WorkerCallError::Host)
}

fn trigger_lean_panic(runtime: &'static LeanRuntime, manifest_path: &Path) -> Result<(), WorkerCallError> {
    let capability = load_worker_manifest(runtime, manifest_path)?;
    let mut bindings = WorkerCapabilityBindings::new(&capability);
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

/// Everything about an `OpenHostSession` request that determines which
/// environment the resulting session holds.
///
/// Once the admitted root order and extension epoch match, a
/// different project or mode is a different anchor, a different import list is
/// a different environment, and a different profile is a different set of
/// modules behind the same list. `imports` is a `Vec`, not a set — import
/// order is significant, and the host's own session-pool key treats it so.
#[derive(Clone, Debug, Eq, PartialEq)]
struct HostSessionKey {
    project_root: String,
    mode: HostSessionMode,
    imports: Vec<String>,
    import_profile: LeanWorkerSessionImportProfile,
}

/// Lean plugin initializers can publish process-wide state before an import fails.
#[derive(Default)]
struct GenerationPlugin(std::cell::OnceCell<Option<(String, String)>>);

impl GenerationPlugin {
    fn accepts(&self, selected: Option<(&str, &str)>) -> bool {
        self.0
            .get()
            .is_none_or(|attempted| attempted.as_ref().map(|(image, init)| (image.as_str(), init.as_str())) == selected)
    }

    fn record_attempt(&self, selected: Option<(&str, &str)>) {
        self.0
            .get_or_init(|| selected.map(|(image, init)| (image.to_owned(), init.to_owned())));
    }
}

/// The leaked, session-independent half of a host session: everything a
/// [`LeanSession`] borrows from but that does not depend on the import set.
///
/// Sessions are `'static` because the host and capabilities behind them are
/// leaked; the child cannot free them, so the only defence against unbounded
/// growth is to build at most one anchor per distinct `(project_root, mode)`
/// and reuse it. Its first observed import fixes the root order for this anchor.
/// The artifact readers and the application continue to own stability of the
/// files and prefixes reached through that order.
struct HostAnchor {
    installed_roots: std::cell::OnceCell<Vec<String>>,
    project_root: String,
    mode: HostSessionMode,
    #[allow(dead_code, reason = "leaked host anchors the capability and session lifetimes")]
    host: &'static LeanHost<'static>,
    capabilities: &'static LeanCapabilities<'static, 'static>,
    worker_capability: Option<&'static TrustedWorkerCapability<'static>>,
}

impl HostAnchor {
    fn matches(&self, project_root: &str, mode: &HostSessionMode) -> bool {
        self.project_root == project_root && &self.mode == mode
    }
}

struct HostSessionState {
    observed_roots: Option<&'static [String]>,
    capability: &'static lean_rs::module::TrustedHostCapability<'static>,
    /// Which open produced this session. Compared for equality to decide
    /// reuse, and folded into the module-query cache key so an answer is never
    /// served to a different environment.
    key: HostSessionKey,
    worker_bindings: Option<WorkerCapabilityBindings<'static>>,
    session: LeanSession<'static, 'static>,
}

struct WorkerCapabilityBindings<'cap> {
    capability: &'cap TrustedWorkerCapability<'static>,
    string_io: HashMap<String, WorkerStringBinding<'static, 'cap>>,
    streams: HashMap<String, WorkerStreamBinding<'static, 'cap>>,
    fixture_mul: HashMap<String, WorkerBinaryBinding<'static, 'cap>>,
    fixture_panic: HashMap<String, WorkerUnitBinding<'static, 'cap>>,
}

impl<'cap> WorkerCapabilityBindings<'cap> {
    fn new(capability: &'cap TrustedWorkerCapability<'static>) -> Self {
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
    ) -> Result<&WorkerStringBinding<'static, 'cap>, WorkerCallError> {
        if !self.string_io.contains_key(export) {
            let binding = self
                .capability
                .bind_string_io(export)
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

    fn stream(&mut self, export: &str) -> Result<&WorkerStreamBinding<'static, 'cap>, WorkerCallError> {
        if !self.streams.contains_key(export) {
            let binding = self.capability.bind_stream(export).map_err(|source| {
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

    fn fixture_mul(&mut self, export: &str) -> Result<&WorkerBinaryBinding<'static, 'cap>, WorkerCallError> {
        if !self.fixture_mul.contains_key(export) {
            let binding = self
                .capability
                .bind_binary(export)
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

    fn fixture_panic(&mut self, export: &str) -> Result<&WorkerUnitBinding<'static, 'cap>, WorkerCallError> {
        if !self.fixture_panic.contains_key(export) {
            let binding = self
                .capability
                .bind_unit(export)
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
    Checked(WorkerBindingFailure),
    Internal(String),
}

impl WorkerBindingError {
    fn checked(operation: WorkerExportOperation, export: &str, source: WorkerBindingFailure) -> WorkerCallError {
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

/// Answer one `OpenHostSession` request, reusing a held session when one
/// already describes the requested environment.
///
/// A reuse is the whole point of the exercise: opening a session runs a full
/// `importModules`, and with `loadExts := true` the imported regions are not
/// reclaimable, so a child that re-imports per request grows with the *request
/// count* rather than with the workload.
///
/// The child therefore holds up to `capacity` imported environments — the
/// current one plus `parked`, MRU at the back — instead of one. Dropping a
/// session is what does not help. Measured on a Mathlib-scale project with five
/// unrelated real import profiles, holding every session costs 8,396,800 KiB of
/// `phys_footprint` against 8,110,080 KiB for dropping each immediately — about
/// 70 MiB per extra live environment, against 2.0–4.5 GB for the import that
/// would otherwise have to run again. Re-importing is the expensive half, and a
/// pool turns "one import per switch" into "one import per *distinct* profile".
///
/// The same shape shows up on the workspace fixture — holding four costs 4.19 GB
/// against 4.04 GB for holding one, and an alternating workload costs 7.9 GB and
/// sixteen imports without the pool against 4.19 GB and four with it — but those
/// figures are RSS over modules that all import `Lean` and therefore share ~99%
/// of their closure. Read them as a shape, not a magnitude.
///
/// `LEAN_RS_WORKER_SESSION_POOL_CAPACITY=1` restores the pre-pool child exactly:
/// nothing is ever parked, so every mismatch drops the old session, clears the
/// cache, and re-imports. That is the rollback lever and the test lever.
///
/// Two orderings are load-bearing when a session is actually **dropped** (not
/// merely parked). The retained snapshot cache is cleared first, because a
/// cached snapshot pins the environment of the session that built it; and the
/// eviction completes before the replacement is imported, so a full pool does
/// not transiently exceed its capacity. The cost of that ordering is that a
/// failed import leaves the child without a *current* session — which the parent
/// already handles, retrying on `worker_session_missing` — but `parked` survives
/// it, so the retry may still land on a pooled key.
fn open_or_reuse_host_session(
    anchor: &'static HostAnchor,
    plugin: &GenerationPlugin,
    host_session: &mut Option<HostSessionState>,
    parked: &mut Vec<HostSessionState>,
    capacity: usize,
    import_budget_exhausted: bool,
    key: HostSessionKey,
) -> Response {
    let (profile, scope) = match anchor.import_scope(&key) {
        Ok(request) => request,
        Err(err) => return error_response(&err),
    };
    if !plugin.accepts(scope.plugin()) {
        return Response::HostArtifactEpochChanged;
    }
    // Before any reuse decision, and before capacity eviction: dropping dead
    // entries can only free slots, so the LRU loop below never discards a live
    // entry while a dead one is still in the list.
    //
    // A stale *current* session becomes `None` here and falls through to the
    // ordinary import path, reporting `HostSessionOpened`. The parent's import
    // accounting therefore stays honest with no new wire signal.
    evict_stale_host_sessions(host_session, parked);

    let current = host_session.as_ref().filter(|state| state.key == key);
    let parked_index = parked.iter().position(|state| state.key == key);
    let candidate = current.or_else(|| parked_index.and_then(|index| parked.get(index)));
    let can_reuse = if let Some(state) = candidate
        && let Some(expected) = state.observed_roots
    {
        match state.session.compare_import_roots(&scope, expected) {
            Ok(Some(true)) => true,
            Ok(Some(false)) => return Response::HostArtifactEpochChanged,
            Ok(None) => false,
            Err(err) => return error_response(&err),
        }
    } else {
        false
    };
    if can_reuse && let Some(state) = current {
        // Restated from the open that built this environment; `import_stats`
        // is computed once at import and stored, so it still describes it.
        return Response::HostSessionReused {
            import_stats: worker_import_stats(state.session.import_stats()),
        };
    }

    // A pooled hit: no clear and no import. The snapshot cache survives because
    // the environment that built it does, which is sound only because `mode`
    // now joins the module-query cache key — two live sessions over the same
    // root, imports, and profile are otherwise indistinguishable to it.
    if can_reuse && let Some(index) = parked_index {
        let state = parked.remove(index);
        if let Some(outgoing) = host_session.take() {
            outgoing.park_if_reusable(parked);
        }
        let import_stats = worker_import_stats(state.session.import_stats());
        *host_session = Some(state);
        return Response::HostSessionReused { import_stats };
    }

    // Only an actual cache miss can consume another session import.
    if import_budget_exhausted {
        return Response::HeaderImportRecycle;
    }
    if let Some(outgoing) = host_session.take() {
        outgoing.park_if_reusable(parked);
    }
    // `capacity` counts the session this call is about to make current, so the
    // pool may keep at most `capacity - 1`. Evicting from the front is LRU.
    while parked.len() >= capacity {
        let evicted = parked.remove(0);
        // Lean's snapshot cache is process-global and clears all-or-nothing, so
        // this also discards entries belonging to sessions that are still live.
        // That costs recomputation and nothing else, and it is the only way to
        // stop a retained snapshot from pinning the environment being dropped.
        //
        // This path only became reachable when pool capacity stopped being
        // derived from the parent's restart bound, so the collateral clear is a
        // new cost. It is self-limiting rather than merely tolerable: a parent
        // using a byte-denominated restart bound recycles the child after
        // `budget / residue-per-import` imports, so capacity binds only where
        // that quotient exceeds `capacity` — which is exactly where imports are
        // small. At Mathlib scale (2–4 GB retained per import against a
        // 9 GiB floor) roughly four imports fit in a generation and this loop
        // never runs; on a project importing 50 MB at a time it runs often, and
        // there both the discarded snapshots and the re-import are cheap.
        evicted.evict();
    }

    plugin.record_attempt(scope.plugin());
    match HostSessionState::open(anchor, key, profile, &scope) {
        Ok(None) => Response::HostArtifactEpochChanged,
        Ok(Some(state)) => {
            let import_stats = worker_import_stats(state.session.import_stats());
            *host_session = Some(state);
            // The import that just completed is the only moment the extension
            // registry can move: `runSessionImport` is the sole registration
            // site reachable in this embedding. Everything else either runs
            // after `lean_io_mark_end_initialization` (where registration
            // throws) or imports with `loadExts := false`. The session set
            // above is definitionally not the one this can drop.
            evict_stale_host_sessions(host_session, parked);
            Response::HostSessionOpened { import_stats }
        }
        Err(err) => error_response(&err),
    }
}

/// Drop every held session whose environment predates an environment-extension
/// registration.
///
/// `Environment.extensions` is sized exactly once, by `mkInitialExtensionStates`
/// inside `finalizeImport`. The only growth path,
/// `EnvExtension.ensureExtensionsArraySize`, runs solely from
/// `finalizePersistentExtensions` for the environment *being imported*, and both
/// it and the `extensions` field are `private` — so an environment that outlives
/// a `registerEnvExtension` can never be repaired. Registration during import is
/// ordinary rather than exotic: `Lean.initializing` is true for all of
/// `importModules`, which is how every user `initialize` block and every
/// `register_simp_attr` comes into existence.
///
/// Serving a query from such an environment is not a degraded answer, it is a
/// dead child. `ScopedEnvExtension.pushScope` / `popScope` / `setDelimitsLocal`
/// / `activateScoped` walk the process-global `scopedEnvExtensionsRef` *without*
/// filtering against the environment they are modifying, so each out-of-range
/// slot reaches `panic! "invalid environment extension has been accessed"`.
/// Their callers are ordinary elaboration: every `namespace`, every `section`,
/// every `open … in`.
///
/// `host_session` and `parked` are swept together on purpose. Today only an
/// import performed outside this function can leave the *current* session
/// behind, but the two fields are one pool split for borrow reasons; letting
/// them diverge is what would open a hole the first time another request path
/// imports.
///
/// No logging, deliberately: the child has no `tracing` dependency, and writing
/// to a stderr pipe the parent drains only after exit is the failure mode this
/// function exists to prevent.
fn evict_stale_host_sessions(host_session: &mut Option<HostSessionState>, parked: &mut Vec<HostSessionState>) {
    // Any live session will do: the stamp reads process-global registries, not
    // anything belonging to the session it is read through.
    let Some(probe) = host_session.as_ref().or_else(|| parked.first()) else {
        return;
    };
    // `None` compares unequal to every recorded stamp, so a failed read means
    // "assume everything is stale". That costs one re-import per held profile
    // and is the only direction that cannot leave a short-`extensions`
    // environment in service.
    let live = probe.session.live_extension_registry_epoch().ok();
    let is_stale = |state: &HostSessionState| live != Some(state.session.extension_registry_epoch());
    if !host_session.as_ref().is_some_and(is_stale) && !parked.iter().any(&is_stale) {
        return;
    }
    // Clear before any drop, for the same reason the capacity eviction does: a
    // retained snapshot pins the environment that built it. Lean's cache is
    // process-global and all-or-nothing, so one clear covers every held session.
    if let Some(state) = host_session.as_mut().or_else(|| parked.first_mut()) {
        drop(state.clear_module_snapshot_cache());
    }
    for state in parked.iter_mut().filter(|state| is_stale(state)) {
        drop(state.session.clear_module_header_cache());
    }
    parked.retain(|state| !is_stale(state));
    if host_session.as_ref().is_some_and(is_stale) {
        if let Some(state) = host_session.as_mut() {
            drop(state.session.clear_module_header_cache());
        }
        *host_session = None;
    }
}

fn ready_host_anchor(
    runtime: &'static LeanRuntime,
    anchors: &mut Vec<&'static HostAnchor>,
    project_root: &str,
    mode: &HostSessionMode,
    admission: Option<&SessionAdmission>,
    ordinary_prepared: &mut Option<LeanPreparedHostArtifacts>,
) -> LeanResult<&'static HostAnchor> {
    if let Some(anchor) = anchors.iter().find(|anchor| anchor.matches(project_root, mode)) {
        return Ok(*anchor);
    }
    let anchor = HostAnchor::build(runtime, project_root, mode, admission, ordinary_prepared)?;
    anchors.push(anchor);
    Ok(anchor)
}

/// Bootstrap owns native loading. The application supplies only an authorization
/// under custody and cannot replace the worker's admitted bytes or runtime.
fn load_worker_snapshot(
    runtime: &'static LeanRuntime,
    path: &Path,
    bytes: &[u8],
    authorize: impl FnOnce(&Path, &[u8]) -> LeanResult<lean_rs::module::AuthorizedWorkerSnapshot<'static>>,
) -> LeanResult<TrustedWorkerCapability<'static>> {
    authorize(path, bytes)?.load_for(runtime, path, bytes)
}

fn prepare_ordinary_host_artifacts() -> LeanResult<LeanPreparedHostArtifacts> {
    #[cfg(unix)]
    {
        match lean_rs_host::host::ready_host_artifacts_locally()? {
            Some(prepared) => Ok(prepared),
            None => lean_rs_host::host::prepare_host_artifacts_without_lock_wait(),
        }
    }
    #[cfg(not(unix))]
    lean_rs_host::host::prepare_host_artifacts()
}

impl HostAnchor {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "all future guarded mode variants must refuse before loading"
    )]
    fn build(
        runtime: &'static LeanRuntime,
        project_root: &str,
        mode: &HostSessionMode,
        admission: Option<&SessionAdmission>,
        ordinary_prepared: &mut Option<LeanPreparedHostArtifacts>,
    ) -> LeanResult<&'static Self> {
        let host = Box::leak(Box::new(LeanHost::from_lake_project(runtime, Path::new(project_root))?));
        let authority = worker_authority()?;
        let manifest = match mode {
            HostSessionMode::Capability { manifest_path, .. } => manifest_path.as_deref(),
            HostSessionMode::ShimsOnly => None,
            _ => return Err(host_internal("unsupported host session loading mode")),
        };
        // Prepare before the first grant and never rebuild under earlier grants.
        // The ordinary ready probe releases its read locks before calling the
        // application, which must independently verify and hold custody. Managed
        // admission carries its own descriptor and its load order is unchanged.
        let prepared = match (admission, ordinary_prepared) {
            (Some(admission), _) => &admission.prepared,
            (None, Some(prepared)) => prepared,
            (None, missing) => missing.insert(prepare_ordinary_host_artifacts()?),
        };
        let load = |path: &Path, bytes: &[u8]| {
            load_worker_snapshot(runtime, path, bytes, |p, b| authority.worker(runtime, p, b))
        };
        let worker_capability = if let Some(admission) = admission {
            admission
                .capability
                .as_ref()
                .map(|s| load(s.path(), s.bytes()))
                .transpose()?
        } else if let Some(path) = manifest {
            Some(load_worker_manifest(runtime, Path::new(path))?)
        } else {
            None
        };
        let selected;
        let prepared = if let (None, Some(image)) = (admission, &worker_capability)
            && authority.use_generated_project_imports(Path::new(project_root), image)?
        {
            selected = prepared.clone().for_generated_project(host, image)?;
            &selected
        } else {
            prepared
        };
        let shim = authority.host(runtime, prepared)?;
        let capabilities = match mode {
            HostSessionMode::Capability {
                package,
                lib_name,
                manifest_path: None,
            } => {
                let library = authority.project(
                    runtime,
                    &host.project_library_path(package, lib_name),
                    &lean_rs::module::LeanModuleInitializer::new(package, lib_name),
                )?;
                LeanCapabilities::from_prepared_capability(host, prepared, shim, library, package, lib_name)?
            }
            _ => LeanCapabilities::from_prepared(host, prepared, shim)?,
        };
        let capabilities = &*Box::leak(Box::new(capabilities));
        let worker_capability = worker_capability.map(|image| &*Box::leak(Box::new(image)));
        Ok(Box::leak(Box::new(Self {
            installed_roots: std::cell::OnceCell::new(),
            project_root: project_root.to_owned(),
            mode: mode.clone(),
            host,
            capabilities,
            worker_capability,
        })))
    }

    fn import_scope(
        &'static self,
        key: &HostSessionKey,
    ) -> LeanResult<(LeanSessionImportProfile, TrustedHostImports<'static, 'static>)> {
        let profile = host_import_profile(key.import_profile)?;
        let options = lean_rs_host::LeanImportProfileMode::FullSession(profile)
            .native_options(&lean_rs_host::LeanImportProfilerOptions::default());
        let paths = self.capabilities.import_search_paths();
        let scope = worker_authority()?.imports(
            self.capabilities.shim_capability(),
            paths.clone(),
            key.imports.clone(),
            options,
        )?;
        if !scope.matches_request(&paths, &key.imports) {
            return Err(host_internal(
                "worker authority returned different import names or ordered search paths",
            ));
        }
        Ok((profile, scope))
    }
}

fn with_authorized_worker_string(
    request: String,
    authorize: impl FnOnce(&str) -> Result<(), WorkerCallError>,
) -> Result<String, WorkerCallError> {
    authorize(&request)?;
    Ok(request)
}

fn with_validated_source<C, R>(
    context: C,
    observed_roots: Option<&[String]>,
    validate: impl FnOnce(&C, Option<&[String]>) -> LeanResult<()>,
    grant: impl FnOnce(C) -> LeanResult<R>,
) -> LeanResult<R> {
    validate(&context, observed_roots)?;
    grant(context)
}

impl HostSessionState {
    fn open(
        anchor: &'static HostAnchor,
        key: HostSessionKey,
        profile: LeanSessionImportProfile,
        scope: &TrustedHostImports<'static, 'static>,
    ) -> LeanResult<Option<Self>> {
        let Some((session, roots)) = LeanSession::open_with_retained_roots(
            anchor.capabilities,
            scope,
            profile,
            anchor.installed_roots.get().map(Vec::as_slice),
        )?
        else {
            return Ok(None);
        };
        let observed_roots = roots.map(|roots| anchor.installed_roots.get_or_init(|| roots).as_slice());
        Ok(Some(Self {
            observed_roots,
            capability: anchor.capabilities.shim_capability(),
            worker_bindings: anchor.worker_capability.map(WorkerCapabilityBindings::new),
            session,
            key,
        }))
    }

    fn park_if_reusable(self, parked: &mut Vec<Self>) {
        if self.observed_roots.is_some() {
            parked.push(self);
        } else {
            self.evict();
        }
    }

    fn evict(mut self) {
        drop(self.clear_module_snapshot_cache());
        drop(self.session.clear_module_header_cache());
    }

    fn worker_bindings_for(
        &mut self,
        operation: WorkerExportOperation,
        export: &str,
    ) -> Result<&mut WorkerCapabilityBindings<'static>, WorkerCallError> {
        self.worker_bindings.as_mut().ok_or_else(|| {
            WorkerCallError::Binding(WorkerBindingError::internal(
                operation,
                export,
                "worker request requires a manifest-backed capability identity, but this session has no capability manifest",
            ))
        })
    }

    fn grant_source<R>(
        &self,
        grant: impl FnOnce(&dyn WorkerAuthority, WorkerSourceContext<'_, 'static>) -> LeanResult<R>,
    ) -> LeanResult<R> {
        let authority = worker_authority()?;
        let (environment, imports) = self.session.source_context();
        let context = WorkerSourceContext {
            capability: self.capability,
            environment,
            imports,
        };
        with_validated_source(
            context,
            self.observed_roots,
            |context, roots| authority.validate_source_context(context, roots),
            |context| grant(authority, context),
        )
    }

    fn elaborate_term(
        &mut self,
        source: &str,
        options: &lean_rs_host::LeanElabOptions,
    ) -> LeanResult<Result<lean_rs::LeanExpr<'static>, lean_rs_host::LeanElabFailure>> {
        let request = self
            .grant_source(|authority, context| authority.term(context, source.to_owned(), options.source_options()))?;
        self.session.elaborate(&request, None)
    }

    fn elaborate(&mut self, source: &str, options: &LeanWorkerElabOptions) -> LeanResult<LeanWorkerElabResult> {
        let options = elab_options_to_host(options);
        let outcome = self.elaborate_term(source, &options)?;
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
        let req = self.grant_source(|authority, context| {
            authority.kernel(context, source.to_owned(), options.source_options())
        })?;
        let outcome = self.session.kernel_check(&req, None, None)?;
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

        let status = self
            .worker_bindings_for(WorkerExportOperation::StreamingCommand, export)
            .map_err(StreamRunError::from_worker_call)?
            .stream(export)
            .map_err(StreamRunError::from_worker_call)?
            .call(request_json, &callback)
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
            .worker_string_call(WorkerExportOperation::Metadata, export, request_json)
            .map_err(CapabilityJsonError::from_worker_call)?;
        serde_json::from_str(&raw).map_err(|err| CapabilityJsonError::Malformed(err.to_string()))
    }

    fn capability_doctor(
        &mut self,
        export: &str,
        request_json: String,
    ) -> Result<LeanWorkerDoctorReport, CapabilityJsonError> {
        let raw = self
            .worker_string_call(WorkerExportOperation::Doctor, export, request_json)
            .map_err(CapabilityJsonError::from_worker_call)?;
        serde_json::from_str(&raw).map_err(|err| CapabilityJsonError::Malformed(err.to_string()))
    }

    fn json_command(&mut self, export: &str, request_json: String) -> Result<String, WorkerCallError> {
        self.worker_string_call(WorkerExportOperation::JsonCommand, export, request_json)
    }

    fn worker_string_call(
        &mut self,
        operation: WorkerExportOperation,
        export: &str,
        request_json: String,
    ) -> Result<String, WorkerCallError> {
        with_authorized_worker_string(request_json, |request_json| {
            if let Some(bindings) = &self.worker_bindings {
                worker_authority()
                    .and_then(|authority| {
                        authority.authorize_worker_string_call(WorkerStringCall {
                            capability: bindings.capability,
                            project_root: &self.key.project_root,
                            operation,
                            export,
                            request_json,
                        })
                    })
                    .map_err(WorkerCallError::Host)?;
            }
            Ok(())
        })
        .and_then(|request_json| {
            self.worker_bindings_for(operation, export)?
                .string_io(operation, export)?
                .call(request_json)
                .map_err(WorkerCallError::Host)
        })
    }

    fn infer_type(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
    ) -> LeanResult<LeanWorkerMetaResult<LeanWorkerRendered>> {
        let elab_options = elab_options_to_host(options);
        let elab_outcome = self.elaborate_term(source, &elab_options)?;
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
        let elab_outcome = self.elaborate_term(source, &elab_options)?;
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
        let lhs_outcome = self.elaborate_term(lhs, &elab_options)?;
        let lhs_expr = match lhs_outcome {
            Ok(expr) => expr,
            Err(failure) => return Ok(meta_failure_from_elab(&failure)),
        };
        let rhs_outcome = self.elaborate_term(rhs, &elab_options)?;
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
        self.session
            .search_declarations(&declaration_search_host(search), None)
            .map(declaration_search_wire)
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

    fn inspect_declaration(
        &mut self,
        request: &LeanWorkerDeclarationInspectionRequest,
    ) -> LeanResult<LeanWorkerDeclarationInspectionResult> {
        self.session
            .inspect_declaration(&declaration_inspection_host(request), None)
            .map(declaration_inspection_result_wire)
    }

    fn attempt_proof(
        &mut self,
        request: &LeanWorkerProofAttemptRequest,
        options: &LeanWorkerElabOptions,
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<LeanWorkerProofAttemptResult> {
        if progress {
            emit_progress(writer, "attempt_proof", 0, Some(1));
        }
        let request = proof_attempt_request_host(request)?;
        let options = elab_options_to_host(options);
        let request =
            self.grant_source(|authority, context| authority.proof(context, request, options.source_options()))?;
        let result = self.session.attempt_proof(&request, None)?;
        if progress {
            emit_progress(writer, "attempt_proof", 1, Some(1));
        }
        Ok(proof_attempt_outcome_wire(result))
    }

    fn verify_declaration(
        &mut self,
        request: &LeanWorkerDeclarationVerificationRequest,
        options: &LeanWorkerElabOptions,
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<LeanWorkerDeclarationVerificationResult> {
        if progress {
            emit_progress(writer, "verify_declaration", 0, Some(1));
        }
        let request = declaration_verification_request_host(request)?;
        let options = elab_options_to_host(options);
        let request =
            self.grant_source(|authority, context| authority.verify(context, request, options.source_options()))?;
        let result = self.session.verify_declaration(&request, None)?;
        if progress {
            emit_progress(writer, "verify_declaration", 1, Some(1));
        }
        Ok(taint_verification_under_memory_pressure(
            declaration_verification_outcome_wire(result),
        ))
    }

    fn verify_declaration_batch(
        &mut self,
        request: &LeanWorkerDeclarationVerificationBatchRequest,
        options: &LeanWorkerElabOptions,
        progress: bool,
        writer: &ProtocolWriter,
    ) -> LeanResult<LeanWorkerDeclarationVerificationBatchResult> {
        if progress {
            emit_progress(writer, "verify_declaration_batch", 0, Some(1));
        }
        let request = declaration_verification_batch_request_host(request)?;
        let options = elab_options_to_host(options);
        let request =
            self.grant_source(|authority, context| authority.verify_batch(context, request, options.source_options()))?;
        let result = self.session.verify_declaration_batch(&request, None)?;
        if progress {
            emit_progress(writer, "verify_declaration_batch", 1, Some(1));
        }
        Ok(taint_verification_batch_under_memory_pressure(
            declaration_verification_batch_outcome_wire(result),
        ))
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
        let request = self.grant_source(|authority, context| {
            authority.query(
                context,
                lean_rs::host_bridge::HostModuleQuery {
                    source: source.into(),
                    query,
                },
                options.source_options(),
            )
        })?;
        Ok(match self.session.process_module_query(&request, None)? {
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
            ModuleQueryOutcome::HeaderParseFailed { diagnostics } => LeanWorkerModuleQueryOutcome::HeaderParseFailed {
                diagnostics: elab_failure_wire(&diagnostics),
            },
            ModuleQueryOutcome::Unsupported => LeanWorkerModuleQueryOutcome::Unsupported,
        })
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
        let batch = lean_rs::host_bridge::HostModuleBatch {
            source: source.into(),
            selectors,
            budgets,
        };
        let policy = format!(
            "{}\n{}\n{}\n{}\n{}",
            policy.file_identity, policy.key, policy.max_entries, policy.ttl_millis, policy.max_bytes
        );
        let cached = self.grant_source(|authority, context| {
            authority.cached(
                context,
                lean_rs::host_bridge::HostCachedModuleBatch {
                    batch: batch.clone(),
                    policy,
                },
                options.source_options(),
            )
        })?;
        let cached = self.session.process_module_query_batch_cached(&cached, None)?;
        if !matches!(cached, ModuleQueryBatchCachedOutcome::Unsupported) {
            return Ok(module_query_batch_cached_outcome_wire(cached));
        }
        let request =
            self.grant_source(|authority, context| authority.batch(context, batch, options.source_options()))?;
        Ok(match self.session.process_module_query_batch(&request, None)? {
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
        })
    }

    fn clear_module_snapshot_cache(&mut self) -> LeanResult<LeanWorkerModuleSnapshotCacheClearResult> {
        let result = self.session.clear_module_snapshot_cache()?;
        Ok(module_snapshot_cache_clear_result_wire(&result))
    }

    fn clear_module_snapshot_cache_for_rss_guard(&mut self) -> LeanResult<()> {
        let guard_kib = module_cache_env_u64("LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB", MODULE_CACHE_DEFAULT_RSS_GUARD_KIB);
        if should_clear_for_rss_guard(guard_kib, current_rss_kib()) {
            let _cleared = self.session.clear_module_snapshot_cache()?;
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
            key: module_query_cache_key(
                &self.key.project_root,
                &self.key.mode,
                source,
                &self.key.imports,
                self.key.import_profile,
                options,
                &file_identity,
            ),
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
        .lean_options(options.lean_options.clone())
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
    elab_failure_wire_with_space(failure, LeanWorkerSourceCoordinateSpace::OriginalSource)
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

fn declaration_search_host(search: &LeanWorkerDeclarationSearch) -> DeclarationSearchRequest {
    DeclarationSearchRequest {
        name_fragment: search.name_fragment.clone(),
        name_match: declaration_name_match_host(search.name_match),
        kind: search.kind.clone(),
        required_constants: search.required_constants.clone(),
        conclusion_head: search.conclusion_head.clone(),
        scope_biases: search.scope_biases.iter().map(declaration_search_bias_host).collect(),
        limit: search.limit.clamp(1, 100),
        filter: LeanDeclarationFilter {
            include_private: search.filter.include_private,
            include_generated: search.filter.include_generated,
            include_internal: search.filter.include_internal,
        },
        include_source: search.include_source,
    }
}

fn derived_work_facts_wire(facts: &LeanDerivedWorkFacts) -> LeanWorkerDerivedWorkFacts {
    LeanWorkerDerivedWorkFacts {
        source_range_lookups: facts.source_range_lookups,
        docstring_lookups: facts.docstring_lookups,
        raw_type_renderings: facts.raw_type_renderings,
        pretty_prints: facts.pretty_prints,
        proof_search_fact_collections: facts.proof_search_fact_collections,
        simp_extension_lookups: facts.simp_extension_lookups,
        parser_elaborator_runs: facts.parser_elaborator_runs,
        module_snapshot_builds: facts.module_snapshot_builds,
        lazy_discr_tree_import_initialization_observed: facts.lazy_discr_tree_import_initialization_observed,
    }
}

fn declaration_inspection_host(request: &LeanWorkerDeclarationInspectionRequest) -> DeclarationInspectionRequest {
    DeclarationInspectionRequest {
        name: request.name.clone(),
        fields: declaration_inspection_fields_host(request.fields),
        budgets: DeclarationInspectionBudgets {
            per_field_bytes: request.budgets.per_field_bytes,
            total_bytes: request.budgets.total_bytes,
        },
    }
}

fn declaration_inspection_fields_host(fields: LeanWorkerDeclarationInspectionFields) -> DeclarationInspectionFields {
    DeclarationInspectionFields {
        source: fields.source,
        statement: fields.statement,
        docstring: fields.docstring,
        attributes: fields.attributes,
        flags: fields.flags,
        statement_pretty: matches!(fields.rendering, LeanWorkerRendering::Pretty),
        proof_search: fields.proof_search,
    }
}

fn declaration_name_match_host(name_match: LeanWorkerDeclarationNameMatch) -> DeclarationNameMatch {
    match name_match {
        LeanWorkerDeclarationNameMatch::Contains => DeclarationNameMatch::Contains,
        LeanWorkerDeclarationNameMatch::Suffix => DeclarationNameMatch::Suffix,
    }
}

fn declaration_search_scope_host(scope: LeanWorkerDeclarationSearchScope) -> DeclarationSearchScope {
    match scope {
        LeanWorkerDeclarationSearchScope::Namespace => DeclarationSearchScope::Namespace,
        LeanWorkerDeclarationSearchScope::Module => DeclarationSearchScope::Module,
    }
}

fn declaration_search_bias_host(bias: &LeanWorkerDeclarationSearchBias) -> DeclarationSearchBias {
    DeclarationSearchBias {
        scope: declaration_search_scope_host(bias.scope),
        prefix: bias.prefix.clone(),
        strict: bias.strict,
        weight: bias.weight,
    }
}

fn declaration_search_wire(result: DeclarationSearchResult) -> LeanWorkerDeclarationSearchResult {
    LeanWorkerDeclarationSearchResult {
        declarations: result
            .declarations
            .into_iter()
            .map(declaration_search_row_wire)
            .collect(),
        truncated: result.truncated,
        facts: declaration_search_facts_wire(result.facts),
    }
}

fn declaration_search_row_wire(row: DeclarationSearchRow) -> LeanWorkerDeclarationSearchRow {
    LeanWorkerDeclarationSearchRow {
        name: row.name,
        kind: row.kind,
        module: row.module,
        source: row.source.map(source_range_wire),
        match_reason: row.match_reason,
        score: row.score,
        rank: row.rank,
        flags: declaration_flags_wire(row.flags),
    }
}

fn declaration_flags_wire(flags: DeclarationFlags) -> LeanWorkerDeclarationFlags {
    LeanWorkerDeclarationFlags {
        is_private: flags.is_private,
        is_generated: flags.is_generated,
        is_internal: flags.is_internal,
    }
}

fn declaration_search_facts_wire(facts: lean_rs_host::DeclarationSearchFacts) -> LeanWorkerDeclarationSearchFacts {
    LeanWorkerDeclarationSearchFacts {
        declarations_scanned: facts.declarations_scanned,
        after_name_filter: facts.after_name_filter,
        after_kind_filter: facts.after_kind_filter,
        after_required_constants_filter: facts.after_required_constants_filter,
        after_conclusion_filter: facts.after_conclusion_filter,
        after_scope_filter: facts.after_scope_filter,
        source_lookups: facts.source_lookups,
        broad_pruning: facts
            .broad_pruning
            .into_iter()
            .map(|pruning| LeanWorkerDeclarationSearchPruning {
                stage: pruning.stage,
                reason: pruning.reason,
                count: pruning.count,
            })
            .collect(),
        truncated: facts.truncated,
        timings: LeanWorkerDeclarationSearchTimings {
            scan_micros: facts.timings.scan_micros,
            rank_micros: facts.timings.rank_micros,
            source_micros: facts.timings.source_micros,
        },
        derived_work: derived_work_facts_wire(&facts.derived_work),
    }
}

fn declaration_inspection_result_wire(result: DeclarationInspectionResult) -> LeanWorkerDeclarationInspectionResult {
    match result {
        DeclarationInspectionResult::Found { declaration } => LeanWorkerDeclarationInspectionResult::Found {
            declaration: Box::new(declaration_inspection_wire(*declaration)),
        },
        DeclarationInspectionResult::NotFound { name } => LeanWorkerDeclarationInspectionResult::NotFound { name },
        DeclarationInspectionResult::Unsupported => LeanWorkerDeclarationInspectionResult::Unsupported,
    }
}

fn declaration_inspection_wire(declaration: DeclarationInspection) -> LeanWorkerDeclarationInspection {
    LeanWorkerDeclarationInspection {
        name: declaration.name,
        kind: declaration.kind,
        module: declaration.module,
        source: declaration.source.map(source_range_wire),
        statement: declaration.statement.map(declaration_rendered_info_wire),
        docstring: declaration.docstring.map(declaration_rendered_info_wire),
        attributes: declaration.attributes,
        proof_search: declaration_proof_search_facts_wire(declaration.proof_search),
        flags: declaration_flags_wire(declaration.flags),
        derived_work: derived_work_facts_wire(&declaration.derived_work),
        statement_rendering: declaration.statement_pretty.map(|pretty| {
            if pretty {
                LeanWorkerRendering::Pretty
            } else {
                LeanWorkerRendering::Raw
            }
        }),
    }
}

fn declaration_rendered_info_wire(info: DeclarationRenderedInfo) -> LeanWorkerRenderedInfo {
    LeanWorkerRenderedInfo {
        value: info.value,
        truncated: info.truncated,
    }
}

fn declaration_proof_search_facts_wire(facts: DeclarationProofSearchFacts) -> LeanWorkerDeclarationProofSearchFacts {
    LeanWorkerDeclarationProofSearchFacts {
        computed: facts.computed,
        unavailable_reason: facts.unavailable_reason,
        is_simp: facts.is_simp,
        is_rw_candidate: facts.is_rw_candidate,
        is_instance: facts.is_instance,
        is_class: facts.is_class,
        class_name: facts.class_name,
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
        LeanWorkerModuleQuerySelector::ProofStateInDeclaration {
            id,
            declaration,
            position,
            locals_raw,
        } => ModuleQuerySelector::ProofStateInDeclaration {
            id,
            declaration,
            position: proof_position_selector_host(&position),
            locals_raw,
        },
        LeanWorkerModuleQuerySelector::TypeAt { id, line, column } => ModuleQuerySelector::TypeAt { id, line, column },
        LeanWorkerModuleQuerySelector::References { id, name } => ModuleQuerySelector::References { id, name },
        LeanWorkerModuleQuerySelector::DeclarationTarget { id, name, line, column } => {
            ModuleQuerySelector::DeclarationTarget { id, name, line, column }
        }
        LeanWorkerModuleQuerySelector::SurroundingDeclaration { id, line, column } => {
            ModuleQuerySelector::SurroundingDeclaration { id, line, column }
        }
        LeanWorkerModuleQuerySelector::DeclarationOutline { id } => ModuleQuerySelector::DeclarationOutline { id },
        _ => return Err(host_internal("unsupported module query selector variant")),
    })
}

fn module_query_budgets_host(budgets: &LeanWorkerOutputBudgets) -> ModuleQueryOutputBudgets {
    ModuleQueryOutputBudgets {
        per_field_bytes: budgets.per_field_bytes,
        total_bytes: budgets.total_bytes,
    }
}

fn module_source_span_host(span: &LeanWorkerModuleSourceSpan) -> ModuleSourceSpan {
    ModuleSourceSpan {
        start_line: span.start_line,
        start_column: span.start_column,
        end_line: span.end_line,
        end_column: span.end_column,
    }
}

fn proof_edit_target_host(target: &LeanWorkerProofEditTarget) -> LeanResult<ProofEditTarget> {
    Ok(match target {
        LeanWorkerProofEditTarget::Declaration { name, position } => ProofEditTarget::Declaration {
            name: name.clone(),
            position: proof_position_selector_host(position),
        },
        _ => return Err(host_internal("unsupported proof edit target variant")),
    })
}

fn proof_position_selector_host(selector: &LeanWorkerProofPositionSelector) -> lean_rs_host::ProofPositionSelector {
    match selector {
        LeanWorkerProofPositionSelector::Default => lean_rs_host::ProofPositionSelector::Default,
        LeanWorkerProofPositionSelector::Index { index } => {
            lean_rs_host::ProofPositionSelector::Index { index: *index }
        }
        LeanWorkerProofPositionSelector::AfterText { text, occurrence } => {
            lean_rs_host::ProofPositionSelector::AfterText {
                text: text.clone(),
                occurrence: *occurrence,
            }
        }
        LeanWorkerProofPositionSelector::Entry => lean_rs_host::ProofPositionSelector::Entry,
        _ => lean_rs_host::ProofPositionSelector::Default,
    }
}

fn proof_attempt_request_host(request: &LeanWorkerProofAttemptRequest) -> LeanResult<ProofAttemptRequest> {
    Ok(ProofAttemptRequest {
        source: request.source.clone(),
        edit: proof_edit_target_host(&request.edit)?,
        candidates: request
            .candidates
            .iter()
            .map(|candidate| ProofCandidate {
                id: candidate.id.clone(),
                text: candidate.text.clone(),
            })
            .collect(),
        budgets: module_query_budgets_host(&request.budgets),
    })
}

fn declaration_verification_target_host(
    target: &LeanWorkerDeclarationVerificationTarget,
) -> LeanResult<DeclarationVerificationTarget> {
    Ok(match target {
        LeanWorkerDeclarationVerificationTarget::Name { name } => {
            DeclarationVerificationTarget::Name { name: name.clone() }
        }
        LeanWorkerDeclarationVerificationTarget::Span { span } => DeclarationVerificationTarget::Span {
            span: module_source_span_host(span),
        },
        _ => return Err(host_internal("unsupported declaration verification target variant")),
    })
}

fn sorry_policy_host(policy: LeanWorkerSorryPolicy) -> SorryPolicy {
    match policy {
        LeanWorkerSorryPolicy::Allow => SorryPolicy::Allow,
        LeanWorkerSorryPolicy::Deny => SorryPolicy::Deny,
        _ => SorryPolicy::Deny,
    }
}

fn declaration_verification_request_host(
    request: &LeanWorkerDeclarationVerificationRequest,
) -> LeanResult<DeclarationVerificationRequest> {
    Ok(DeclarationVerificationRequest {
        source: request.source.clone(),
        target: declaration_verification_target_host(&request.target)?,
        sorry_policy: sorry_policy_host(request.sorry_policy),
        report_axioms: request.report_axioms,
        budgets: module_query_budgets_host(&request.budgets),
    })
}

fn declaration_verification_batch_item_host(
    item: &LeanWorkerDeclarationVerificationBatchItem,
) -> LeanResult<DeclarationVerificationBatchItem> {
    Ok(DeclarationVerificationBatchItem {
        id: item.id.clone(),
        target: declaration_verification_target_host(&item.target)?,
    })
}

fn declaration_verification_batch_request_host(
    request: &LeanWorkerDeclarationVerificationBatchRequest,
) -> LeanResult<DeclarationVerificationBatchRequest> {
    Ok(DeclarationVerificationBatchRequest {
        source: request.source.clone(),
        targets: request
            .targets
            .iter()
            .map(declaration_verification_batch_item_host)
            .collect::<LeanResult<Vec<_>>>()?,
        sorry_policy: sorry_policy_host(request.sorry_policy),
        report_axioms: request.report_axioms,
        budgets: module_query_budgets_host(&request.budgets),
    })
}

fn module_cache_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
}

fn configure_lean_runtime_memory_limit_from_env() {
    let limit_kib = module_cache_env_u64("LEAN_RS_LEAN_MAX_MEMORY_KIB", 0);
    if limit_kib == 0 {
        return;
    }
    let limit_bytes = limit_kib.saturating_mul(1024);
    let limit_bytes = usize::try_from(limit_bytes).unwrap_or(usize::MAX);
    lean_rs::__host_internals::set_runtime_memory_limit_bytes_for_guardrail(limit_bytes);
}

fn approx_json_bytes<T: serde::Serialize>(value: &T) -> u64 {
    serde_json::to_vec(value).map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
}

/// Key one module-query answer to the environment that produced it.
///
/// Every input that can change the answer has to be here — the whole of
/// [`HostSessionKey`], plus the source and elaboration options. `project_root`
/// and `import_profile` joined the key when sessions became reusable across
/// requests: before, the unconditional clear on every open hid their absence,
/// and the same import *names* under a different project or profile resolve to
/// different `.olean` files.
///
/// `mode` joined it when the child gained a **pool**, which is when the last of
/// that cover disappeared. Two sessions differing only in mode — `ShimsOnly`
/// versus a `Capability` dylib over the same root, imports, and profile — can
/// now be live at once with no clear between them, and
/// `processModuleQueryBatchCached` looks up by key *before* it touches the
/// caller's environment, then projects the stored snapshot's. Without `mode` in
/// the key one would answer from the other's environment.
///
/// Every other field is hashed directly; `mode` goes through its wire encoding,
/// for the reason given at the call.
fn module_query_cache_key(
    project_root: &str,
    mode: &HostSessionMode,
    source: &str,
    imports: &[String],
    import_profile: LeanWorkerSessionImportProfile,
    options: &LeanWorkerElabOptions,
    file_identity: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(MODULE_QUERY_CACHE_API_VERSION.as_bytes());
    hasher.update(b"\0project\0");
    hasher.update(project_root.as_bytes());
    hasher.update(b"\0mode\0");
    // Through the wire encoding rather than field by field: that encoding *is*
    // this type's contract, so it is stable and injective across variants —
    // including ones added later, which a match on this `#[non_exhaustive]`
    // enum would have to route through a wildcard and silently conflate. A
    // digest that shifts on an unrelated protocol edit costs one cold cache in
    // one child; a digest that fails to separate two environments serves a
    // wrong answer.
    match serde_json::to_string(mode) {
        Ok(encoded) => hasher.update(encoded.as_bytes()),
        // Unreachable for a plain tagged enum. The debug form rather than a
        // constant, because a constant would hash every mode alike.
        Err(_) => hasher.update(format!("{mode:?}").as_bytes()),
    }
    hasher.update(b"\0file\0");
    hasher.update(file_identity.as_bytes());
    hasher.update(b"\0source\0");
    hasher.update(source.as_bytes());
    hasher.update(b"\0imports\0");
    for import in imports {
        hasher.update(import.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\0import_profile\0");
    hasher.update(import_profile.label().as_bytes());
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
    hasher.update(lean_toolchain::lean_options_json(&options.lean_options).as_bytes());
    hasher.update(b"\0");
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

fn declaration_outline_result_wire(result: DeclarationOutlineResult) -> LeanWorkerDeclarationOutlineResult {
    LeanWorkerDeclarationOutlineResult {
        declarations: result
            .declarations
            .into_iter()
            .map(declaration_target_info_wire)
            .collect(),
        truncated: result.truncated,
    }
}

fn proof_attempt_status_wire(status: ProofAttemptStatus) -> LeanWorkerProofAttemptStatus {
    match status {
        ProofAttemptStatus::Closed => LeanWorkerProofAttemptStatus::Closed,
        ProofAttemptStatus::Progressed => LeanWorkerProofAttemptStatus::Progressed,
        ProofAttemptStatus::Failed => LeanWorkerProofAttemptStatus::Failed,
        ProofAttemptStatus::Timeout => LeanWorkerProofAttemptStatus::Timeout,
        ProofAttemptStatus::BudgetExceeded => LeanWorkerProofAttemptStatus::BudgetExceeded,
        ProofAttemptStatus::NotAttempted => LeanWorkerProofAttemptStatus::NotAttempted,
        ProofAttemptStatus::Unsupported => LeanWorkerProofAttemptStatus::Unsupported,
    }
}

fn proof_attempt_row_wire(row: ProofAttemptRow) -> LeanWorkerProofAttemptRow {
    LeanWorkerProofAttemptRow {
        id: row.id,
        status: proof_attempt_status_wire(row.status),
        candidate_text: rendered_info_wire(row.candidate_text),
        diagnostics: elab_failure_wire_with_space(&row.diagnostics, LeanWorkerSourceCoordinateSpace::SyntheticBuffer),
        downstream_diagnostics: elab_failure_wire_with_space(
            &row.downstream_diagnostics,
            LeanWorkerSourceCoordinateSpace::SyntheticBuffer,
        ),
        goals: row.goals.into_iter().map(rendered_info_wire).collect(),
        declaration: row.declaration.map(declaration_target_info_wire),
        proof_position: row.proof_position.map(proof_position_summary_wire),
        output_truncated: row.output_truncated,
    }
}

fn proof_position_summary_wire(summary: lean_rs_host::ProofPositionSummary) -> LeanWorkerProofPositionSummary {
    LeanWorkerProofPositionSummary {
        index: summary.index,
        tactic: rendered_info_wire(summary.tactic),
    }
}

fn proof_boundary_candidate_wire(candidate: ProofBoundaryCandidate) -> LeanWorkerProofBoundaryCandidate {
    LeanWorkerProofBoundaryCandidate {
        index: candidate.index,
        kind: candidate.kind,
        source: module_source_span_wire(&candidate.source),
        excerpt: rendered_info_wire(candidate.excerpt),
    }
}

fn proof_attempt_envelope_wire(envelope: ProofAttemptEnvelope) -> LeanWorkerProofAttemptEnvelope {
    LeanWorkerProofAttemptEnvelope {
        candidates: envelope.candidates.into_iter().map(proof_attempt_row_wire).collect(),
        candidate_limit: envelope.candidate_limit,
        candidates_truncated: envelope.candidates_truncated,
        entry_goals: envelope.entry_goals.into_iter().map(rendered_info_wire).collect(),
        locals: envelope.locals.into_iter().map(local_info_wire).collect(),
    }
}

fn proof_attempt_outcome_wire(outcome: ProofAttemptOutcome) -> LeanWorkerProofAttemptResult {
    match outcome {
        ProofAttemptOutcome::Ok { result, imports } => LeanWorkerProofAttemptResult::Ok {
            result: proof_attempt_envelope_wire(result),
            imports,
        },
        ProofAttemptOutcome::MissingImports {
            result,
            imports,
            missing,
        } => LeanWorkerProofAttemptResult::MissingImports {
            result: proof_attempt_envelope_wire(result),
            imports,
            missing,
        },
        ProofAttemptOutcome::HeaderParseFailed { diagnostics } => LeanWorkerProofAttemptResult::HeaderParseFailed {
            diagnostics: elab_failure_wire(&diagnostics),
        },
        ProofAttemptOutcome::Unsupported => LeanWorkerProofAttemptResult::Unsupported,
    }
}

/// A verdict that names a genuine resolution — and so could be a degraded
/// artifact of memory pressure rather than the truth. `Accepted` is excluded
/// (verification is monotone); `Timeout`/`BudgetExceeded` are already honest
/// degraded verdicts; `Unsupported` is a capability fact.
fn verification_status_non_positive(status: LeanWorkerDeclarationVerificationStatus) -> bool {
    use LeanWorkerDeclarationVerificationStatus as S;
    matches!(status, S::NotFound | S::NeedsBuild | S::Rejected | S::Ambiguous)
}

/// Memory-pressure taint: a non-positive verdict produced while the child's RSS
/// is at or above `LEAN_RS_VERIFY_RSS_TAINT_KIB` cannot be trusted to reflect a
/// genuine resolution — the elaboration may have been silently degraded (no Lean
/// diagnostic). Relabel it to `BudgetExceeded` with the axiom set unavailable so
/// the caller sees "could not complete under memory pressure" rather than a
/// misleading `NotFound`/`Rejected`. The ceiling is gated high (off by default)
/// so genuine name-absent queries on a warm mathlib worker are not mislabeled,
/// and a missing RSS sample never taints. This is the worker-internal,
/// authoritative counterpart to the parent's post-hoc `worker_recycled` relabel.
fn taint_verification_under_memory_pressure(
    result: LeanWorkerDeclarationVerificationResult,
) -> LeanWorkerDeclarationVerificationResult {
    let ceiling = module_cache_env_u64("LEAN_RS_VERIFY_RSS_TAINT_KIB", 0);
    apply_memory_pressure_taint(result, ceiling, current_rss_kib())
}

/// Whether the retained module-snapshot cache should be dropped before a
/// cacheable module-query request, parameterised on the guard and the sampled
/// RSS so it is testable without environment or platform state.
///
/// Clears only when the guard is enabled (`guard_kib != 0`), an RSS sample is
/// available, and it is at or above the guard — the same three conditions
/// [`apply_memory_pressure_taint`] applies. An unavailable sample must not
/// clear: sampling shells out to `ps` on non-Linux hosts, so a transient
/// failure would otherwise discard the whole cache on an otherwise healthy
/// child, turning a missing measurement into guaranteed re-elaboration.
const fn should_clear_for_rss_guard(guard_kib: u64, current_rss_kib: Option<u64>) -> bool {
    if guard_kib == 0 {
        return false;
    }
    match current_rss_kib {
        Some(current_kib) => current_kib >= guard_kib,
        None => false,
    }
}

/// Pure core of [`taint_verification_under_memory_pressure`], parameterised on
/// the ceiling and the sampled RSS so it is testable without environment or
/// platform state. Taints only when the taint is enabled (`ceiling != 0`), an
/// RSS sample is available, and it is at or above the ceiling.
fn apply_memory_pressure_taint(
    mut result: LeanWorkerDeclarationVerificationResult,
    ceiling_kib: u64,
    current_rss_kib: Option<u64>,
) -> LeanWorkerDeclarationVerificationResult {
    if ceiling_kib == 0 {
        return result;
    }
    let Some(current_kib) = current_rss_kib else {
        return result;
    };
    if current_kib < ceiling_kib {
        return result;
    }
    // `HeaderParseFailed`, `Unsupported`, and any future variant carry no
    // verdict to relabel.
    let (LeanWorkerDeclarationVerificationResult::Ok {
        verification_status: status,
        facts,
        ..
    }
    | LeanWorkerDeclarationVerificationResult::MissingImports {
        verification_status: status,
        facts,
        ..
    }) = &mut result
    else {
        return result;
    };
    if verification_status_non_positive(*status) {
        *status = LeanWorkerDeclarationVerificationStatus::BudgetExceeded;
        facts.axioms.clear();
        facts.axioms_available = false;
    }
    result
}

fn taint_verification_batch_under_memory_pressure(
    result: LeanWorkerDeclarationVerificationBatchResult,
) -> LeanWorkerDeclarationVerificationBatchResult {
    let ceiling = module_cache_env_u64("LEAN_RS_VERIFY_RSS_TAINT_KIB", 0);
    apply_batch_memory_pressure_taint(result, ceiling, current_rss_kib())
}

fn apply_batch_memory_pressure_taint(
    mut result: LeanWorkerDeclarationVerificationBatchResult,
    ceiling_kib: u64,
    current_rss_kib: Option<u64>,
) -> LeanWorkerDeclarationVerificationBatchResult {
    if ceiling_kib == 0 {
        return result;
    }
    let Some(current_kib) = current_rss_kib else {
        return result;
    };
    if current_kib < ceiling_kib {
        return result;
    }
    let rows = match &mut result {
        LeanWorkerDeclarationVerificationBatchResult::Ok { results, .. }
        | LeanWorkerDeclarationVerificationBatchResult::MissingImports { results, .. } => results,
        LeanWorkerDeclarationVerificationBatchResult::HeaderParseFailed { .. }
        | LeanWorkerDeclarationVerificationBatchResult::Unsupported => return result,
        _ => return result,
    };
    for row in rows {
        if verification_status_non_positive(row.verification_status) {
            row.verification_status = LeanWorkerDeclarationVerificationStatus::BudgetExceeded;
            row.facts.axioms.clear();
            row.facts.axioms_available = false;
        }
    }
    result
}

fn declaration_verification_status_wire(
    status: DeclarationVerificationStatus,
) -> LeanWorkerDeclarationVerificationStatus {
    match status {
        DeclarationVerificationStatus::Accepted => LeanWorkerDeclarationVerificationStatus::Accepted,
        DeclarationVerificationStatus::Rejected => LeanWorkerDeclarationVerificationStatus::Rejected,
        DeclarationVerificationStatus::NotFound => LeanWorkerDeclarationVerificationStatus::NotFound,
        DeclarationVerificationStatus::Ambiguous => LeanWorkerDeclarationVerificationStatus::Ambiguous,
        DeclarationVerificationStatus::Timeout => LeanWorkerDeclarationVerificationStatus::Timeout,
        DeclarationVerificationStatus::BudgetExceeded => LeanWorkerDeclarationVerificationStatus::BudgetExceeded,
        DeclarationVerificationStatus::Unsupported => LeanWorkerDeclarationVerificationStatus::Unsupported,
        DeclarationVerificationStatus::NeedsBuild => LeanWorkerDeclarationVerificationStatus::NeedsBuild,
    }
}

fn declaration_verification_target_wire(
    target: DeclarationVerificationTarget,
) -> LeanWorkerDeclarationVerificationTarget {
    match target {
        DeclarationVerificationTarget::Name { name } => LeanWorkerDeclarationVerificationTarget::Name { name },
        DeclarationVerificationTarget::Span { span } => LeanWorkerDeclarationVerificationTarget::Span {
            span: module_source_span_wire(&span),
        },
    }
}

fn declaration_verification_facts_wire(facts: DeclarationVerificationFacts) -> LeanWorkerDeclarationVerificationFacts {
    LeanWorkerDeclarationVerificationFacts {
        target: facts.target.map(declaration_target_info_wire),
        diagnostics: elab_failure_wire(&facts.diagnostics),
        unresolved_goals: facts.unresolved_goals.into_iter().map(rendered_info_wire).collect(),
        contains_sorry: facts.contains_sorry,
        contains_admit: facts.contains_admit,
        contains_sorry_ax: facts.contains_sorry_ax,
        axioms: facts.axioms,
        axioms_truncated: facts.axioms_truncated,
        output_truncated: facts.output_truncated,
        candidates: facts.candidates.into_iter().map(declaration_target_info_wire).collect(),
        axioms_available: facts.axioms_available,
    }
}

fn declaration_verification_outcome_wire(
    outcome: DeclarationVerificationOutcome,
) -> LeanWorkerDeclarationVerificationResult {
    match outcome {
        DeclarationVerificationOutcome::Ok { status, facts, imports } => LeanWorkerDeclarationVerificationResult::Ok {
            verification_status: declaration_verification_status_wire(status),
            facts: Box::new(declaration_verification_facts_wire(*facts)),
            imports,
        },
        DeclarationVerificationOutcome::MissingImports {
            status,
            facts,
            imports,
            missing,
        } => LeanWorkerDeclarationVerificationResult::MissingImports {
            verification_status: declaration_verification_status_wire(status),
            facts: Box::new(declaration_verification_facts_wire(*facts)),
            imports,
            missing,
        },
        DeclarationVerificationOutcome::HeaderParseFailed { diagnostics } => {
            LeanWorkerDeclarationVerificationResult::HeaderParseFailed {
                diagnostics: elab_failure_wire(&diagnostics),
            }
        }
        DeclarationVerificationOutcome::Unsupported => LeanWorkerDeclarationVerificationResult::Unsupported,
    }
}

fn declaration_verification_batch_row_wire(
    row: DeclarationVerificationBatchRow,
) -> LeanWorkerDeclarationVerificationBatchRow {
    LeanWorkerDeclarationVerificationBatchRow {
        id: row.id,
        target: declaration_verification_target_wire(row.target),
        verification_status: declaration_verification_status_wire(row.status),
        facts: Box::new(declaration_verification_facts_wire(*row.facts)),
    }
}

fn declaration_verification_batch_outcome_wire(
    outcome: DeclarationVerificationBatchOutcome,
) -> LeanWorkerDeclarationVerificationBatchResult {
    match outcome {
        DeclarationVerificationBatchOutcome::Ok { results, imports } => {
            LeanWorkerDeclarationVerificationBatchResult::Ok {
                results: results
                    .into_iter()
                    .map(declaration_verification_batch_row_wire)
                    .collect(),
                imports,
            }
        }
        DeclarationVerificationBatchOutcome::MissingImports {
            results,
            imports,
            missing,
        } => LeanWorkerDeclarationVerificationBatchResult::MissingImports {
            results: results
                .into_iter()
                .map(declaration_verification_batch_row_wire)
                .collect(),
            imports,
            missing,
        },
        DeclarationVerificationBatchOutcome::HeaderParseFailed { diagnostics } => {
            LeanWorkerDeclarationVerificationBatchResult::HeaderParseFailed {
                diagnostics: elab_failure_wire(&diagnostics),
            }
        }
        DeclarationVerificationBatchOutcome::Unsupported => LeanWorkerDeclarationVerificationBatchResult::Unsupported,
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
        proof_boundaries: info
            .proof_boundaries
            .into_iter()
            .map(proof_boundary_candidate_wire)
            .collect(),
        proof_boundaries_truncated: info.proof_boundaries_truncated,
    }
}

fn proof_state_result_wire(result: ProofStateResult) -> LeanWorkerProofStateResult {
    match result {
        ProofStateResult::State(info) => LeanWorkerProofStateResult::State {
            info: Box::new(proof_state_info_wire(*info)),
        },
        ProofStateResult::Unavailable {
            message,
            proof_boundaries,
            proof_boundaries_truncated,
        } => LeanWorkerProofStateResult::Unavailable {
            message,
            proof_boundaries: proof_boundaries
                .into_iter()
                .map(proof_boundary_candidate_wire)
                .collect(),
            proof_boundaries_truncated,
        },
        ProofStateResult::Ambiguous { candidates } => LeanWorkerProofStateResult::Ambiguous {
            candidates: candidates.into_iter().map(declaration_target_info_wire).collect(),
        },
        ProofStateResult::NeedsBuild { missing } => LeanWorkerProofStateResult::NeedsBuild { missing },
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
        ModuleQueryBatchResult::DeclarationOutline(result) => {
            LeanWorkerModuleQueryBatchResult::DeclarationOutline(declaration_outline_result_wire(result))
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
        resource: None,
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
    diagnostics_with_space(failure, LeanWorkerSourceCoordinateSpace::OriginalSource)
}

fn elab_failure_wire_with_space(
    failure: &LeanElabFailure,
    coordinate_space: LeanWorkerSourceCoordinateSpace,
) -> LeanWorkerElabFailure {
    LeanWorkerElabFailure {
        diagnostics: diagnostics_with_space(failure, coordinate_space),
        truncated: failure.truncated(),
    }
}

fn diagnostics_with_space(
    failure: &LeanElabFailure,
    coordinate_space: LeanWorkerSourceCoordinateSpace,
) -> Vec<LeanWorkerDiagnostic> {
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
            let original_range = match coordinate_space {
                LeanWorkerSourceCoordinateSpace::OriginalSource => {
                    line.zip(column)
                        .map(|(start_line, start_column)| LeanWorkerSourceRange {
                            file: diagnostic.file_label().to_owned(),
                            start_line,
                            start_column,
                            end_line: end_line.unwrap_or(start_line),
                            end_column: end_column.unwrap_or(start_column),
                        })
                }
                LeanWorkerSourceCoordinateSpace::SyntheticBuffer | LeanWorkerSourceCoordinateSpace::Unknown => None,
                _ => None,
            };
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
                coordinate_space,
                original_range,
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

#[cfg(test)]
mod tests {
    use super::{
        HostSessionKey, apply_memory_pressure_taint, module_query_cache_key, should_clear_for_rss_guard,
        verification_status_non_positive,
    };
    use lean_rs_worker_protocol::protocol::HostSessionMode;
    use lean_rs_worker_protocol::types::{
        LeanWorkerDeclarationVerificationFacts, LeanWorkerDeclarationVerificationResult,
        LeanWorkerDeclarationVerificationStatus, LeanWorkerElabOptions, LeanWorkerSessionImportProfile,
    };

    #[test]
    fn string_export_requires_approval_of_the_exact_unmodified_payload() {
        let exact = r#"{"name":"admitted","limit":3}"#;
        for (input, allowed) in [(exact, true), (r#"{"name":"other","limit":3}"#, false)] {
            let mut entered = false;
            let result = super::with_authorized_worker_string(input.to_owned(), |observed| {
                if observed == exact {
                    Ok(())
                } else {
                    Err(super::WorkerCallError::Host(lean_rs::error::host_internal(
                        "payload refused",
                    )))
                }
            })
            .inspect(|_| entered = true);
            assert_eq!(entered, allowed);
            assert_eq!(result.ok().as_deref(), allowed.then_some(input));
        }
    }

    #[test]
    fn source_grant_requires_validation_of_the_retained_import_order() {
        let expected = ["project", "dependency", "project"].map(str::to_owned);
        let reordered = ["dependency", "project", "project"].map(str::to_owned);
        for (observed, allowed) in [
            (None, false),
            (Some(reordered.as_slice()), false),
            (Some(expected.as_slice()), true),
        ] {
            let mut granted = false;
            let result = super::with_validated_source(
                expected.as_slice(),
                observed,
                |context, roots| {
                    if roots == Some(*context) {
                        Ok(())
                    } else {
                        Err(lean_rs::error::host_internal("unapproved import order"))
                    }
                },
                |context| {
                    granted = true;
                    Ok(context)
                },
            );
            assert_eq!(granted, allowed);
            assert_eq!(result.ok(), allowed.then_some(expected.as_slice()));
        }
    }

    #[test]
    fn metadata_frame_omits_only_complete_order_when_over_budget() -> Result<(), Box<dyn std::error::Error>> {
        use lean_rs_worker_protocol::protocol::{Message, Response, read_frame};
        use lean_rs_worker_protocol::types::{LeanWorkerArtifactReadEpoch, LeanWorkerHostRuntimeMetadata};

        let base = LeanWorkerHostRuntimeMetadata {
            compiled_lean_version: "4.36.0".into(),
            compiled_header_sha256: "a".repeat(64),
            runtime_lean_version: Some("4.36.0".into()),
            checked_host_bindings: std::collections::BTreeMap::new(),
            artifact_read_epochs: Some(vec![LeanWorkerArtifactReadEpoch {
                project_root: "/fixture".into(),
                epoch: "17".into(),
            }]),
            effective_import_roots: None,
        };
        let message = |metadata| Message::Response(Response::HostRuntimeMetadata { metadata });
        let mut full = base.clone();
        let roots = vec![r#"/fixture/a"b"#.into(); 3];
        assert!(super::copy_import_roots_for_frame(&roots, 2).is_none());
        full.effective_import_roots = super::copy_import_roots_for_frame(&roots, u32::MAX);
        assert_eq!(full.effective_import_roots.as_ref(), Some(&roots));
        let mut bytes = Vec::new();
        super::write_worker_frame(&mut bytes, message(full.clone()), u32::MAX)?;
        let exact_cap = u32::try_from(bytes.len() - 4)?;
        for (cap, expected) in [(exact_cap, full.clone()), (exact_cap - 1, base)] {
            bytes.clear();
            super::write_worker_frame(&mut bytes, message(full.clone()), cap)?;
            let mut received = bytes.as_slice();
            assert_eq!(read_frame(&mut received, cap)?.message, message(expected));
            assert!(received.is_empty());
        }
        Ok(())
    }

    #[test]
    fn bootstrap_rejects_valid_authorization_for_another_manifest_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        // A real, test-owned capability suffices for this byte-identity check.
        let owner = tempfile::tempdir()?;
        let project = owner.path().join("lean");
        std::fs::create_dir(&project)?;
        std::fs::write(
            owner.path().join("Cargo.toml"),
            "[package]\nname = \"snapshot-test\"\nversion = \"0.0.0\"\n",
        )?;
        std::fs::write(
            project.join("lean-toolchain"),
            format!("leanprover/lean4:v{}\n", lean_toolchain::LEAN_VERSION),
        )?;
        std::fs::write(
            project.join("lakefile.toml"),
            "name = \"snapshot\"\n[[lean_lib]]\nname = \"Snapshot\"\n",
        )?;
        std::fs::write(
            project.join("lake-manifest.json"),
            r#"{"version":"1.2.0","packagesDir":".lake/packages","packages":[],"name":"snapshot","lakeDir":".lake"}"#,
        )?;
        std::fs::write(project.join("Snapshot.lean"), "def Snapshot.value : Nat := 1\n")?;
        let prepared = lean_toolchain::CargoLeanCapability::new(&project, "Snapshot")
            .package("snapshot")
            .module("Snapshot")
            .private_bootstrap(lean_toolchain::shared_writer::PrivateLakeBootstrap::new(
                owner.path(),
                &project,
            )?)
            .build_quiet()?;
        let runtime = lean_rs::LeanRuntime::init()?;
        let path = prepared.manifest_path();
        let snapshot = std::fs::read(path)?;
        let admitted = snapshot.as_slice();
        let mut other = admitted.to_vec();
        other.push(b'\n');
        let result = super::load_worker_snapshot(runtime, path, admitted, |requested_path, bytes| {
            assert_eq!(requested_path, path);
            assert_eq!(bytes, admitted);
            #[allow(
                unsafe_code,
                reason = "the test owns the private fixture and approves no export calls"
            )]
            // SAFETY: other names only our freshly compiled Snapshot module, with no dependencies or exports.
            // The production bootstrap must refuse its distinct bytes before loading it.
            Ok(unsafe {
                lean_rs::module::AuthorizedWorkerSnapshot::attest(runtime, path.to_owned(), other, vec![], owner)
            })
        });
        assert!(matches!(result, Err(error) if error.to_string().contains("different manifest snapshot")));
        Ok(())
    }

    fn session_key() -> HostSessionKey {
        HostSessionKey {
            project_root: "/proj".to_owned(),
            mode: HostSessionMode::ShimsOnly,
            imports: vec!["Init".to_owned(), "Std".to_owned()],
            import_profile: LeanWorkerSessionImportProfile::Private,
        }
    }

    /// Alongside matching roots and registry epoch, every key field has to
    /// participate: a key that ignored one would reuse an environment the
    /// request did not ask for, and every answer computed from it would be
    /// quietly wrong rather than merely stale.
    #[test]
    fn first_import_attempt_fixes_plugin_for_the_worker_generation() {
        let plugin = super::GenerationPlugin::default();
        let first = Some(("/fixture/Plugin.dylib", "initialize_Plugin"));
        assert!(plugin.accepts(first));
        // The import may fail after running the initializer, before it creates a session.
        plugin.record_attempt(first);
        assert!(plugin.accepts(first));
        assert!(!plugin.accepts(None));
        assert!(!plugin.accepts(Some(("/fixture/Other.dylib", "initialize_Plugin"))));
        assert!(!plugin.accepts(Some(("/fixture/Plugin.dylib", "initialize_Other"))));
        let legacy = super::GenerationPlugin::default();
        legacy.record_attempt(None);
        assert!(legacy.accepts(None));
        assert!(!legacy.accepts(first));
    }

    #[test]
    fn every_dimension_of_a_session_key_defeats_reuse() {
        assert_eq!(session_key(), session_key());

        let mut project = session_key();
        project.project_root = "/other".to_owned();
        assert_ne!(session_key(), project);

        let mut mode = session_key();
        mode.mode = HostSessionMode::Capability {
            package: "p".to_owned(),
            lib_name: "L".to_owned(),
            manifest_path: None,
        };
        assert_ne!(session_key(), mode);

        let mut imports = session_key();
        imports.imports.push("Lean".to_owned());
        assert_ne!(session_key(), imports);

        let mut profile = session_key();
        profile.import_profile = LeanWorkerSessionImportProfile::Server;
        assert_ne!(session_key(), profile);
    }

    /// Import order is significant — the host's own session-pool key treats it
    /// so — which is why `imports` is a sequence and not a set.
    #[test]
    fn reordering_imports_defeats_reuse() {
        let mut reordered = session_key();
        reordered.imports.reverse();
        assert_ne!(session_key(), reordered);
    }

    /// A session is now reused across requests, so the project and the import
    /// profile have to be in the module-query cache key. Before reuse, the
    /// unconditional clear on every open hid their absence; under reuse the
    /// same import *names* under a different project or profile resolve to
    /// different `.olean` files, and a shared key would serve one file's
    /// answer for the other.
    ///
    /// `mode` is here for the same reason one step further on: the pool can
    /// hold a `ShimsOnly` and a `Capability` session over otherwise identical
    /// inputs at the same time, with no clear between them.
    #[test]
    fn the_module_query_cache_key_separates_projects_modes_and_import_profiles() {
        let options = LeanWorkerElabOptions::default();
        let imports = vec!["Init".to_owned()];
        let capability = HostSessionMode::Capability {
            package: "pkg".to_owned(),
            lib_name: "Lib".to_owned(),
            manifest_path: None,
        };
        let other_lib = HostSessionMode::Capability {
            package: "pkg".to_owned(),
            lib_name: "Other".to_owned(),
            manifest_path: None,
        };
        let key = |root: &str, mode: &HostSessionMode, profile| {
            module_query_cache_key(
                root,
                mode,
                "theorem t : True := trivial",
                &imports,
                profile,
                &options,
                "f.lean",
            )
        };

        let shims = HostSessionMode::ShimsOnly;
        let base = key("/a", &shims, LeanWorkerSessionImportProfile::Private);
        assert_ne!(base, key("/b", &shims, LeanWorkerSessionImportProfile::Private));
        assert_ne!(base, key("/a", &shims, LeanWorkerSessionImportProfile::Server));
        assert_ne!(
            base,
            key("/a", &capability, LeanWorkerSessionImportProfile::Private),
            "shims-only and a capability dylib are different environments"
        );
        assert_ne!(
            key("/a", &capability, LeanWorkerSessionImportProfile::Private),
            key("/a", &other_lib, LeanWorkerSessionImportProfile::Private),
            "the whole mode is keyed, not just which variant it is"
        );
        assert_eq!(base, key("/a", &shims, LeanWorkerSessionImportProfile::Private));
        let configured = options
            .clone()
            .lean_options([("autoImplicit".to_owned(), lean_toolchain::LeanOptionValue::Bool(false))].into());
        assert_ne!(
            base,
            module_query_cache_key(
                "/a",
                &shims,
                "theorem t : True := trivial",
                &imports,
                LeanWorkerSessionImportProfile::Private,
                &configured,
                "f.lean",
            )
        );
    }

    #[test]
    fn rss_guard_zero_never_clears() {
        assert!(!should_clear_for_rss_guard(0, Some(u64::MAX)));
        assert!(!should_clear_for_rss_guard(0, None));
    }

    #[test]
    fn rss_guard_clears_only_at_or_above_an_available_sample() {
        assert!(!should_clear_for_rss_guard(1024, Some(1023)));
        assert!(should_clear_for_rss_guard(1024, Some(1024)));
        assert!(should_clear_for_rss_guard(1024, Some(1025)));
    }

    #[test]
    fn rss_guard_unavailable_sample_does_not_clear() {
        // Sampling shells out to `ps` off Linux; a transient failure must not
        // discard the cache on an otherwise healthy child.
        assert!(!should_clear_for_rss_guard(1024, None));
    }

    fn ok_result(
        status: LeanWorkerDeclarationVerificationStatus,
        axioms_available: bool,
    ) -> LeanWorkerDeclarationVerificationResult {
        let mut facts = LeanWorkerDeclarationVerificationFacts::unavailable();
        facts.axioms_available = axioms_available;
        if axioms_available {
            facts.axioms = vec!["propext".to_owned()];
        }
        LeanWorkerDeclarationVerificationResult::Ok {
            verification_status: status,
            facts: Box::new(facts),
            imports: Vec::new(),
        }
    }

    /// The verdict status of an `Ok` result, or `None` for any other variant —
    /// keeps the assertions free of `panic!`/wildcard matches on the
    /// `non_exhaustive` result enum.
    fn ok_status(result: &LeanWorkerDeclarationVerificationResult) -> Option<LeanWorkerDeclarationVerificationStatus> {
        if let LeanWorkerDeclarationVerificationResult::Ok {
            verification_status, ..
        } = result
        {
            Some(*verification_status)
        } else {
            None
        }
    }

    /// `(axioms_available, axioms_is_empty)` of an `Ok` result's facts.
    fn ok_axiom_state(result: &LeanWorkerDeclarationVerificationResult) -> Option<(bool, bool)> {
        if let LeanWorkerDeclarationVerificationResult::Ok { facts, .. } = result {
            Some((facts.axioms_available, facts.axioms.is_empty()))
        } else {
            None
        }
    }

    #[test]
    fn taint_relabels_non_positive_verdict_at_or_above_ceiling() {
        let tainted = apply_memory_pressure_taint(
            ok_result(LeanWorkerDeclarationVerificationStatus::NotFound, true),
            1000,
            Some(5000),
        );
        assert_eq!(
            ok_status(&tainted),
            Some(LeanWorkerDeclarationVerificationStatus::BudgetExceeded)
        );
        // A degraded verdict marks the axiom set unavailable and drops any stale axioms.
        assert_eq!(ok_axiom_state(&tainted), Some((false, true)));
    }

    #[test]
    fn taint_disabled_when_ceiling_is_zero() {
        let tainted = apply_memory_pressure_taint(
            ok_result(LeanWorkerDeclarationVerificationStatus::NotFound, true),
            0,
            Some(50_000_000),
        );
        assert_eq!(
            ok_status(&tainted),
            Some(LeanWorkerDeclarationVerificationStatus::NotFound)
        );
    }

    #[test]
    fn taint_skipped_below_ceiling() {
        let tainted = apply_memory_pressure_taint(
            ok_result(LeanWorkerDeclarationVerificationStatus::NotFound, true),
            5000,
            Some(1000),
        );
        assert_eq!(
            ok_status(&tainted),
            Some(LeanWorkerDeclarationVerificationStatus::NotFound)
        );
    }

    #[test]
    fn taint_skipped_when_rss_sample_unavailable() {
        let tainted = apply_memory_pressure_taint(
            ok_result(LeanWorkerDeclarationVerificationStatus::NotFound, true),
            1000,
            None,
        );
        assert_eq!(
            ok_status(&tainted),
            Some(LeanWorkerDeclarationVerificationStatus::NotFound)
        );
    }

    #[test]
    fn taint_never_downgrades_accepted() {
        let tainted = apply_memory_pressure_taint(
            ok_result(LeanWorkerDeclarationVerificationStatus::Accepted, true),
            1000,
            Some(5000),
        );
        assert_eq!(
            ok_status(&tainted),
            Some(LeanWorkerDeclarationVerificationStatus::Accepted)
        );
    }

    #[test]
    fn non_positive_set_is_exactly_the_unresolved_verdicts() {
        use LeanWorkerDeclarationVerificationStatus as S;
        for status in [S::NotFound, S::NeedsBuild, S::Rejected, S::Ambiguous] {
            assert!(
                verification_status_non_positive(status),
                "{status:?} should be non-positive"
            );
        }
        for status in [S::Accepted, S::BudgetExceeded, S::Timeout, S::Unsupported] {
            assert!(
                !verification_status_non_positive(status),
                "{status:?} should not be non-positive"
            );
        }
    }
}
