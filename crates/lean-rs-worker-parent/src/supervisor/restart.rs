//! When and how the supervisor replaces its child.
//!
//! [`LeanWorkerRestartPolicy`] sets the planned limits, `GenerationBudget`
//! counts what the current child has used against them, and
//! [`LeanWorkerRestartReason`] records why a replacement happened. The
//! `LeanWorker` methods here check the limits before each request, replace a
//! lost or failed child, and refuse to restart while abnormal restarts fill
//! the restart window.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lean_rs_worker_protocol::types::LeanWorkerImportStats;

use super::config::LeanWorkerConfig;
use super::error::{DISPLAY_DIAGNOSTICS_MAX_BYTES, LeanWorkerError, duration_ms, truncate_for_display};
use super::shutdown::ShutdownIntent;
use super::startup::{SpawnFailure, StartupScope};
use super::{LeanWorker, LeanWorkerExit, LeanWorkerReplacementTiming, LeanWorkerStatus, WorkerSupervisorState};
use crate::session::{LeanWorkerCancellationToken, check_cancelled};

const DEFAULT_RESTART_INTENSITY_LIMIT: u64 = 16;

const DEFAULT_RESTART_INTENSITY_WINDOW: Duration = Duration::from_mins(1);

/// Policy for cycling a worker child before the next request.
///
/// The policy resets retained Lean runtime memory only by restarting the
/// process. It does not change `lean-rs-host`'s in-process memory model:
/// dropping a session in the same process does not return process-global Lean
/// memory to the OS.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanWorkerRestartPolicy {
    max_requests: Option<u64>,
    pub(super) max_imports: Option<u64>,
    pub(super) max_import_residue_bytes: Option<u64>,
    max_rss_kib: Option<u64>,
    idle_restart_after: Option<Duration>,
    pub(super) restart_intensity: RestartIntensityLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RestartIntensityLimit {
    pub(super) max_restarts: u64,
    pub(super) window: Duration,
}

impl Default for RestartIntensityLimit {
    fn default() -> Self {
        Self {
            max_restarts: DEFAULT_RESTART_INTENSITY_LIMIT,
            window: DEFAULT_RESTART_INTENSITY_WINDOW,
        }
    }
}

impl LeanWorkerRestartPolicy {
    /// Disable automatic policy restarts.
    ///
    /// Use only for short-lived tests, benchmarks, or hosts that enforce a
    /// process memory boundary elsewhere. Long-running Lean hosts should use
    /// [`Self::memory_bounded`], because fresh imports retain Lean
    /// process-global state until the child exits.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Restart before fresh-import-like requests or RSS growth can accumulate
    /// without bound in one child process.
    ///
    /// The production default shape is one full-session import per worker
    /// under a measured local RSS cap:
    ///
    /// ```rust
    /// # use lean_rs_worker_parent::LeanWorkerRestartPolicy;
    /// let policy = LeanWorkerRestartPolicy::memory_bounded(1, 1_572_864);
    /// ```
    ///
    /// This is admission and cycling policy, not memory reclamation inside a
    /// running Lean process.
    #[must_use]
    pub fn memory_bounded(max_imports: u64, max_rss_kib: u64) -> Self {
        Self::default().max_imports(max_imports).max_rss_kib(max_rss_kib)
    }

    /// Restart before a request when this many requests have entered the child.
    #[must_use]
    pub fn max_requests(mut self, limit: u64) -> Self {
        self.max_requests = Some(limit.max(1));
        self
    }

    /// Restart before an import-like request when this many imports have run.
    #[must_use]
    pub fn max_imports(mut self, limit: u64) -> Self {
        self.max_imports = Some(limit.max(1));
        self
    }

