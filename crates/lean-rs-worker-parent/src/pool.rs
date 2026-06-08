//! Local worker-pool orchestration and session leasing.
//!
//! The pool sits above `LeanWorkerCapabilityBuilder` and typed commands. It
//! chooses a compatible local child process for capability work, while callers
//! only see session requirements and a lease that can run typed commands.

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use lean_rs_worker_protocol::types::{LeanWorkerImportStats, LeanWorkerSessionImportProfile};

use crate::capability::{LeanWorkerCapability, LeanWorkerCapabilityBuilder};
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerDiagnosticSink, LeanWorkerJsonCommand, LeanWorkerProgressSink,
    LeanWorkerRuntimeMetadata, LeanWorkerSession, LeanWorkerStreamingCommand, LeanWorkerTypedDataSink,
    LeanWorkerTypedStreamSummary,
};
use crate::supervisor::{LeanWorkerError, LeanWorkerRestartReason, LeanWorkerStatus};

/// Coarse restart-policy class used in pool session keys.
///
/// The pool key records whether a session was opened under the default policy
/// or a caller-selected policy class. It deliberately does not expose every
/// restart-policy knob as key material; memory-aware scheduling and richer
/// policy admission are not part of the session-key contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LeanWorkerRestartPolicyClass {
    Default,
    Custom,
}

/// Worker reuse key for a capability-backed session.
///
/// A session key answers only one pool question: can an already-open child
/// session safely host compatible work? The key includes both the capability
/// project root and the import workspace root because one open session has one
/// fixed Lean search path. It is not a downstream cache key, and it does not
/// encode row schemas, cache validity, ranking, reporting, or source
/// provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerSessionKey {
    project_root: PathBuf,
    import_workspace_root: PathBuf,
    package: String,
    lib_name: String,
    imports: Vec<String>,
    import_profile: LeanWorkerSessionImportProfile,
    metadata_expectation: Option<LeanWorkerMetadataExpectationKey>,
    toolchain_fingerprint: lean_toolchain::ToolchainFingerprint,
    restart_policy_class: LeanWorkerRestartPolicyClass,
}

impl LeanWorkerSessionKey {
    /// Create a session key from the caller-visible capability requirements.
    ///
    /// `project_root` is the capability project root. The import workspace
    /// root defaults to the same value; capability builders with a distinct
    /// target workspace override it internally.
    #[must_use]
    pub fn new(
        project_root: impl Into<PathBuf>,
        package: impl Into<String>,
        lib_name: impl Into<String>,
        imports: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let project_root = project_root.into();
        Self {
            import_workspace_root: normalize_import_workspace_root(project_root.clone()),
            project_root,
            package: package.into(),
            lib_name: lib_name.into(),
            imports: imports.into_iter().map(Into::into).collect(),
            import_profile: LeanWorkerSessionImportProfile::default(),
            metadata_expectation: None,
            toolchain_fingerprint: lean_toolchain::ToolchainFingerprint::current(),
            restart_policy_class: LeanWorkerRestartPolicyClass::Default,
        }
    }

