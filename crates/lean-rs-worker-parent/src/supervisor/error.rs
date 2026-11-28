//! [`LeanWorkerError`], the resource facts some errors carry, and the bounded
//! text the supervisor displays for child diagnostics.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use lean_rs_worker_protocol::types::{LeanWorkerImportStats, LeanWorkerResourceExhaustedFacts};

use super::restart::LeanWorkerRestartReason;
use super::{LeanWorkerExit, LeanWorkerStats};
use crate::capability::LeanWorkerBootstrapDiagnosticCode;

/// Errors reported by the worker supervisor.
#[derive(Debug)]
pub enum LeanWorkerError {
    /// The worker child could not be spawned.
    Spawn {
        executable: PathBuf,
        source: std::io::Error,
    },
    /// The default worker child executable could not be resolved.
    WorkerChildUnresolved {
        /// Candidate paths checked by the default resolver.
        tried: Vec<PathBuf>,
    },
    /// The resolved worker child is missing or is not executable.
    WorkerChildNotExecutable { path: PathBuf, reason: String },
    /// Worker bootstrap preflight failed before a real command ran.
    Bootstrap {
        code: LeanWorkerBootstrapDiagnosticCode,
        message: String,
    },
    /// The capability Lake target could not be built.
    CapabilityBuild {
        /// Typed Lake/toolchain diagnostic from `lean-toolchain`.
        diagnostic: lean_toolchain::LinkDiagnostics,
    },
    /// The child process could not be prepared after spawning.
    Setup { message: String },
    /// The child did not complete the startup handshake.
    Handshake { message: String },
    /// The worker protocol failed after the handshake.
    Protocol { message: String },
    /// The child returned a typed worker error.
    Worker { code: String, message: String },
    /// The child exited while a request was in flight. The supervisor has
    /// already replaced it, so the next request runs on a new child.
    ChildExited { exit: LeanWorkerExit },
    /// The child exited fatally while a request was in flight, or before its
    /// handshake. After a request, the supervisor has already replaced it.
    ChildPanicOrAbort { exit: LeanWorkerExit },
    /// A worker operation timed out.
    Timeout {
        operation: &'static str,
        duration: Duration,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// The worker was killed and replaced because an in-flight RSS sample
    /// crossed the hard parent-side limit.
    RssHardLimitExceeded {
        operation: &'static str,
        current_kib: u64,
        limit_kib: u64,
        last_import_stats: Option<Box<LeanWorkerImportStats>>,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// A parent-side cancellation token was observed.
    Cancelled {
        operation: &'static str,
        resource: Box<LeanWorkerResourceExhaustedFacts>,
    },
    /// A parent-side progress sink panicked while handling a worker event.
    ProgressPanic { message: String },
    /// A parent-side data sink panicked while handling a worker row.
    DataSinkPanic { message: String },
    /// A capability metadata export returned malformed JSON.
    CapabilityMetadataMalformed { message: String },
    /// A capability doctor export returned malformed JSON.
    CapabilityDoctorMalformed { message: String },
    /// Abnormal restarts used up the restart window, so the supervisor refused
    /// to replace the child. `restarts` counts the abnormal restarts inside
    /// `window`. The worker stays down for `retry_after`; after that, the next
    /// request starts a new child.
    RestartLimitExceeded {
        restarts: u64,
        window: Duration,
        retry_after: Duration,
    },
    /// The public supervisor does not support the requested operation.
    UnsupportedRequest { operation: &'static str },
    /// Waiting for a child process failed.
    Wait { source: std::io::Error },
    /// Killing a child process failed.
    Kill { source: std::io::Error },
    /// Waiting for a child process exceeded the configured bounded wait.
    WaitTimeout {
        operation: &'static str,
        duration: Duration,
    },
    /// The worker is stopping its child or was shut down, so it refuses new
    /// requests. After [`LeanWorker::try_shutdown`], [`LeanWorker::cycle`]
    /// starts a new child.
    ///
    /// [`LeanWorker::try_shutdown`]: crate::LeanWorker::try_shutdown
    /// [`LeanWorker::cycle`]: crate::LeanWorker::cycle
    ShutdownInProgress { operation: &'static str },
}

impl fmt::Display for LeanWorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { executable, source } => {
                write!(f, "failed to spawn worker {}: {source}", executable.display())
            }
            Self::WorkerChildUnresolved { tried } => {
                let tried = tried
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "could not resolve lean-rs-worker-child; set LEAN_RS_WORKER_CHILD or place it beside the current \
                     executable (tried: {tried})"
                )
            }
            Self::WorkerChildNotExecutable { path, reason } => {
                write!(f, "worker child '{}' is not executable: {reason}", path.display())
            }
            Self::Bootstrap { code, message } => {
                write!(f, "worker bootstrap check {code} failed: {message}")
            }
            Self::CapabilityBuild { diagnostic } => {
                write!(f, "worker capability Lake target build failed: {diagnostic}")
            }
            Self::Setup { message } => write!(f, "worker child setup failed: {message}"),
            Self::Handshake { message } => write!(f, "worker handshake failed: {message}"),
            Self::Protocol { message } => write!(f, "worker protocol failed: {message}"),
            Self::Worker { code, message } => write!(f, "worker returned {code}: {message}"),
            Self::ChildExited { exit } => write_exit(f, "worker exited", exit),
            Self::ChildPanicOrAbort { exit } => write_exit(f, "worker exited fatally", exit),
            Self::Timeout {
                operation, duration, ..
            } => {
                write!(f, "worker operation {operation} timed out after {duration:?}")
            }
            Self::RssHardLimitExceeded {
                operation,
                current_kib,
                limit_kib,
                last_import_stats,
                ..
            } => {
                write!(
                    f,
                    "worker operation {operation} exceeded hard RSS limit; current_kib={current_kib} \
                     limit_kib={limit_kib}; {}",
                    import_stats_diagnostic(last_import_stats.as_deref())
                )
            }
            Self::Cancelled { operation, .. } => write!(f, "worker operation {operation} was cancelled"),
            Self::ProgressPanic { message } => write!(f, "worker progress sink panicked: {message}"),
            Self::DataSinkPanic { message } => write!(f, "worker data sink panicked: {message}"),
            Self::CapabilityMetadataMalformed { message } => {
                write!(f, "capability metadata export returned malformed JSON: {message}")
            }
            Self::CapabilityDoctorMalformed { message } => {
                write!(f, "capability doctor export returned malformed JSON: {message}")
            }
            Self::RestartLimitExceeded {
                restarts,
                window,
                retry_after,
            } => {
                write!(
                    f,
                    "worker restart limit exceeded after {restarts} abnormal restarts in {window:?}; \
                     next restart allowed in {retry_after:?}"
                )
            }
            Self::UnsupportedRequest { operation } => {
                write!(f, "worker operation {operation} is not supported")
            }
            Self::Wait { source } => write!(f, "failed to wait for worker child: {source}"),
            Self::Kill { source } => write!(f, "failed to kill worker child: {source}"),
            Self::WaitTimeout { operation, duration } => {
                write!(
                    f,
                    "timed out waiting for worker child during {operation} after {duration:?}"
                )
            }
            Self::ShutdownInProgress { operation } => {
                write!(
                    f,
                    "worker operation {operation} was rejected because shutdown is in progress"
                )
            }
        }
    }
}