    /// Restart before an import-like request when completed imports have retained
    /// this many bytes the child cannot reclaim.
    ///
    /// This is the byte-denominated form of [`Self::max_imports`], and the one to
    /// prefer. An import retains its environment for the life of the process —
    /// `freeRegions` is unsound once extensions are loaded — so the only question
    /// a cycling policy can usefully ask is *how much* has been retained, not how
    /// many times. One import ranges from tens of megabytes to several gigabytes
    /// depending on the profile's closure, so a count that is right for one
    /// project is wrong for the next by two orders of magnitude.
    ///
    /// The quantity accumulated is `non_memory_mapped_region_bytes` from the
    /// child's own [`LeanWorkerImportStats`] — the compacted-region bytes Lean had
    /// to materialise privately rather than map from an `.olean` file. Measured
    /// against process `phys_footprint` on a Mathlib-scale project, it predicts
    /// retained bytes with slope 1.09 and R² = 0.96; `compacted_region_bytes` and
    /// the imported-extension-entry count both fall below 0.83. It also reports
    /// near-zero for the first import in a fresh process, which is correct: that
    /// import maps its regions at their preferred addresses and costs almost
    /// nothing, and only later imports in the same process must copy.
    ///
    /// A generation always completes at least one import, however large: the
    /// counter advances only on imports the child confirmed, and the check runs
    /// before a request, so a single import can never restart before itself.
    ///
    /// Pair this with a high [`Self::max_imports`] backstop. Nothing else stands
    /// between an accounting bug here and unbounded growth, and the backstop costs
    /// one integer compare.
    #[must_use]
    pub fn max_import_residue_bytes(mut self, limit: u64) -> Self {
        self.max_import_residue_bytes = Some(limit.max(1));
        self
    }

    /// Restart before a request when measured child RSS is at least this many KiB.
    ///
    /// RSS measurement is best effort. It is implemented for the current
    /// supported Unix development targets; unsupported platforms skip the
    /// check and increment `LeanWorkerStats::rss_samples_unavailable`.
    #[must_use]
    pub fn max_rss_kib(mut self, limit: u64) -> Self {
        self.max_rss_kib = Some(limit.max(1));
        self
    }

    /// Restart before a request when the worker has been idle for this long.
    #[must_use]
    pub fn idle_restart_after(mut self, duration: Duration) -> Self {
        self.idle_restart_after = Some(duration);
        self
    }

    /// Refuse a replacement after this many abnormal restarts in one moving
    /// time window. The default is 16 per minute.
    ///
    /// Only abnormal restarts count (see [`LeanWorkerRestartReason::is_planned`]);
    /// planned restarts are neither counted nor refused. The limit applies
    /// after the old child stops and before a replacement starts. A refused
    /// replacement returns [`LeanWorkerError::RestartLimitExceeded`] and leaves
    /// the worker without a child until the oldest counted restart leaves the
    /// window. Until then requests and [`LeanWorker::cycle`] fail with the same
    /// error; after it, the next request starts a new child.
    #[must_use]
    pub fn max_restarts_per_window(mut self, max_restarts: u64, window: Duration) -> Self {
        self.restart_intensity = RestartIntensityLimit {
            max_restarts: max_restarts.max(1),
            window: window.max(Duration::from_millis(1)),
        };
        self
    }
}

/// What one child generation has accumulated, and the cycling decision that
/// follows from it.
///
/// These counters reset together on every restart, which is the whole reason
/// they are one type: `LeanWorkerStats` is cloned forward onto the replacement
/// child, so anything per-generation kept there would never reset.
///
/// It also owns the *ordering* of the import bounds. Two places ask whether an
/// import bound has been crossed. Session opens add their confirmed private
/// region bytes; header events add only increases in the child's cumulative
/// count. Eviction never subtracts allocations. A failed header marks the
/// generation unknown so the next request replaces it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct GenerationBudget {
    pub(super) requests: u64,
    pub(super) imports: u64,
    pub(super) import_residue_bytes: u64,
    header_import_bytes: u64,
    pub(super) header_imports: u64,
    pub(super) accounting_unknown: bool,
}

impl GenerationBudget {
    pub(super) fn record_request(&mut self, import_like: bool) {
        self.requests = self.requests.saturating_add(1);
        if import_like {
            self.imports = self.imports.saturating_add(1);
        }
    }

    pub(super) fn record_import(&mut self, import_stats: &LeanWorkerImportStats) {
        self.imports = self.imports.saturating_add(1);
        self.import_residue_bytes = self
            .import_residue_bytes
            .saturating_add(import_stats.non_memory_mapped_region_bytes);
    }

    pub(super) fn record_header_imports(&mut self, bytes: u64, imports: u64, unknown: bool) {
        self.import_residue_bytes = self
            .import_residue_bytes
            .saturating_add(bytes.saturating_sub(self.header_import_bytes));
        self.header_import_bytes = self.header_import_bytes.max(bytes);
        self.header_imports = self.header_imports.max(imports);
        self.accounting_unknown |= unknown;
    }