    pub(crate) fn with_import_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.import_workspace_root = normalize_import_workspace_root(root.into());
        self
    }

    pub(crate) fn with_import_profile(mut self, profile: LeanWorkerSessionImportProfile) -> Self {
        self.import_profile = profile;
        self
    }

    /// Add the metadata expectation used to decide safe session reuse.
    ///
    /// `expected` is downstream metadata transported by the generic metadata
    /// envelope. The pool compares it as opaque facts and does not interpret
    /// command names or semantic versions.
    #[must_use]
    pub fn metadata_expectation(
        mut self,
        export: impl Into<String>,
        request: Value,
        expected: Option<lean_rs_worker_protocol::types::LeanWorkerCapabilityMetadata>,
    ) -> Self {
        self.metadata_expectation = Some(LeanWorkerMetadataExpectationKey {
            export: export.into(),
            request,
            expected,
        });
        self
    }

    /// Set the coarse restart-policy class for this session key.
    #[must_use]
    pub fn restart_policy_class(mut self, class: LeanWorkerRestartPolicyClass) -> Self {
        self.restart_policy_class = class;
        self
    }

    /// Return the capability Lake project root for this session key.
    #[must_use]
    pub fn project_root(&self) -> &std::path::Path {
        &self.project_root
    }

    /// Return the target Lake workspace root whose dependency closure the session imports against.
    #[must_use]
    pub fn import_workspace_root(&self) -> &std::path::Path {
        &self.import_workspace_root
    }

    /// Return the Lake package name for this session key.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Return the Lake library target for this session key.
    #[must_use]
    pub fn lib_name(&self) -> &str {
        &self.lib_name
    }

    /// Return the imports required by this session key.
    #[must_use]
    pub fn imports(&self) -> &[String] {
        &self.imports
    }

    /// Return the import profile required by this session key.
    #[must_use]
    pub fn import_profile(&self) -> LeanWorkerSessionImportProfile {
        self.import_profile
    }

    /// Return the build-baked Lean toolchain fingerprint used by this key.
    #[must_use]
    pub fn toolchain_fingerprint(&self) -> &lean_toolchain::ToolchainFingerprint {
        &self.toolchain_fingerprint
    }

    /// Return the restart-policy class used by this key.
    #[must_use]
    pub fn policy_class(&self) -> LeanWorkerRestartPolicyClass {
        self.restart_policy_class
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LeanWorkerMetadataExpectationKey {
    export: String,
    request: Value,
    expected: Option<lean_rs_worker_protocol::types::LeanWorkerCapabilityMetadata>,
}

/// Configuration for a local `LeanWorkerPool`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerPoolConfig {
    max_workers: usize,
    max_total_child_rss_kib: Option<u64>,
    per_worker_rss_ceiling_kib: Option<u64>,
    idle_cycle_after: Option<Duration>,
    queue_wait_timeout: Duration,
}

impl LeanWorkerPoolConfig {
    /// Create pool configuration with a fixed local worker limit.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers: max_workers.max(1),
            max_total_child_rss_kib: None,
            per_worker_rss_ceiling_kib: None,
            idle_cycle_after: None,
            queue_wait_timeout: Duration::ZERO,
        }
    }

    /// Return the maximum number of local child workers the pool may own.
    #[must_use]
    pub fn max_workers(&self) -> usize {
        self.max_workers
    }

    /// Reject new distinct workers when known total child RSS reaches `limit`.
    ///
    /// RSS sampling is best effort. On platforms where the pool cannot obtain
    /// samples, it records unavailable samples and does not make a false
    /// admission claim.
    #[must_use]
    pub fn max_total_child_rss_kib(mut self, limit: u64) -> Self {
        self.max_total_child_rss_kib = Some(limit.max(1));
        self
    }

    /// Cycle a worker before assigning work when its sampled RSS reaches `limit`.
    #[must_use]
    pub fn per_worker_rss_ceiling_kib(mut self, limit: u64) -> Self {
        self.per_worker_rss_ceiling_kib = Some(limit.max(1));
        self
    }

    /// Cycle an idle worker before assigning more work through an old lease.
    #[must_use]
    pub fn idle_cycle_after(mut self, limit: Duration) -> Self {
        self.idle_cycle_after = Some(limit);
        self
    }

    /// Wait this long for local pool admission before returning a typed error.
    ///
    /// The current pool is synchronous. This timeout documents and bounds the
    /// admission point without exposing worker ids or queue internals.
    #[must_use]
    pub fn queue_wait_timeout(mut self, timeout: Duration) -> Self {
        self.queue_wait_timeout = timeout;
        self
    }

    /// Return the configured total child RSS budget in KiB.
    #[must_use]
    pub fn max_total_child_rss_kib_limit(&self) -> Option<u64> {
        self.max_total_child_rss_kib
    }

    /// Return the configured per-worker RSS ceiling in KiB.
    #[must_use]
    pub fn per_worker_rss_ceiling_kib_limit(&self) -> Option<u64> {
        self.per_worker_rss_ceiling_kib
    }

    /// Return the configured idle-cycle duration.
    #[must_use]
    pub fn idle_cycle_after_limit(&self) -> Option<Duration> {
        self.idle_cycle_after
    }

    /// Return the configured pool admission wait timeout.
    #[must_use]
    pub fn queue_wait_timeout_limit(&self) -> Duration {
        self.queue_wait_timeout
    }
}