impl std::error::Error for LeanWorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Wait { source } | Self::Kill { source } => Some(source),
            Self::CapabilityBuild { diagnostic } => Some(diagnostic),
            Self::WorkerChildUnresolved { .. } | Self::WorkerChildNotExecutable { .. } | Self::Bootstrap { .. } => None,
            Self::Setup { .. }
            | Self::Handshake { .. }
            | Self::Protocol { .. }
            | Self::Worker { .. }
            | Self::ChildExited { .. }
            | Self::ChildPanicOrAbort { .. }
            | Self::Timeout { .. }
            | Self::RssHardLimitExceeded { .. }
            | Self::Cancelled { .. }
            | Self::ProgressPanic { .. }
            | Self::DataSinkPanic { .. }
            | Self::CapabilityMetadataMalformed { .. }
            | Self::CapabilityDoctorMalformed { .. }
            | Self::RestartLimitExceeded { .. }
            | Self::UnsupportedRequest { .. }
            | Self::WaitTimeout { .. }
            | Self::ShutdownInProgress { .. } => None,
        }
    }
}

impl LeanWorkerError {
    /// Structured resource-boundary facts for errors caused by an admission,
    /// timeout, cancellation, or RSS limit. `None` means the error is a
    /// protocol, Lean, build, or lifecycle failure without resource evidence.
    #[must_use]
    pub fn resource_exhausted_facts(&self) -> Option<&LeanWorkerResourceExhaustedFacts> {
        match self {
            Self::Timeout { resource, .. }
            | Self::RssHardLimitExceeded { resource, .. }
            | Self::Cancelled { resource, .. } => Some(resource.as_ref()),
            Self::Spawn { .. }
            | Self::Kill { .. }
            | Self::WorkerChildUnresolved { .. }
            | Self::WorkerChildNotExecutable { .. }
            | Self::Bootstrap { .. }
            | Self::CapabilityBuild { .. }
            | Self::Setup { .. }
            | Self::Handshake { .. }
            | Self::Protocol { .. }
            | Self::Worker { .. }
            | Self::ChildExited { .. }
            | Self::ChildPanicOrAbort { .. }
            | Self::ProgressPanic { .. }
            | Self::DataSinkPanic { .. }
            | Self::CapabilityMetadataMalformed { .. }
            | Self::CapabilityDoctorMalformed { .. }
            | Self::RestartLimitExceeded { .. }
            | Self::UnsupportedRequest { .. }
            | Self::WaitTimeout { .. }
            | Self::ShutdownInProgress { .. }
            | Self::Wait { .. } => None,
        }
    }
}

