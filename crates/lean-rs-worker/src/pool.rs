//! Local worker-pool orchestration and session leasing.
//!
//! The pool sits above `LeanWorkerCapabilityBuilder` and typed commands. It
//! chooses a compatible local child process for capability work, while callers
//! only see session requirements and a lease that can run typed commands.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::capability::{LeanWorkerCapability, LeanWorkerCapabilityBuilder};
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerDiagnosticSink, LeanWorkerJsonCommand, LeanWorkerProgressSink,
    LeanWorkerRuntimeMetadata, LeanWorkerStreamingCommand, LeanWorkerTypedDataSink, LeanWorkerTypedStreamSummary,
};
use crate::supervisor::{LeanWorkerError, LeanWorkerStatus};

/// Coarse restart-policy class used in pool session keys.
///
/// The pool key records whether a session was opened under the default policy
/// or a caller-selected policy class. It deliberately does not expose every
/// restart-policy knob as key material; prompt 79 owns memory-aware scheduling
/// and richer policy admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LeanWorkerRestartPolicyClass {
    Default,
    Custom,
}

/// Worker reuse key for a capability-backed session.
///
/// A session key answers only one pool question: can an already-open child
/// session safely host compatible work? It is not a downstream cache key, and
/// it does not encode row schemas, cache validity, ranking, reporting, or
/// source provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerSessionKey {
    project_root: PathBuf,
    package: String,
    lib_name: String,
    imports: Vec<String>,
    metadata_expectation: Option<LeanWorkerMetadataExpectationKey>,
    toolchain_fingerprint: lean_toolchain::ToolchainFingerprint,
    restart_policy_class: LeanWorkerRestartPolicyClass,
}

impl LeanWorkerSessionKey {
    /// Create a session key from the caller-visible capability requirements.
    #[must_use]
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
            metadata_expectation: None,
            toolchain_fingerprint: lean_toolchain::ToolchainFingerprint::current(),
            restart_policy_class: LeanWorkerRestartPolicyClass::Default,
        }
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
        expected: Option<crate::session::LeanWorkerCapabilityMetadata>,
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

    /// Return the Lake project root for this session key.
    #[must_use]
    pub fn project_root(&self) -> &std::path::Path {
        &self.project_root
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
    expected: Option<crate::session::LeanWorkerCapabilityMetadata>,
}

/// Configuration for a local `LeanWorkerPool`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerPoolConfig {
    max_workers: usize,
}

impl LeanWorkerPoolConfig {
    /// Create pool configuration with a fixed local worker limit.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers: max_workers.max(1),
        }
    }

    /// Return the maximum number of local child workers the pool may own.
    #[must_use]
    pub fn max_workers(&self) -> usize {
        self.max_workers
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
}

/// Local pool for worker-backed capability sessions.
#[derive(Debug)]
pub struct LeanWorkerPool {
    config: LeanWorkerPoolConfig,
    entries: Vec<PoolEntry>,
}

impl LeanWorkerPool {
    /// Create an empty local worker pool.
    #[must_use]
    pub fn new(config: LeanWorkerPoolConfig) -> Self {
        Self {
            config,
            entries: Vec::new(),
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
            let entry = self.entries.get_mut(index).ok_or_else(|| LeanWorkerError::Protocol {
                message: "worker pool entry disappeared during lease acquisition".to_owned(),
            })?;
            return Ok(LeanWorkerSessionLease {
                entry,
                valid: true,
                invalidation_reason: None,
                request_timeout_override: None,
            });
        }

        if self.entries.len() >= self.config.max_workers {
            return Err(LeanWorkerError::WorkerPoolExhausted {
                max_workers: self.config.max_workers,
            });
        }

        let capability = builder.clone().open()?;
        let base_request_timeout = builder.pool_request_timeout();
        self.entries.push(PoolEntry {
            key,
            builder,
            capability,
            base_request_timeout,
        });
        let entry = self.entries.last_mut().ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker pool failed to retain newly opened entry".to_owned(),
        })?;
        Ok(LeanWorkerSessionLease {
            entry,
            valid: true,
            invalidation_reason: None,
            request_timeout_override: None,
        })
    }

    /// Return a public snapshot of pool state.
    #[must_use]
    pub fn snapshot(&self) -> LeanWorkerPoolSnapshot {
        LeanWorkerPoolSnapshot {
            max_workers: self.config.max_workers,
            workers: self.entries.len(),
        }
    }

    fn ensure_entry_running(&mut self, index: usize) -> Result<(), LeanWorkerError> {
        let entry = self.entries.get_mut(index).ok_or_else(|| LeanWorkerError::Protocol {
            message: "worker pool entry disappeared during liveness check".to_owned(),
        })?;
        match entry.capability.worker_mut().status()? {
            LeanWorkerStatus::Running => Ok(()),
            LeanWorkerStatus::Exited(_exit) => {
                entry.capability = entry.builder.clone().open()?;
                Ok(())
            }
        }
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
}

/// Borrowed lease for running typed commands on a compatible worker session.
///
/// The lease does not expose which worker was selected. If a command triggers
/// timeout, cancellation, child failure, or explicit cycle, the lease becomes
/// invalid and a fresh lease must be acquired from the pool.
#[derive(Debug)]
pub struct LeanWorkerSessionLease<'pool> {
    entry: &'pool mut PoolEntry,
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
        self.entry.capability.worker_mut().cycle()?;
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
        self.ensure_valid()?;
        let request_timeout = self.request_timeout_override;
        let result = self
            .entry
            .capability
            .open_session(cancellation, progress)
            .and_then(|mut session| {
                if let Some(timeout) = request_timeout {
                    session.set_request_timeout(timeout);
                }
                session.run_json_command(command, request, cancellation, progress)
            });
        self.map_lifecycle_result(result)
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
        self.ensure_valid()?;
        let request_timeout = self.request_timeout_override;
        let result = self
            .entry
            .capability
            .open_session(cancellation, progress)
            .and_then(|mut session| {
                if let Some(timeout) = request_timeout {
                    session.set_request_timeout(timeout);
                }
                session.run_streaming_command(command, request, rows, diagnostics, cancellation, progress)
            });
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

    fn map_lifecycle_result<T>(&mut self, result: Result<T, LeanWorkerError>) -> Result<T, LeanWorkerError> {
        if self.request_timeout_override.is_some() {
            self.entry
                .capability
                .worker_mut()
                .set_request_timeout(self.entry.base_request_timeout);
        }
        match result {
            Ok(value) => Ok(value),
            Err(err) => {
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