impl Default for LeanWorkerPoolConfig {
    fn default() -> Self {
        Self::new(1)
    }
}

/// Summary of public pool state.
///
/// This snapshot exposes admission and reuse facts without revealing worker
/// ids, child pids, pipe handles, or which warm child will be selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerPoolSnapshot {
    pub max_workers: usize,
    pub workers: usize,
    pub active_workers: usize,
    pub warm_leases: usize,
    pub queue_depth: usize,
    pub total_child_rss_kib: Option<u64>,
    pub rss_samples_unavailable: u64,
    pub requests: u64,
    pub imports: u64,
    pub worker_restarts: u64,
    pub max_request_restarts: u64,
    pub max_import_restarts: u64,
    pub rss_restarts: u64,
    pub idle_restarts: u64,
    pub cancelled_restarts: u64,
    pub timeout_restarts: u64,
    pub policy_restarts: u64,
    pub queue_timeouts: u64,
    pub memory_budget_rejections: u64,
    pub last_restart_reason: Option<LeanWorkerRestartReason>,
    pub last_import_stats: Option<LeanWorkerImportStats>,
    pub stream_requests: u64,
    pub stream_successes: u64,
    pub stream_failures: u64,
    pub data_rows_delivered: u64,
    pub data_row_payload_bytes: u64,
    pub stream_elapsed: Duration,
    pub backpressure_waits: u64,
    pub backpressure_failures: u64,
}

/// Local pool for worker-backed capability sessions.
#[derive(Debug)]
pub struct LeanWorkerPool {
    config: LeanWorkerPoolConfig,
    entries: Vec<PoolEntry>,
    queue_timeouts: u64,
    memory_budget_rejections: u64,
}

impl LeanWorkerPool {
    /// Create an empty local worker pool.
    #[must_use]
    pub fn new(config: LeanWorkerPoolConfig) -> Self {
        Self {
            config,
            entries: Vec::new(),
            queue_timeouts: 0,
            memory_budget_rejections: 0,
        }
    }

    /// Acquire a lease for the capability described by `builder`.
    ///
    /// The pool reuses a warm compatible worker session when possible. If a
    /// matching worker has died, the pool replaces it before returning the
    /// lease. If admitting a distinct session key would exceed `max_workers`,
    /// the pool returns `LeanWorkerError::WorkerPoolExhausted`.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` when the capability cannot be built, metadata
    /// validation fails, a dead compatible worker cannot be replaced, or the
    /// fixed local worker limit is already full of distinct session keys.
    pub fn acquire_lease(
        &mut self,
        builder: LeanWorkerCapabilityBuilder,
    ) -> Result<LeanWorkerSessionLease<'_>, LeanWorkerError> {
        let key = builder.session_key();
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            self.ensure_entry_running(index)?;
            self.enforce_entry_policy_before_assignment(index)?;
            let entry = self.entries.get_mut(index).ok_or_else(|| LeanWorkerError::Protocol {
                message: "worker pool entry disappeared during lease acquisition".to_owned(),
            })?;
            entry.active_leases = entry.active_leases.saturating_add(1);
            return Ok(LeanWorkerSessionLease {
                entry,
                config: self.config.clone(),
                valid: true,
                invalidation_reason: None,
                request_timeout_override: None,
            });
        }

        if self.entries.len() >= self.config.max_workers {
            return self.pool_full_error();
        }
        self.ensure_spawn_within_total_rss_budget()?;

        let capability = builder.clone().open()?;
        let base_request_timeout = builder.pool_request_timeout();
        self.entries.push(PoolEntry {
            key,
            builder,
            capability,
            base_request_timeout,
            last_rss_kib: None,
            rss_samples_unavailable: 0,
            last_activity: Instant::now(),
            last_restart_reason: None,
            policy_restarts: 0,
            active_leases: 0,
        });
        let entry = self.entries.last_mut().ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker pool failed to retain newly opened entry".to_owned(),
        })?;
        let _ = entry.sample_rss();
        entry.active_leases = entry.active_leases.saturating_add(1);
        Ok(LeanWorkerSessionLease {
            entry,
            config: self.config.clone(),
            valid: true,
            invalidation_reason: None,
            request_timeout_override: None,
        })
    }

    /// Return a public snapshot of pool state.
    #[must_use]
    pub fn snapshot(&self) -> LeanWorkerPoolSnapshot {
        snapshot_from_entries(
            &self.config,
            &self.entries,
            self.queue_timeouts,
            self.memory_budget_rejections,
        )
    }

    fn snapshot_from_lease_config(config: &LeanWorkerPoolConfig, entry: &PoolEntry) -> LeanWorkerPoolSnapshot {
        snapshot_from_entries(config, std::slice::from_ref(entry), 0, 0)
    }
}