pub(super) fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[allow(
    clippy::too_many_arguments,
    reason = "flat fact construction mirrors the public diagnostic payload"
)]
pub(super) fn resource_facts(
    cause: impl Into<String>,
    work_entered_child: bool,
    operation: Option<&'static str>,
    current_rss_kib: Option<u64>,
    limit_kib: Option<u64>,
    import_count: Option<u64>,
    worker_generation: Option<u64>,
    restart_reason: Option<String>,
    queue_wait: Option<Duration>,
    duration: Option<Duration>,
    cold_open_attempts: Option<u64>,
    cold_open_admitted: Option<u64>,
    cold_open_refusals: Option<u64>,
    import_like_requests: Option<u64>,
    import_like_admitted: Option<u64>,
    last_import_stats: Option<LeanWorkerImportStats>,
) -> LeanWorkerResourceExhaustedFacts {
    LeanWorkerResourceExhaustedFacts {
        cause: cause.into(),
        work_entered_child,
        operation: operation.map(str::to_owned),
        current_rss_kib,
        limit_kib,
        import_count,
        worker_generation,
        restart_reason,
        queue_wait_ms: queue_wait.map(duration_ms),
        duration_ms: duration.map(duration_ms),
        cold_open_attempts,
        cold_open_admitted,
        cold_open_refusals,
        import_like_requests,
        import_like_admitted,
        last_import_stats,
    }
}