    /// Why this generation must cycle before it performs another import, if it must.
    ///
    /// Residue first: when both bounds are configured the byte bound is the one
    /// meant to fire, and reporting a cycle as `max_imports` when bytes were what
    /// ran out sends an operator to the wrong knob.
    ///
    /// A generation always completes at least one import however large, because
    /// the counters only advance on completed imports and this is asked before a
    /// request. That is deliberate: the alternative is a child that can never
    /// serve a profile whose closure exceeds the whole budget.
    pub(super) fn import_limit_reached(&self, policy: &LeanWorkerRestartPolicy) -> Option<LeanWorkerRestartReason> {
        if let Some(limit_bytes) = policy.max_import_residue_bytes
            && self.import_residue_bytes >= limit_bytes
        {
            return Some(LeanWorkerRestartReason::ImportResidue {
                residue_bytes: self.import_residue_bytes,
                limit_bytes,
            });
        }
        if let Some(limit) = policy.max_imports
            && self.imports >= limit
        {
            return Some(LeanWorkerRestartReason::MaxImports { limit });
        }
        None
    }

    pub(super) fn request_limit_reached(&self, policy: &LeanWorkerRestartPolicy) -> Option<LeanWorkerRestartReason> {
        policy
            .max_requests
            .filter(|limit| self.requests >= *limit)
            .map(|limit| LeanWorkerRestartReason::MaxRequests { limit })
    }
}

/// Why the supervisor replaced the worker child.
///
/// Each reason is planned or abnormal, and [`Self::is_planned`] says which.
/// A planned restart carries out the restart policy or a caller's request on a
/// working child before the next request; the restart window neither counts
/// nor refuses it. An abnormal restart replaces a child that failed: it timed
/// out, was cancelled or crossed the hard RSS limit during a request, exited,
/// lost its session, broke the protocol, or never started. Each abnormal
/// restart counts against [`LeanWorkerRestartPolicy::max_restarts_per_window`].
///
/// Non-exhaustive, so a caller that matches the reasons it cares about does not
/// break when one is added. [`Self::stable_cause`] gives a flat name and
/// `Display` a one-line description.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeanWorkerRestartReason {
    /// The caller explicitly requested a process cycle.
    Explicit,
    /// Request count reached the configured limit before the next request.
    MaxRequests { limit: u64 },
    /// Import-like request count reached the configured limit before the next import.
    MaxImports { limit: u64 },
    /// Completed imports retained at least `limit_bytes` the child cannot reclaim.
    ImportResidue { residue_bytes: u64, limit_bytes: u64 },
    /// A failed header import may have left allocations that cannot be measured.
    ImportAccountingUnknown,
    /// Child resident set size reached the configured limit.
    RssCeiling {
        current_kib: u64,
        limit_kib: u64,
        last_import_stats: Option<LeanWorkerImportStats>,
    },
    /// Child resident set size crossed the hard in-flight kill limit.
    RssHardLimit {
        operation: &'static str,
        current_kib: u64,
        limit_kib: u64,
        last_import_stats: Option<LeanWorkerImportStats>,
    },
    /// Worker was idle at least as long as the configured limit.
    Idle { idle_for: Duration, limit: Duration },
    /// Parent-side cancellation replaced the child during an in-flight request.
    Cancelled { operation: &'static str },
    /// Parent-side request timeout replaced the child during an in-flight request.
    RequestTimeout {
        operation: &'static str,
        duration: Duration,
    },
    /// The child exited with a failure status or a signal, and the supervisor
    /// replaced it. `operation` names the request that ran on the child or
    /// found it dead.
    ///
    /// `status` and `diagnostics` are carried because they are otherwise
    /// **unrecoverable**: the child's bounded stderr tail is captured at exit by
    /// the same code path that builds this reason, and nothing else retains it.
    /// A consumer that only sees `stable_cause() == "child_abort"` cannot
    /// distinguish a Lean out-of-memory panic from a segfault from a protocol
    /// bug — which is the difference between raising a limit and filing a bug.
    ChildAbort {
        operation: &'static str,
        /// Platform-rendered exit status, e.g. `signal: 6 (SIGABRT)`.
        status: String,
        /// Child stderr at the abort, trimmed and truncated for display.
        diagnostics: String,
    },
    /// The child exited with a success status that nobody asked for, and the
    /// supervisor replaced it. `operation` names the request that ran on the
    /// child or found it dead.
    ChildExit {
        operation: &'static str,
        /// Platform-rendered exit status.
        status: String,
        /// Child stderr at the exit, trimmed and truncated for display.
        diagnostics: String,
    },
    /// The child answered a session request without holding a session. The
    /// supervisor replaced it, opened the session again and resent the request.
    SessionMissing { operation: &'static str },
    /// The request broke the worker protocol or a parent-side sink panicked, so
    /// the supervisor killed and replaced the child.
    RequestFailed {
        operation: &'static str,
        /// The request's error, truncated for display.
        error: String,
    },
    /// An earlier replacement failed to start, and this restart tries again.
    StartFailed {
        /// The start error, truncated for display.
        error: String,
    },
}

impl LeanWorkerRestartReason {
    /// Whether the restart policy or a caller asked for this restart.
    ///
    /// Planned restarts replace a working child between requests and never
    /// count against the restart window. Every other reason replaces a child
    /// that failed, and counts.
    #[must_use]
    pub const fn is_planned(&self) -> bool {
        matches!(
            self,
            Self::Explicit
                | Self::MaxRequests { .. }
                | Self::MaxImports { .. }
                | Self::ImportResidue { .. }
                | Self::ImportAccountingUnknown
                | Self::RssCeiling { .. }
                | Self::Idle { .. }
        )
    }

    /// Stable wire/policy cause name for this restart reason.
    ///
    /// This is intentionally smaller than the full enum payload: callers can
    /// branch on the cause while still using the typed enum when they need
    /// details such as limits or durations.
    #[must_use]
    pub const fn stable_cause(&self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::MaxRequests { .. } => "max_requests",
            Self::MaxImports { .. } => "max_imports",
            Self::ImportResidue { .. } => "import_residue",
            Self::ImportAccountingUnknown => "import_accounting_unknown",
            Self::RssCeiling { .. } => "rss_ceiling",
            Self::RssHardLimit { .. } => "rss_hard_limit",
            Self::Idle { .. } => "idle",
            Self::Cancelled { .. } => "cancelled",
            Self::RequestTimeout { .. } => "timeout",
            Self::ChildAbort { .. } => "child_abort",
            Self::ChildExit { .. } => "child_exit",
            Self::SessionMissing { .. } => "session_missing",
            Self::RequestFailed { .. } => "request_failed",
            Self::StartFailed { .. } => "start_failed",
        }
    }
}