fn snapshot_from_entries(
    config: &LeanWorkerPoolConfig,
    entries: &[PoolEntry],
    queue_timeouts: u64,
    memory_budget_rejections: u64,
) -> LeanWorkerPoolSnapshot {
    LeanWorkerPoolSnapshot {
        max_workers: config.max_workers,
        workers: entries.len(),
        active_workers: entries.iter().filter(|entry| entry.active_leases > 0).count(),
        warm_leases: entries.iter().filter(|entry| entry.active_leases == 0).count(),
        queue_depth: 0,
        total_child_rss_kib: total_known_child_rss_kib(entries),
        rss_samples_unavailable: entries.iter().map(|entry| entry.rss_samples_unavailable).sum(),
        requests: entries.iter().map(|entry| entry.capability.stats().requests).sum(),
        imports: entries.iter().map(|entry| entry.capability.stats().imports).sum(),
        worker_restarts: entries.iter().map(|entry| entry.capability.stats().restarts).sum(),
        max_request_restarts: entries
            .iter()
            .map(|entry| entry.capability.stats().max_request_restarts)
            .sum(),
        max_import_restarts: entries
            .iter()
            .map(|entry| entry.capability.stats().max_import_restarts)
            .sum(),
        rss_restarts: entries.iter().map(|entry| entry.capability.stats().rss_restarts).sum(),
        idle_restarts: entries.iter().map(|entry| entry.capability.stats().idle_restarts).sum(),
        cancelled_restarts: entries
            .iter()
            .map(|entry| entry.capability.stats().cancelled_restarts)
            .sum(),
        timeout_restarts: entries
            .iter()
            .map(|entry| entry.capability.stats().timeout_restarts)
            .sum(),
        policy_restarts: entries.iter().map(|entry| entry.policy_restarts).sum(),
        queue_timeouts,
        memory_budget_rejections,
        last_restart_reason: entries.iter().rev().find_map(|entry| entry.last_restart_reason.clone()),
        last_import_stats: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_import_stats),
        stream_requests: entries
            .iter()
            .map(|entry| entry.capability.stats().stream_requests)
            .sum(),
        stream_successes: entries
            .iter()
            .map(|entry| entry.capability.stats().stream_successes)
            .sum(),
        stream_failures: entries
            .iter()
            .map(|entry| entry.capability.stats().stream_failures)
            .sum(),
        data_rows_delivered: entries
            .iter()
            .map(|entry| entry.capability.stats().data_rows_delivered)
            .sum(),
        data_row_payload_bytes: entries
            .iter()
            .map(|entry| entry.capability.stats().data_row_payload_bytes)
            .sum(),
        stream_elapsed: entries.iter().fold(Duration::ZERO, |acc, entry| {
            acc.saturating_add(entry.capability.stats().stream_elapsed)
        }),
        backpressure_waits: entries
            .iter()
            .map(|entry| entry.capability.stats().backpressure_waits)
            .sum(),
        backpressure_failures: entries
            .iter()
            .map(|entry| entry.capability.stats().backpressure_failures)
            .sum(),
    }
}

fn total_known_child_rss_kib(entries: &[PoolEntry]) -> Option<u64> {
    entries
        .iter()
        .map(|entry| entry.last_rss_kib)
        .try_fold(0_u64, |acc, value| value.map(|rss| acc.saturating_add(rss)))
}