pub(super) fn worker_resource_facts(
    cause: impl Into<String>,
    work_entered_child: bool,
    operation: Option<&'static str>,
    stats: &LeanWorkerStats,
    current_rss_kib: Option<u64>,
    limit_kib: Option<u64>,
    duration: Option<Duration>,
) -> LeanWorkerResourceExhaustedFacts {
    resource_facts(
        cause,
        work_entered_child,
        operation,
        current_rss_kib,
        limit_kib,
        Some(stats.imports),
        Some(stats.restarts),
        stats
            .last_restart_reason
            .as_ref()
            .map(LeanWorkerRestartReason::stable_cause)
            .map(str::to_owned),
        None,
        duration,
        None,
        None,
        None,
        Some(stats.import_like_admission_attempts),
        Some(stats.import_like_admitted),
        stats.last_import_stats.clone(),
    )
}

/// Cap so a single `Display` line cannot blow up downstream log/telemetry
/// pipelines that capture `err.to_string()` (`tracing::error!("{err}")`,
/// JSON log shippers). The full `exit.diagnostics` text is still available on
/// the field; this only limits what crosses the `Display` surface.
pub(super) const DISPLAY_DIAGNOSTICS_MAX_BYTES: usize = 4 * 1024;

fn write_exit(f: &mut fmt::Formatter<'_>, prefix: &str, exit: &LeanWorkerExit) -> fmt::Result {
    let tail = exit.diagnostics.trim();
    if tail.is_empty() {
        write!(f, "{prefix} with {}", exit.status)
    } else {
        let truncated = truncate_for_display(tail, DISPLAY_DIAGNOSTICS_MAX_BYTES);
        write!(f, "{prefix} with {}: {truncated}", exit.status)
    }
}

fn import_stats_diagnostic(stats: Option<&LeanWorkerImportStats>) -> String {
    let Some(stats) = stats else {
        return String::from("last_import_stats=unavailable");
    };
    format!(
        "last_import_stats=available import_profile=level:{} import_all:{} load_exts:{} direct_import_count={} \
         direct_imports={} effective_modules={} compacted_regions={} memory_mapped_regions={} \
         compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} \
         imported_constants={} extension_entries={}",
        stats.import_level,
        stats.import_all,
        stats.load_exts,
        stats.direct_import_names.len(),
        stats.direct_import_names.join(","),
        stats.effective_module_count,
        stats.compacted_region_count,
        stats.memory_mapped_region_count,
        stats.compacted_region_bytes,
        stats.memory_mapped_region_bytes,
        stats.non_memory_mapped_region_bytes,
        stats.imported_constant_count,
        stats.total_imported_extension_entries
    )
}

/// Truncate `text` to at most `max_bytes` bytes for human display, appending
/// `… (N bytes truncated)` when bytes are dropped.
///
/// The cut respects two invariants that matter for the actual log streams we
/// surface (Lake/`elan` stderr): the cut lands on a UTF-8 char boundary, and
/// it never lands inside an unterminated ANSI CSI escape (`ESC '[' …
/// 0x40..=0x7e`). If the naive cut would bisect a CSI, we back up to the byte
/// immediately before the `ESC`. Half-open escapes corrupt downstream terminals
/// and log viewers; the byte cost of backing up is negligible compared to the
/// 4 KiB cap.
pub(super) fn truncate_for_display(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }

    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }

    let bytes = text.as_bytes();
    if let Some(esc_off) = bytes
        .get(..cut)
        .and_then(|prefix| prefix.iter().rposition(|&b| b == 0x1b))
    {
        // CSI is `ESC '[' params terminator-in-0x40..=0x7e`, with `[` excluded
        // from the terminator range. Earlier escape sequences must already be
        // terminated to have reached the parser state for this one.
        let scan_start = esc_off.saturating_add(2).min(cut);
        let terminated = bytes
            .get(scan_start..cut)
            .is_some_and(|tail| tail.iter().any(|&b| matches!(b, 0x40..=0x5a | 0x5c..=0x7e)));
        if !terminated {
            cut = esc_off;
        }
    }

    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }

    let truncated_bytes = text.len().saturating_sub(cut);
    let kept = text.get(..cut).unwrap_or("");
    format!("{kept}… ({truncated_bytes} bytes truncated)")
}