impl fmt::Display for LeanWorkerRestartReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MIB: u64 = 1024 * 1024;
        match self {
            Self::Explicit => f.write_str("explicit"),
            Self::MaxRequests { limit } => write!(f, "max_requests limit={limit}"),
            Self::MaxImports { limit } => write!(f, "max_imports limit={limit}"),
            Self::ImportResidue {
                residue_bytes,
                limit_bytes,
            } => write!(
                f,
                "import_residue residue_mib={} limit_mib={}",
                residue_bytes / MIB,
                limit_bytes / MIB
            ),
            Self::ImportAccountingUnknown => {
                f.write_str("import_accounting_unknown: retained memory could not be measured after an import error")
            }
            Self::RssCeiling {
                current_kib, limit_kib, ..
            } => write!(f, "rss_ceiling current_kib={current_kib} limit_kib={limit_kib}"),
            Self::RssHardLimit {
                operation,
                current_kib,
                limit_kib,
                ..
            } => write!(
                f,
                "rss_hard_limit operation={operation} current_kib={current_kib} limit_kib={limit_kib}"
            ),
            Self::Idle { idle_for, limit } => write!(
                f,
                "idle idle_for_millis={} limit_millis={}",
                duration_ms(*idle_for),
                duration_ms(*limit)
            ),
            Self::Cancelled { operation } => write!(f, "cancelled operation={operation}"),
            Self::RequestTimeout { operation, duration } => write!(
                f,
                "timeout operation={operation} duration_millis={}",
                duration_ms(*duration)
            ),
            Self::ChildAbort {
                operation,
                status,
                diagnostics,
            }
            | Self::ChildExit {
                operation,
                status,
                diagnostics,
            } => {
                write!(f, "{} operation={operation} status={status}", self.stable_cause())?;
                let detail = diagnostics.trim();
                if detail.is_empty() {
                    Ok(())
                } else {
                    write!(f, ": {detail}")
                }
            }
            Self::SessionMissing { operation } => write!(f, "session_missing operation={operation}"),
            Self::RequestFailed { operation, error } => write!(f, "request_failed operation={operation}: {error}"),
            Self::StartFailed { error } => write!(f, "start_failed: {error}"),
        }
    }
}