impl LeanWorkerPool {
    fn ensure_entry_running(&mut self, index: usize) -> Result<(), LeanWorkerError> {
        let entry = self.entries.get_mut(index).ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker pool entry disappeared during liveness check".to_owned(),
        })?;
        match entry.capability.status()? {
            LeanWorkerStatus::Running => Ok(()),
            LeanWorkerStatus::Exited(_exit) => {
                entry.capability = entry.builder.clone().open()?;
                entry.last_activity = Instant::now();
                Ok(())
            }
        }
    }

    fn enforce_entry_policy_before_assignment(&mut self, index: usize) -> Result<(), LeanWorkerError> {
        let entry = self.entries.get_mut(index).ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker pool entry disappeared during policy check".to_owned(),
        })?;
        entry.enforce_policy(&self.config).map(|_| ())
    }

    fn ensure_spawn_within_total_rss_budget(&mut self) -> Result<(), LeanWorkerError> {
        let Some(limit_kib) = self.config.max_total_child_rss_kib else {
            return Ok(());
        };
        let rss = self.refresh_total_child_rss();
        if rss.unavailable > 0 {
            return Ok(());
        }
        if rss.total_kib >= limit_kib {
            self.memory_budget_rejections = self.memory_budget_rejections.saturating_add(1);
            return Err(LeanWorkerError::WorkerPoolMemoryBudgetExceeded {
                current_kib: rss.total_kib,
                limit_kib,
                last_import_stats: self.latest_import_stats(),
            });
        }
        Ok(())
    }

    fn latest_import_stats(&self) -> Option<LeanWorkerImportStats> {
        self.entries
            .iter()
            .find_map(|entry| entry.capability.stats().last_import_stats)
    }

    fn refresh_total_child_rss(&mut self) -> PoolRssTotal {
        let mut total_kib = 0_u64;
        let mut unavailable = 0_u64;
        for entry in &mut self.entries {
            match entry.sample_rss() {
                Some(value) => {
                    total_kib = total_kib.saturating_add(value);
                }
                None => {
                    unavailable = unavailable.saturating_add(1);
                }
            }
        }
        PoolRssTotal { total_kib, unavailable }
    }

    fn pool_full_error<T>(&mut self) -> Result<T, LeanWorkerError> {
        if self.config.queue_wait_timeout.is_zero() {
            return Err(LeanWorkerError::WorkerPoolExhausted {
                max_workers: self.config.max_workers,
            });
        }
        let started = Instant::now();
        while started.elapsed() < self.config.queue_wait_timeout {
            let remaining = self.config.queue_wait_timeout.saturating_sub(started.elapsed());
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
        self.queue_timeouts = self.queue_timeouts.saturating_add(1);
        Err(LeanWorkerError::WorkerPoolQueueTimeout {
            waited: self.config.queue_wait_timeout,
        })
    }
}

impl Default for LeanWorkerPool {
    fn default() -> Self {
        Self::new(LeanWorkerPoolConfig::default())
    }
}

#[derive(Debug)]
struct PoolEntry {
    key: LeanWorkerSessionKey,
    builder: LeanWorkerCapabilityBuilder,
    capability: LeanWorkerCapability,
    base_request_timeout: Duration,
    last_rss_kib: Option<u64>,
    rss_samples_unavailable: u64,
    last_activity: Instant,
    last_restart_reason: Option<LeanWorkerRestartReason>,
    policy_restarts: u64,
    active_leases: u64,
}

impl PoolEntry {
    fn sample_rss(&mut self) -> Option<u64> {
        match self.capability.rss_kib() {
            Some(value) => {
                self.last_rss_kib = Some(value);
                Some(value)
            }
            None => {
                self.rss_samples_unavailable = self.rss_samples_unavailable.saturating_add(1);
                None
            }
        }
    }

