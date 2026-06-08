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

use lean_rs_worker_protocol::types::{
    LeanWorkerElabOptions, LeanWorkerImportStats, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQuerySelector,
    LeanWorkerOutputBudgets, LeanWorkerResourceExhaustedFacts, LeanWorkerSessionImportProfile,
};

use crate::capability::{LeanWorkerCapability, LeanWorkerCapabilityBuilder};
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerDiagnosticSink, LeanWorkerJsonCommand, LeanWorkerProgressSink,
    LeanWorkerRuntimeMetadata, LeanWorkerSession, LeanWorkerStreamingCommand, LeanWorkerTypedDataSink,
    LeanWorkerTypedStreamSummary,
};
use crate::supervisor::{LeanWorkerError, LeanWorkerReplacementTiming, LeanWorkerRestartReason, LeanWorkerStatus};

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
    built_manifest_path: Option<PathBuf>,
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
        let project_root = normalize_import_workspace_root(project_root.into());
        Self {
            import_workspace_root: normalize_import_workspace_root(project_root.clone()),
            project_root,
            built_manifest_path: None,
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

    pub(crate) fn with_built_manifest_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.built_manifest_path = Some(normalize_import_workspace_root(path.into()));
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
///
/// Long-running hosts should configure the worker count, total child RSS
/// budget, per-worker RSS ceiling, and each worker's
/// `LeanWorkerRestartPolicy` together. The pool admits cold distinct session
/// keys; the worker restart policy bounds retained Lean state inside an
/// admitted child.
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
    ///
    /// `max_workers` is clamped to at least one. Production callers should
    /// normally add [`Self::max_total_child_rss_kib`] and
    /// [`Self::per_worker_rss_ceiling_kib`] rather than relying on the worker
    /// count alone.
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
    /// admission claim. Size this with the worker count; for example, a host
    /// that admits `n` children under a per-worker cap should normally set this
    /// to `n * per_worker_rss_ceiling_kib`.
    #[must_use]
    pub fn max_total_child_rss_kib(mut self, limit: u64) -> Self {
        self.max_total_child_rss_kib = Some(limit.max(1));
        self
    }

    /// Cycle a worker before assigning work when its sampled RSS reaches `limit`.
    ///
    /// This is the pool-side assignment guard. The worker's own
    /// `LeanWorkerRestartPolicy::memory_bounded` should use a compatible RSS
    /// cap so fresh import-like requests are checked before entering the child.
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
    pub cold_open_attempts: u64,
    pub cold_open_admitted: u64,
    pub cold_open_refusals: u64,
    pub key_hits: u64,
    pub key_misses: u64,
    pub distinct_keys_seen: u64,
    pub fresh_cold_opens_avoided: u64,
    pub miss_empty_pool: u64,
    pub miss_no_matching_key: u64,
    pub last_key_miss_reason: Option<String>,
    pub import_like_requests: u64,
    pub concurrent_cold_opens_observed: u64,
    pub rss_before_admission_kib: Option<u64>,
    pub rss_after_open_kib: Option<u64>,
    pub refusal_reason: Option<String>,
    pub replacement_attempts: u64,
    pub replacement_successes: u64,
    pub replacement_failures: u64,
    pub replacement_budget_admitted: u64,
    pub replacement_budget_skipped: u64,
    pub last_replacement_timing: Option<LeanWorkerReplacementTiming>,
    pub last_replacement_skipped_reason: Option<String>,
    pub last_spawn_handshake_elapsed: Option<Duration>,
    pub last_capability_load_elapsed: Option<Duration>,
    pub last_session_open_import_elapsed: Option<Duration>,
    pub last_first_command_elapsed: Option<Duration>,
    pub last_warm_command_elapsed: Option<Duration>,
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
    cold_open_attempts: u64,
    cold_open_admitted: u64,
    cold_open_refusals: u64,
    key_hits: u64,
    key_misses: u64,
    seen_keys: Vec<LeanWorkerSessionKey>,
    fresh_cold_opens_avoided: u64,
    miss_empty_pool: u64,
    miss_no_matching_key: u64,
    last_key_miss_reason: Option<String>,
    cold_opens_in_progress: u64,
    concurrent_cold_opens_observed: u64,
    rss_before_admission_kib: Option<u64>,
    rss_after_open_kib: Option<u64>,
    refusal_reason: Option<String>,
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
            cold_open_attempts: 0,
            cold_open_admitted: 0,
            cold_open_refusals: 0,
            key_hits: 0,
            key_misses: 0,
            seen_keys: Vec::new(),
            fresh_cold_opens_avoided: 0,
            miss_empty_pool: 0,
            miss_no_matching_key: 0,
            last_key_miss_reason: None,
            cold_opens_in_progress: 0,
            concurrent_cold_opens_observed: 0,
            rss_before_admission_kib: None,
            rss_after_open_kib: None,
            refusal_reason: None,
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
        self.remember_seen_key(&key);
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            self.record_key_hit();
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

        let miss_reason = if self.entries.is_empty() {
            "empty_pool"
        } else {
            "no_matching_key"
        };
        self.record_key_miss(miss_reason);
        self.record_cold_open_attempt();
        let rss_before_admission = self.refresh_total_child_rss();
        self.rss_before_admission_kib = rss_before_admission.available_total();
        if self.entries.len() >= self.config.max_workers {
            return self.pool_full_error();
        }
        self.ensure_spawn_within_total_rss_budget(rss_before_admission)?;

        self.record_cold_open_admitted();
        let capability = match builder.clone().open() {
            Ok(capability) => capability,
            Err(err) => {
                self.cold_opens_in_progress = self.cold_opens_in_progress.saturating_sub(1);
                return Err(err);
            }
        };
        self.cold_opens_in_progress = self.cold_opens_in_progress.saturating_sub(1);
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
            commands_since_open: 0,
        });
        let index = self
            .entries
            .len()
            .checked_sub(1)
            .ok_or_else(|| LeanWorkerError::Protocol {
                message: "worker pool failed to retain newly opened entry".to_owned(),
            })?;
        let entry = self.entries.get_mut(index).ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker pool failed to retain newly opened entry".to_owned(),
        })?;
        let _ = entry.sample_rss();
        self.rss_after_open_kib = total_known_child_rss_kib(&self.entries);
        let entry = self.entries.get_mut(index).ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker pool failed to retain newly opened entry".to_owned(),
        })?;
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
            self.cold_open_attempts,
            self.cold_open_admitted,
            self.cold_open_refusals,
            self.key_hits,
            self.key_misses,
            u64::try_from(self.seen_keys.len()).unwrap_or(u64::MAX),
            self.fresh_cold_opens_avoided,
            self.miss_empty_pool,
            self.miss_no_matching_key,
            self.last_key_miss_reason.clone(),
            self.concurrent_cold_opens_observed,
            self.rss_before_admission_kib,
            self.rss_after_open_kib,
            self.refusal_reason.clone(),
        )
    }

    fn snapshot_from_lease_config(config: &LeanWorkerPoolConfig, entry: &PoolEntry) -> LeanWorkerPoolSnapshot {
        snapshot_from_entries(
            config,
            std::slice::from_ref(entry),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            None,
            0,
            None,
            None,
            None,
        )
    }
}