impl LeanWorker {
    /// How long abnormal restarts keep the worker down, or `None` when a
    /// restart is allowed at `now`.
    pub(super) fn restart_retry_after(&self, now: Instant) -> Option<Duration> {
        let WorkerSupervisorState::RestartExhausted { until, .. } = self.state else {
            return None;
        };
        until.map_or(Some(Duration::MAX), |until| {
            Some(until.saturating_duration_since(now)).filter(|wait| !wait.is_zero())
        })
    }

    /// Explicitly cycle the worker process.
    ///
    /// This is the manual memory-reset operation. It stops the current child,
    /// starts a replacement with the original configuration, and records
    /// `LeanWorkerRestartReason::Explicit`. After [`Self::try_shutdown`] it
    /// starts the worker again. If the child had already failed, the
    /// replacement records why and counts against the restart window, as the
    /// next request's replacement would.
    ///
    /// # Errors
    ///
    /// Returns [`LeanWorkerError::RestartLimitExceeded`] while abnormal
    /// restarts keep the worker down, or another `LeanWorkerError` if the
    /// existing child cannot be stopped or the replacement cannot be spawned
    /// and handshaken.
    pub fn cycle(&mut self) -> Result<(), LeanWorkerError> {
        self.refuse_while_exhausted()?;
        let reason = if std::mem::take(&mut self.stopped) {
            LeanWorkerRestartReason::Explicit
        } else {
            self.lost_child_reason("cycle")?
                .unwrap_or(LeanWorkerRestartReason::Explicit)
        };
        self.restart_with_reason(reason)
    }

    pub(super) fn prepare_request(
        &mut self,
        import_like: bool,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        check_cancelled(operation, cancellation)?;
        self.ensure_child(operation, cancellation)?;
        if self.budget.accounting_unknown {
            self.restart_for_request(
                LeanWorkerRestartReason::ImportAccountingUnknown,
                operation,
                cancellation,
            )?;
        }
        if import_like {
            self.stats.import_like_admission_attempts = self.stats.import_like_admission_attempts.saturating_add(1);
            self.stats.last_import_like_rss_before_admission_kib = self.child_rss_kib();
        }

        if let Some(reason) = self.budget.request_limit_reached(&self.config.restart_policy) {
            self.restart_for_request(reason, operation, cancellation)?;
            if import_like {
                self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
            }
            return Ok(());
        }

        if import_like && let Some(reason) = self.budget.import_limit_reached(&self.config.restart_policy) {
            self.restart_for_request(reason, operation, cancellation)?;
            self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
            return Ok(());
        }

        if let Some(limit_kib) = self.config.restart_policy.max_rss_kib {
            match self.child_rss_kib() {
                Some(current_kib) if current_kib >= limit_kib => {
                    self.stats.last_rss_kib = Some(current_kib);
                    self.restart_for_request(
                        LeanWorkerRestartReason::RssCeiling {
                            current_kib,
                            limit_kib,
                            last_import_stats: self.stats.last_import_stats.clone(),
                        },
                        operation,
                        cancellation,
                    )?;
                    if import_like {
                        self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
                    }
                    return Ok(());
                }
                Some(current_kib) => {
                    self.stats.last_rss_kib = Some(current_kib);
                }
                None => {
                    self.stats.rss_samples_unavailable = self.stats.rss_samples_unavailable.saturating_add(1);
                }
            }
        }

        if let Some(limit) = self.config.restart_policy.idle_restart_after {
            let idle_for = self.last_activity.elapsed();
            if idle_for >= limit {
                self.restart_for_request(
                    LeanWorkerRestartReason::Idle { idle_for, limit },
                    operation,
                    cancellation,
                )?;
                if import_like {
                    self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
                }
                return Ok(());
            }
        }

        if import_like {
            self.stats.import_like_admitted = self.stats.import_like_admitted.saturating_add(1);
        }
        Ok(())
    }

    pub(super) fn record_request(&mut self, import_like: bool) {
        self.stats.requests = self.stats.requests.saturating_add(1);
        if import_like {
            self.stats.imports = self.stats.imports.saturating_add(1);
        }
        self.budget.record_request(import_like);
        self.last_activity = Instant::now();
    }