    fn enforce_policy(&mut self, config: &LeanWorkerPoolConfig) -> Result<Option<String>, LeanWorkerError> {
        if let Some(limit_kib) = config.per_worker_rss_ceiling_kib {
            match self.sample_rss() {
                Some(current_kib) if current_kib >= limit_kib => {
                    let reason = LeanWorkerRestartReason::RssCeiling {
                        current_kib,
                        limit_kib,
                        last_import_stats: self.capability.stats().last_import_stats,
                    };
                    self.cycle_for_policy(reason)?;
                    return Ok(Some(format!(
                        "memory policy cycled worker at {current_kib} KiB RSS with limit {limit_kib} KiB"
                    )));
                }
                Some(_) | None => {}
            }
        }

        if let Some(limit) = config.idle_cycle_after {
            let idle_for = self.last_activity.elapsed();
            if idle_for >= limit {
                let reason = LeanWorkerRestartReason::Idle { idle_for, limit };
                self.cycle_for_policy(reason)?;
                return Ok(Some(format!(
                    "idle policy cycled worker after {idle_for:?} idle with limit {limit:?}"
                )));
            }
        }

        Ok(None)
    }

    fn cycle_for_policy(&mut self, reason: LeanWorkerRestartReason) -> Result<(), LeanWorkerError> {
        self.capability.cycle_with_restart_reason(reason.clone())?;
        self.last_restart_reason = Some(reason);
        self.last_activity = Instant::now();
        self.last_rss_kib = None;
        self.policy_restarts = self.policy_restarts.saturating_add(1);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PoolRssTotal {
    total_kib: u64,
    unavailable: u64,
}

/// Borrowed lease for running typed commands on a compatible worker session.
///
/// The lease does not expose which worker was selected. If a command triggers
/// timeout, cancellation, child failure, or explicit cycle, the lease becomes
/// invalid and a fresh lease must be acquired from the pool.
#[derive(Debug)]
pub struct LeanWorkerSessionLease<'pool> {
    entry: &'pool mut PoolEntry,
    config: LeanWorkerPoolConfig,
    valid: bool,
    invalidation_reason: Option<String>,
    request_timeout_override: Option<Duration>,
}

impl LeanWorkerSessionLease<'_> {
    /// Return the session key that justified this lease.
    #[must_use]
    pub fn session_key(&self) -> &LeanWorkerSessionKey {
        &self.entry.key
    }

    /// Return protocol/runtime facts reported by the leased worker child.
    #[must_use]
    pub fn runtime_metadata(&self) -> LeanWorkerRuntimeMetadata {
        self.entry.capability.runtime_metadata()
    }

    /// Return whether this lease can still run commands.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// Return an operational snapshot for the worker entry behind this lease.
    ///
    /// This is the sampling hook to use while a lease is checked out. It keeps
    /// child identity, pipe state, and protocol details hidden; the snapshot
    /// only reports the same aggregate counters as `LeanWorkerPool::snapshot`
    /// for the leased entry.
    #[must_use]
    pub fn snapshot(&self) -> LeanWorkerPoolSnapshot {
        LeanWorkerPool::snapshot_from_lease_config(&self.config, self.entry)
    }

    /// Explicitly cycle the leased worker and invalidate this lease.
    ///
    /// Acquire a fresh lease before running more work. The pool keeps the
    /// restarted child available for compatible future leases.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the lease was already invalid or the
    /// underlying worker cannot be cycled.
    pub fn cycle(&mut self) -> Result<(), LeanWorkerError> {
        self.ensure_valid()?;
        self.entry.capability.cycle()?;
        self.invalidate("explicit worker cycle");
        Ok(())
    }

    /// Set the request timeout for commands run through this lease.
    ///
    /// The pool and supervisor still own the watchdog, child kill, and restart
    /// bookkeeping. This method only selects the deadline for subsequent
    /// leased requests.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the lease was already invalidated.
    pub fn set_request_timeout(&mut self, timeout: Duration) -> Result<(), LeanWorkerError> {
        self.ensure_valid()?;
        self.request_timeout_override = Some(timeout);
        Ok(())
    }