fn snapshot_from_entries(
    config: &LeanWorkerPoolConfig,
    entries: &[PoolEntry],
    queue_timeouts: u64,
    memory_budget_rejections: u64,
    cold_open_attempts: u64,
    cold_open_admitted: u64,
    cold_open_refusals: u64,
    key_hits: u64,
    key_misses: u64,
    distinct_keys_seen: u64,
    fresh_cold_opens_avoided: u64,
    miss_empty_pool: u64,
    miss_no_matching_key: u64,
    last_key_miss_reason: Option<String>,
    concurrent_cold_opens_observed: u64,
    rss_before_admission_kib: Option<u64>,
    rss_after_open_kib: Option<u64>,
    refusal_reason: Option<String>,
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
        cold_open_attempts,
        cold_open_admitted,
        cold_open_refusals,
        key_hits,
        key_misses,
        distinct_keys_seen,
        fresh_cold_opens_avoided,
        miss_empty_pool,
        miss_no_matching_key,
        last_key_miss_reason,
        import_like_requests: entries
            .iter()
            .map(|entry| entry.capability.stats().import_like_admission_attempts)
            .sum(),
        concurrent_cold_opens_observed,
        rss_before_admission_kib,
        rss_after_open_kib,
        refusal_reason,
        replacement_attempts: entries
            .iter()
            .map(|entry| entry.capability.stats().replacement_attempts)
            .sum(),
        replacement_successes: entries
            .iter()
            .map(|entry| entry.capability.stats().replacement_successes)
            .sum(),
        replacement_failures: entries
            .iter()
            .map(|entry| entry.capability.stats().replacement_failures)
            .sum(),
        replacement_budget_admitted: entries
            .iter()
            .map(|entry| entry.capability.stats().replacement_budget_admitted)
            .sum(),
        replacement_budget_skipped: entries
            .iter()
            .map(|entry| entry.capability.stats().replacement_budget_skipped)
            .sum(),
        last_replacement_timing: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_replacement_timing),
        last_replacement_skipped_reason: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_replacement_skipped_reason),
        last_spawn_handshake_elapsed: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_spawn_handshake_elapsed),
        last_capability_load_elapsed: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_capability_load_elapsed),
        last_session_open_import_elapsed: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_session_open_import_elapsed),
        last_first_command_elapsed: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_first_command_elapsed),
        last_warm_command_elapsed: entries
            .iter()
            .rev()
            .find_map(|entry| entry.capability.stats().last_warm_command_elapsed),
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

    fn ensure_spawn_within_total_rss_budget(&mut self, rss: PoolRssTotal) -> Result<(), LeanWorkerError> {
        let Some(limit_kib) = self.config.max_total_child_rss_kib else {
            return Ok(());
        };
        if rss.unavailable > 0 {
            return Ok(());
        }
        if rss.total_kib >= limit_kib {
            self.memory_budget_rejections = self.memory_budget_rejections.saturating_add(1);
            self.record_cold_open_refusal("rss_budget");
            let last_import_stats = self.latest_import_stats();
            return Err(LeanWorkerError::WorkerPoolMemoryBudgetExceeded {
                current_kib: rss.total_kib,
                limit_kib,
                last_import_stats: last_import_stats.clone().map(Box::new),
                resource: Box::new(self.pool_resource_facts(
                    "worker_pool_total_rss_budget",
                    Some(rss.total_kib),
                    Some(limit_kib),
                    None,
                    last_import_stats,
                )),
            });
        }
        Ok(())
    }

    fn record_cold_open_attempt(&mut self) {
        self.cold_open_attempts = self.cold_open_attempts.saturating_add(1);
        self.refusal_reason = None;
        if self.cold_opens_in_progress > 0 {
            self.concurrent_cold_opens_observed = self.concurrent_cold_opens_observed.saturating_add(1);
        }
    }

    fn record_cold_open_admitted(&mut self) {
        self.cold_open_admitted = self.cold_open_admitted.saturating_add(1);
        self.cold_opens_in_progress = self.cold_opens_in_progress.saturating_add(1);
    }

    fn record_cold_open_refusal(&mut self, reason: impl Into<String>) {
        self.cold_open_refusals = self.cold_open_refusals.saturating_add(1);
        self.refusal_reason = Some(reason.into());
    }

    fn remember_seen_key(&mut self, key: &LeanWorkerSessionKey) {
        if self.seen_keys.iter().all(|seen| seen != key) {
            self.seen_keys.push(key.clone());
        }
    }

    fn record_key_hit(&mut self) {
        self.key_hits = self.key_hits.saturating_add(1);
        self.fresh_cold_opens_avoided = self.fresh_cold_opens_avoided.saturating_add(1);
        self.last_key_miss_reason = None;
    }

    fn record_key_miss(&mut self, reason: &'static str) {
        self.key_misses = self.key_misses.saturating_add(1);
        match reason {
            "empty_pool" => self.miss_empty_pool = self.miss_empty_pool.saturating_add(1),
            "no_matching_key" => self.miss_no_matching_key = self.miss_no_matching_key.saturating_add(1),
            _ => {}
        }
        self.last_key_miss_reason = Some(reason.to_owned());
    }

    fn latest_import_stats(&self) -> Option<LeanWorkerImportStats> {
        self.entries
            .iter()
            .find_map(|entry| entry.capability.stats().last_import_stats)
    }

    fn pool_resource_facts(
        &self,
        cause: &str,
        current_rss_kib: Option<u64>,
        limit_kib: Option<u64>,
        queue_wait: Option<Duration>,
        last_import_stats: Option<LeanWorkerImportStats>,
    ) -> LeanWorkerResourceExhaustedFacts {
        let restart_reason = self
            .entries
            .iter()
            .find_map(|entry| entry.capability.stats().last_restart_reason)
            .map(|reason| reason.stable_cause().to_owned());
        let import_like_requests = self
            .entries
            .iter()
            .map(|entry| entry.capability.stats().import_like_admission_attempts)
            .fold(0_u64, u64::saturating_add);
        LeanWorkerResourceExhaustedFacts {
            cause: cause.to_owned(),
            work_entered_child: false,
            operation: Some("worker_pool_acquire_lease".to_owned()),
            current_rss_kib,
            limit_kib,
            import_count: None,
            worker_generation: None,
            restart_reason,
            queue_wait_ms: queue_wait.map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
            duration_ms: None,
            cold_open_attempts: Some(self.cold_open_attempts),
            cold_open_admitted: Some(self.cold_open_admitted),
            cold_open_refusals: Some(self.cold_open_refusals),
            import_like_requests: Some(import_like_requests),
            import_like_admitted: None,
            last_import_stats,
        }
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
            self.record_cold_open_refusal("max_workers");
            return Err(LeanWorkerError::WorkerPoolExhausted {
                max_workers: self.config.max_workers,
                resource: Box::new(self.pool_resource_facts(
                    "worker_pool_max_workers",
                    None,
                    None,
                    None,
                    self.latest_import_stats(),
                )),
            });
        }
        let started = Instant::now();
        while started.elapsed() < self.config.queue_wait_timeout {
            let remaining = self.config.queue_wait_timeout.saturating_sub(started.elapsed());
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
        self.queue_timeouts = self.queue_timeouts.saturating_add(1);
        self.record_cold_open_refusal("queue_timeout");
        Err(LeanWorkerError::WorkerPoolQueueTimeout {
            waited: self.config.queue_wait_timeout,
            resource: Box::new(self.pool_resource_facts(
                "worker_pool_queue_timeout",
                None,
                None,
                Some(self.config.queue_wait_timeout),
                self.latest_import_stats(),
            )),
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
    commands_since_open: u64,
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
        self.commands_since_open = 0;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PoolRssTotal {
    total_kib: u64,
    unavailable: u64,
}

impl PoolRssTotal {
    fn available_total(self) -> Option<u64> {
        if self.unavailable == 0 {
            Some(self.total_kib)
        } else {
            None
        }
    }
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

    /// Parse and elaborate a Lean module once, returning bounded selector
    /// projections through the already-open worker session behind this lease.
    ///
    /// This is the warm proof-agent batch path for pool callers. It keeps
    /// worker identity, session reopening, policy checks, timeout handling,
    /// and `session_missing` retry inside the lease lifecycle wrapper.
    /// Batching here reduces request/session churn on a warm child; it does
    /// not reclaim Lean import memory or replace worker cycling for full
    /// `loadExts := true` sessions.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` for invalidated leases, session startup
    /// failures, cancellation, timeout, child failure, progress panic, or
    /// protocol failure. Header-parse failures, missing imports, selector
    /// unavailability, and budget exhaustion surface in the returned
    /// [`LeanWorkerModuleQueryBatchOutcome`].
    pub fn process_module_query_batch(
        &mut self,
        source: &str,
        selectors: &[LeanWorkerModuleQuerySelector],
        budgets: &LeanWorkerOutputBudgets,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryBatchOutcome, LeanWorkerError> {
        self.run_with_current_session(cancellation, progress, |session| {
            session.process_module_query_batch(source, selectors, budgets, options, cancellation, progress)
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
        let first_command_after_open = self.entry.commands_since_open == 0;
        let command_started = Instant::now();
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
        self.entry
            .capability
            .record_command_timing(first_command_after_open, command_started.elapsed());
        self.entry.commands_since_open = self.entry.commands_since_open.saturating_add(1);
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
    if let LeanWorkerError::Cancelled { operation, .. } = err {
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