    /// Count an import that the child confirmed it performed, and charge its
    /// unreclaimable bytes to the current generation.
    ///
    /// Separate from [`Self::record_request`] because a session open is a
    /// request whether or not it imports: only the child's answer says which
    /// happened, and it arrives after the request is already recorded. That
    /// answer carries the import's own attribution, which is the only reason
    /// this can be denominated in bytes at all — no `ps` fork, no sampling, and
    /// nothing that counts the clean `.olean` pages an RSS reading is dominated
    /// by. A reused session reports the same stats but performed no import, so
    /// it charges nothing; the caller only reaches here on `HostSessionOpened`.
    pub(super) fn record_import(&mut self, import_stats: &LeanWorkerImportStats) {
        self.stats.imports = self.stats.imports.saturating_add(1);
        // Modules came in but no region bytes were attributed to them: the shim
        // is not reporting what this policy is denominated in. Count it rather
        // than guessing a figure, and let `max_imports` be the live bound.
        if import_stats.effective_module_count > 0 && import_stats.compacted_region_bytes == 0 {
            self.stats.import_stats_unusable = self.stats.import_stats_unusable.saturating_add(1);
        }
        self.budget.record_import(import_stats);
    }

    pub(super) fn restart_for_request(
        &mut self,
        reason: LeanWorkerRestartReason,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        let scope = cancellation
            .filter(|token| token.deadline().is_some())
            .map(|token| (operation, token));
        self.restart_with_reason_before_spawn_scoped_using(reason, || {}, scope, Self::spawn_tracked_scoped)
    }

    pub(super) fn restart_with_reason(&mut self, reason: LeanWorkerRestartReason) -> Result<(), LeanWorkerError> {
        self.restart_with_reason_before_spawn(reason, || {})
    }

    pub(super) fn restart_with_reason_before_spawn(
        &mut self,
        reason: LeanWorkerRestartReason,
        before_spawn: impl FnOnce(),
    ) -> Result<(), LeanWorkerError> {
        self.restart_with_reason_before_spawn_scoped_using(reason, before_spawn, None, Self::spawn_tracked_scoped)
    }

    #[cfg(test)]
    pub(super) fn restart_with_reason_before_spawn_using(
        &mut self,
        reason: LeanWorkerRestartReason,
        before_spawn: impl FnOnce(),
        spawn: impl FnOnce(&LeanWorkerConfig) -> Result<Self, SpawnFailure>,
    ) -> Result<(), LeanWorkerError> {
        self.restart_with_reason_before_spawn_scoped_using(reason, before_spawn, None, |config, _| spawn(config))
    }