    /// Run a typed non-streaming downstream JSON command through this lease.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` for invalidated leases, session startup
    /// failures, typed command errors, cancellation, timeout, child failure,
    /// progress panic, or protocol failure.
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
        self.run_with_current_session(cancellation, progress, |session| {
            session.run_json_command(command, request, cancellation, progress)
        })
    }

    /// Run a typed downstream streaming command through this lease.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` for invalidated leases, row or summary decode
    /// errors, sink failures, cancellation, timeout, child failure, or protocol
    /// failure.
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
        self.run_with_current_session(cancellation, progress, |session| {
            session.run_streaming_command(command, request, rows, diagnostics, cancellation, progress)
        })
    }

    fn run_with_current_session<T>(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
        mut command: impl FnMut(&mut LeanWorkerSession<'_>) -> Result<T, LeanWorkerError>,
    ) -> Result<T, LeanWorkerError> {
        self.ensure_valid()?;
        self.enforce_policy_before_request()?;
        let request_timeout = self.request_timeout_override;
        let result = {
            let mut session = self.entry.capability.attach_open_session();
            if let Some(timeout) = request_timeout {
                session.set_request_timeout(timeout);
            }
            command(&mut session)
        };
        let result = match result {
            Err(err) if worker_session_missing(&err) => self
                .entry
                .capability
                .open_session(cancellation, progress)
                .and_then(|mut session| {
                    if let Some(timeout) = request_timeout {
                        session.set_request_timeout(timeout);
                    }
                    command(&mut session)
                }),
            other => other,
        };
        self.map_lifecycle_result(result)
    }

    fn ensure_valid(&self) -> Result<(), LeanWorkerError> {
        if self.valid {
            Ok(())
        } else {
            Err(LeanWorkerError::LeaseInvalidated {
                reason: self
                    .invalidation_reason
                    .clone()
                    .unwrap_or_else(|| "lease was invalidated by a worker lifecycle transition".to_owned()),
            })
        }
    }

    fn enforce_policy_before_request(&mut self) -> Result<(), LeanWorkerError> {
        if let Some(reason) = self.entry.enforce_policy(&self.config)? {
            self.invalidate(reason.clone());
            return Err(LeanWorkerError::LeaseInvalidated { reason });
        }
        Ok(())
    }

    fn map_lifecycle_result<T>(&mut self, result: Result<T, LeanWorkerError>) -> Result<T, LeanWorkerError> {
        if self.request_timeout_override.is_some() {
            self.entry
                .capability
                .set_request_timeout(self.entry.base_request_timeout);
        }
        match result {
            Ok(value) => {
                self.entry.last_activity = Instant::now();
                Ok(value)
            }
            Err(err) => {
                self.entry.last_activity = Instant::now();
                if invalidates_lease(&err) {
                    self.invalidate(invalidation_reason(&err));
                }
                Err(err)
            }
        }
    }

    fn invalidate(&mut self, reason: impl Into<String>) {
        self.valid = false;
        self.invalidation_reason = Some(reason.into());
    }
}

impl Drop for LeanWorkerSessionLease<'_> {
    fn drop(&mut self) {
        self.entry.active_leases = self.entry.active_leases.saturating_sub(1);
    }
}

fn invalidates_lease(err: &LeanWorkerError) -> bool {
    matches!(
        err,
        LeanWorkerError::Cancelled { .. }
            | LeanWorkerError::Timeout { .. }
            | LeanWorkerError::ChildExited { .. }
            | LeanWorkerError::ChildPanicOrAbort { .. }
            | LeanWorkerError::CapabilityMetadataMismatch { .. }
    )
}

fn invalidation_reason(err: &LeanWorkerError) -> String {
    if let LeanWorkerError::Cancelled { operation } = err {
        format!("cancelled during {operation}")
    } else if let LeanWorkerError::Timeout { operation, .. } = err {
        format!("timed out during {operation}")
    } else if matches!(err, LeanWorkerError::ChildExited { .. }) {
        "worker child exited".to_owned()
    } else if matches!(err, LeanWorkerError::ChildPanicOrAbort { .. }) {
        "worker child exited fatally".to_owned()
    } else if let LeanWorkerError::CapabilityMetadataMismatch { export, .. } = err {
        format!("capability metadata mismatch from {export}")
    } else {
        "worker lifecycle transition".to_owned()
    }
}

fn worker_session_missing(err: &LeanWorkerError) -> bool {
    matches!(err, LeanWorkerError::Worker { code, .. } if code == "lean_rs.worker.session_missing")
}

fn normalize_import_workspace_root(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}