    pub(super) fn restart_with_reason_before_spawn_scoped_using(
        &mut self,
        reason: LeanWorkerRestartReason,
        before_spawn: impl FnOnce(),
        scope: Option<StartupScope<'_>>,
        spawn: impl FnOnce(&LeanWorkerConfig, Option<StartupScope<'_>>) -> Result<Self, SpawnFailure>,
    ) -> Result<(), LeanWorkerError> {
        let config = self.config.clone();
        let replacement_started = Instant::now();
        self.stats.replacement_attempts = self.stats.replacement_attempts.saturating_add(1);
        let stop_intent = if reason.is_planned() {
            ShutdownIntent::Graceful
        } else {
            ShutdownIntent::KillOnly
        };
        // Each refusal below keeps the reason, so the request or cycle that
        // finally replaces the child records why the old one went.
        if let Err(err) = self.shutdown_child(stop_intent) {
            self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
            self.stats.last_replacement_skipped_reason = Some("stop_failed".to_owned());
            self.recovery = Some(reason);
            return Err(err);
        }
        before_spawn();
        // Cleanup belongs to the retired generation even after the request
        // expires. A successor is new work and cannot spend that budget again.
        if let Some((operation, token)) = scope
            && let Err(error) = check_cancelled(operation, Some(token))
        {
            self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
            self.stats.last_replacement_skipped_reason = Some("request_scope_expired".to_owned());
            self.recovery = Some(reason);
            return Err(error);
        }
        if let Err(err) = self.admit_restart(&reason, replacement_started) {
            self.stats.replacement_budget_skipped = self.stats.replacement_budget_skipped.saturating_add(1);
            self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
            self.stats.last_replacement_skipped_reason = Some("restart_limit_exceeded".to_owned());
            self.recovery = Some(reason);
            return Err(err);
        }
        self.stats.replacement_budget_admitted = self.stats.replacement_budget_admitted.saturating_add(1);
        let next_generation = self.generation.next().ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker generation identifier space exhausted before replacement".to_owned(),
        })?;
        self.stats.record_restart(reason);
        self.budget = GenerationBudget::default();
        // The generation moves with every recorded restart, started or not, so
        // it always equals `stats.restarts`.
        self.generation = next_generation;
        let reason = self
            .stats
            .last_restart_reason
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), |reason| reason.stable_cause().to_owned());
        let mut next = match spawn(&config, scope) {
            Ok(next) => next,
            Err(failure) => {
                self.stats.replacement_failures = self.stats.replacement_failures.saturating_add(1);
                self.stats.last_replacement_skipped_reason = Some("spawn_failed".to_owned());
                self.pending_startup = failure.pending;
                if let Some(exit) = failure.exit {
                    self.last_exit = Some(*exit);
                    self.stats.exits = self.stats.exits.saturating_add(1);
                }
                self.recovery = Some(LeanWorkerRestartReason::StartFailed {
                    error: display_error(&failure.error),
                });
                return Err(failure.error);
            }
        };
        let next_request_id = self.next_request_id;
        let spawn_handshake = next.stats.last_spawn_handshake_elapsed.unwrap_or_default();
        let mut stats = self.stats.clone();
        stats.replacement_successes = stats.replacement_successes.saturating_add(1);
        stats.last_replacement_skipped_reason = None;
        stats.last_spawn_handshake_elapsed = Some(spawn_handshake);
        stats.last_replacement_timing = Some(LeanWorkerReplacementTiming {
            spawn_handshake,
            session_open_import: Duration::ZERO,
            replacement_total: replacement_started.elapsed(),
            replacement_reason: reason,
            replacement_budget_status: "synchronous-no-overlap".to_owned(),
        });
        next.stats = stats;
        next.request_scope = Arc::clone(&self.request_scope);
        next.last_request.clone_from(&self.last_request);
        next.last_request_stop.clone_from(&self.last_request_stop);
        next.last_request_exit.clone_from(&self.last_request_exit);
        next.generation = next_generation;
        next.next_request_id = next_request_id;
        next.state = WorkerSupervisorState::Idle {
            generation: next_generation,
        };
        next.restart_window.clone_from(&self.restart_window);
        next.last_activity = Instant::now();
        *self = next;
        Ok(())
    }

    /// Count an abnormal restart against the window, or refuse it when the
    /// window is full. Planned restarts pass without being counted.
    fn admit_restart(&mut self, reason: &LeanWorkerRestartReason, now: Instant) -> Result<(), LeanWorkerError> {
        if reason.is_planned() {
            return Ok(());
        }
        let limit = self.config.restart_policy.restart_intensity;
        while self
            .restart_window
            .front()
            .is_some_and(|instant| now.saturating_duration_since(*instant) >= limit.window)
        {
            let _ = self.restart_window.pop_front();
        }
        let restarts = u64::try_from(self.restart_window.len()).unwrap_or(u64::MAX);
        if restarts >= limit.max_restarts {
            let until = self
                .restart_window
                .front()
                .and_then(|oldest| oldest.checked_add(limit.window));
            self.state = WorkerSupervisorState::RestartExhausted {
                generation: self.generation,
                until,
            };
            return Err(LeanWorkerError::RestartLimitExceeded {
                restarts,
                window: limit.window,
                retry_after: until.map_or(Duration::MAX, |until| until.saturating_duration_since(now)),
            });
        }
        self.restart_window.push_back(now);
        Ok(())
    }

    /// Refuse to replace the child while abnormal restarts keep the worker down.
    fn refuse_while_exhausted(&self) -> Result<(), LeanWorkerError> {
        self.restart_retry_after(Instant::now()).map_or(Ok(()), |retry_after| {
            Err(LeanWorkerError::RestartLimitExceeded {
                restarts: u64::try_from(self.restart_window.len()).unwrap_or(u64::MAX),
                window: self.config.restart_policy.restart_intensity.window,
                retry_after,
            })
        })
    }

    /// Why the child must be replaced before it can take a request, or `None`
    /// while it runs.
    ///
    /// The reason is the one the request that lost the child left behind, or
    /// else the child's own exit. `operation` names the request that found it.
    fn lost_child_reason(
        &mut self,
        operation: &'static str,
    ) -> Result<Option<LeanWorkerRestartReason>, LeanWorkerError> {
        if self.pending_startup.is_some() {
            let reason = self
                .recovery
                .take()
                .unwrap_or_else(|| LeanWorkerRestartReason::StartFailed {
                    error: "replacement child exit remains unconfirmed".to_owned(),
                });
            return Ok(Some(reason));
        }
        Ok(match self.status()? {
            LeanWorkerStatus::Running => None,
            LeanWorkerStatus::Exited(exit) => {
                Some(self.recovery.take().unwrap_or_else(|| exit_reason(operation, &exit)))
            }
        })
    }

    /// Make sure a child can take the next request, replacing a lost one.
    ///
    /// The replacement records why the old child went and, being abnormal,
    /// counts against the restart window. With a deadline on `cancellation`,
    /// it must also start inside that deadline.
    pub(super) fn ensure_child(
        &mut self,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
    ) -> Result<(), LeanWorkerError> {
        if self.stopped {
            return Err(LeanWorkerError::ShutdownInProgress { operation });
        }
        self.refuse_while_exhausted()?;
        if self.state.rejects_new_requests() {
            return Err(LeanWorkerError::ShutdownInProgress {
                operation: self.state.current_operation().unwrap_or("worker_request"),
            });
        }
        match self.lost_child_reason(operation)? {
            Some(reason) => self.restart_for_request(reason, operation, cancellation),
            None if self.cleanup_pending() => Err(LeanWorkerError::ShutdownInProgress {
                operation: self.state.current_operation().unwrap_or("worker_request"),
            }),
            None => Ok(()),
        }
    }

    /// Replace a child that a request lost, and return the error that request
    /// reports: its own if the replacement starts, else the replacement's.
    fn replace_lost_child(&mut self, reason: LeanWorkerRestartReason, error: LeanWorkerError) -> LeanWorkerError {
        match self.restart_with_reason(reason) {
            Ok(()) => error,
            Err(replacement) => replacement,
        }
    }

    /// Replace a child that exited under a request, then return the error
    /// that reports the exit. Errors that are not a child exit pass through.
    pub(super) fn replace_exited_child(&mut self, operation: &'static str, error: LeanWorkerError) -> LeanWorkerError {
        if let LeanWorkerError::ChildExited { exit } | LeanWorkerError::ChildPanicOrAbort { exit } = &error {
            let reason = exit_reason(operation, exit);
            self.replace_lost_child(reason, error)
        } else {
            error
        }
    }

    /// Kill and replace the child after a request failed in a way that leaves
    /// its protocol state unknown, then return the request's error.
    pub(super) fn replace_failed_child(&mut self, operation: &'static str, error: LeanWorkerError) -> LeanWorkerError {
        let reason = LeanWorkerRestartReason::RequestFailed {
            operation,
            error: display_error(&error),
        };
        self.replace_lost_child(reason, error)
    }
}

/// Carry the one record of *why* a child died into the restart reason.
///
/// The child's stderr is drained continuously into a bounded tail and captured
/// when the supervisor reaps it. A consumer that sees only `stable_cause()` cannot tell
/// a Lean out-of-memory abort from a segfault from a protocol bug — which is the
/// difference between raising a limit and filing a bug. Truncated on the same
/// budget as the `Display` rendering, since the reason is cloned onto every
/// lifetime snapshot that follows it.
pub(super) fn exit_reason(operation: &'static str, exit: &LeanWorkerExit) -> LeanWorkerRestartReason {
    let status = exit.status.clone();
    let diagnostics = truncate_for_display(exit.diagnostics.trim(), DISPLAY_DIAGNOSTICS_MAX_BYTES);
    if exit.success {
        LeanWorkerRestartReason::ChildExit {
            operation,
            status,
            diagnostics,
        }
    } else {
        LeanWorkerRestartReason::ChildAbort {
            operation,
            status,
            diagnostics,
        }
    }
}

/// An error's display text, cut to the length a restart reason carries.
fn display_error(error: &LeanWorkerError) -> String {
    truncate_for_display(&error.to_string(), DISPLAY_DIAGNOSTICS_MAX_BYTES)
}
