//! Typed requests: one method per request kind, each sending the request and
//! matching the reply it expects.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use lean_rs_worker_protocol::protocol::{HostSessionMode, Request, Response};
use lean_rs_worker_protocol::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationBatchRequest,
    LeanWorkerDeclarationVerificationBatchResult, LeanWorkerDeclarationVerificationBatchRow,
    LeanWorkerDeclarationVerificationFacts, LeanWorkerDeclarationVerificationRequest,
    LeanWorkerDeclarationVerificationResult, LeanWorkerDeclarationVerificationStatus, LeanWorkerDoctorReport,
    LeanWorkerElabOptions, LeanWorkerElabResult, LeanWorkerHostRuntimeMetadata, LeanWorkerKernelResult,
    LeanWorkerMetaResult, LeanWorkerMetaTransparency, LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchEnvelope,
    LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryCacheFacts,
    LeanWorkerModuleQueryOutcome, LeanWorkerModuleQuerySelector, LeanWorkerModuleSnapshotCacheClearResult,
    LeanWorkerOutputBudgets, LeanWorkerProofAttemptRequest, LeanWorkerProofAttemptResult, LeanWorkerRendered,
    LeanWorkerResourceExhaustedFacts,
};
use lean_rs_worker_protocol::worker_exports::{fixture_mul_signature, fixture_panic_signature};

use super::unexpected_response;
use crate::session::{
    LeanWorkerCancellationToken, LeanWorkerProgressSink, LeanWorkerRawDataRow, LeanWorkerRawDataSink,
    LeanWorkerSessionConfig, LeanWorkerSessionMode, check_cancelled,
};
use crate::supervisor::error::{LeanWorkerError, worker_resource_facts};
use crate::supervisor::restart::LeanWorkerRestartReason;
use crate::supervisor::{LeanWorker, LeanWorkerExit};

/// Reuse-hint capacity for an embedder that sets no `max_imports`. Mirrors the
/// child's own `SESSION_POOL_DEFAULT_CAPACITY`, which is what such a child uses.
const DEFAULT_REUSE_HINT_CAPACITY: usize = 4;

impl LeanWorker {
    /// Check whether the worker responds to requests.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the protocol fails, or
    /// the child returns a typed worker error.
    pub fn health(&mut self) -> Result<(), LeanWorkerError> {
        self.prepare_host_request(false, "health", None, None)?;
        self.send_request("health", Request::Health)?;
        self.record_request(false);
        match self.read_response("health")? {
            Response::HealthOk => Ok(()),
            other => Err(unexpected_response("health", &other)),
        }
    }

    /// Load the in-tree fixture capability in the worker child.
    ///
    /// This is a fixture-only entry point used to exercise the supervisor path
    /// in tests. The supported public path is `open_session`, which returns the
    /// host-session adapter instead of expanding this fixture surface.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, fixture loading fails,
    /// or protocol communication fails.
    pub fn load_fixture_capability(&mut self, fixture_root: impl AsRef<Path>) -> Result<(), LeanWorkerError> {
        let manifest = fixture_capability_manifest(fixture_root.as_ref())?;
        self.prepare_request(true, "load_fixture_capability", None)?;
        self.send_request(
            "load_fixture_capability",
            Request::LoadFixtureCapability {
                manifest_path: path_string(&manifest),
            },
        )?;
        self.record_request(true);
        match self.read_response("load_fixture_capability")? {
            Response::CapabilityLoaded => Ok(()),
            other => Err(unexpected_response("load_fixture_capability", &other)),
        }
    }

    /// Call the fixture multiplication export in the worker child.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker is dead, the export fails, or
    /// protocol communication fails.
    pub fn call_fixture_mul(
        &mut self,
        fixture_root: impl AsRef<Path>,
        lhs: u64,
        rhs: u64,
    ) -> Result<u64, LeanWorkerError> {
        let manifest = fixture_capability_manifest(fixture_root.as_ref())?;
        self.prepare_request(true, "call_fixture_mul", None)?;
        self.send_request(
            "call_fixture_mul",
            Request::CallFixtureMul {
                manifest_path: path_string(&manifest),
                lhs,
                rhs,
            },
        )?;
        self.record_request(true);
        match self.read_response("call_fixture_mul")? {
            Response::U64 { value } => Ok(value),
            other => Err(unexpected_response("call_fixture_mul", &other)),
        }
    }

    #[doc(hidden)]
    /// Trigger the fixture panic path.
    ///
    /// # Errors
    ///
    /// Returns `LeanWorkerError` if the worker does not exit fatally or if the
    /// protocol fails before the panic path runs.
    pub fn __trigger_lean_panic_fixture(
        mut self,
        fixture_root: impl AsRef<Path>,
    ) -> Result<LeanWorkerExit, LeanWorkerError> {
        let manifest = fixture_capability_manifest(fixture_root.as_ref())?;
        self.prepare_request(true, "trigger_lean_panic", None)?;
        self.send_request(
            "trigger_lean_panic",
            Request::TriggerLeanPanic {
                manifest_path: path_string(&manifest),
            },
        )?;
        self.record_request(true);
        match self.read_response("trigger_lean_panic") {
            Ok(response) => Err(unexpected_response("trigger_lean_panic", &response)),
            Err(LeanWorkerError::ChildPanicOrAbort { exit }) => Ok(exit),
            Err(err) => Err(err),
        }
    }

    /// How many session keys the reuse hint may remember.
    ///
    /// Exactly the pool capacity the child was spawned with, resolved through
    /// the same function that set it, so the parent predicts reuse for exactly
    /// the keys the child can still be holding. An embedder that configures
    /// neither gets the child's own default; the two disagree only if the
    /// child's `LEAN_RS_WORKER_SESSION_POOL_CAPACITY` was set by hand.
    ///
    /// The child checks its actual cache before any new byte-budgeted import.
    /// Over-predicting costs one recycle response and bounded retry. Under-
    /// predicting costs one admission check that was not needed — free under a
    /// byte bound, since the check is idempotent and a re-examined budget that
    /// has not moved does not restart anything.
    fn reuse_hint_capacity(&self) -> usize {
        self.config
            .resolved_session_pool_capacity()
            .unwrap_or(DEFAULT_REUSE_HINT_CAPACITY)
            .max(1)
    }

    /// Whether the current child holds a session this parent opened. A
    /// replacement that has not opened a session yet holds none.
    pub(crate) fn holds_session(&self) -> bool {
        !self.open_session_keys.is_empty()
    }

    /// Record that the child holds `config`, MRU at the back, evicting the
    /// least recently used key past capacity — the child's own eviction order.
    fn note_open_session_key(&mut self, config: &LeanWorkerSessionConfig) {
        if let Some(index) = self.open_session_keys.iter().position(|key| key == config) {
            let key = self.open_session_keys.remove(index);
            self.open_session_keys.push(key);
            return;
        }
        self.open_session_keys.push(config.clone());
        let capacity = self.reuse_hint_capacity();
        while self.open_session_keys.len() > capacity {
            self.open_session_keys.remove(0);
        }
    }

    pub(crate) fn open_worker_session(
        &mut self,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<(), LeanWorkerError> {
        self.host_runtime_metadata = None;
        self.last_host_session_refusal = None;
        let mut observed_pre_body_refusal = None;
        let result = (|| {
            self.open_worker_session_attempt(config, cancellation, progress, true)?;
            if config.host_runtime_expectation().is_some() {
                let actual = self.worker_host_runtime_metadata(cancellation, progress)?;
                if let Some(expected) = config.host_runtime_expectation()
                    && let Err(message) = expected.validate(&actual)
                {
                    // This exact metadata response was consumed. It is a
                    // pre-body refusal, unlike a transport/protocol failure
                    // while waiting for the response.
                    observed_pre_body_refusal.clone_from(&self.last_request);
                    return Err(LeanWorkerError::Protocol {
                        message: format!("host runtime compatibility: {message}"),
                    });
                }
                self.host_runtime_metadata = Some(actual);
            }
            Ok(())
        })();
        if result.is_err() {
            // Preserve the exact framed pre-body refusal before cleanup sends
            // another request or replaces the generation. It is no exit claim.
            let refusal = observed_pre_body_refusal
                .or_else(|| {
                    matches!(&result, Err(LeanWorkerError::Worker { .. }))
                        .then(|| self.last_request.clone())
                        .flatten()
                })
                .filter(|request| {
                    matches!(
                        request.operation,
                        "open_worker_session" | "worker_host_runtime_metadata"
                    )
                });
            self.last_host_session_refusal = refusal;
        }
        result
    }

    fn open_worker_session_attempt(
        &mut self,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
        allow_retry: bool,
    ) -> Result<(), LeanWorkerError> {
        const OPERATION: &str = "open_worker_session";
        check_cancelled(OPERATION, cancellation)?;
        // Replace a lost child first, so the restart below cannot record a
        // planned reason for what was a failure.
        self.ensure_child(OPERATION, cancellation)?;
        // An explicit switch needs only its requested profile in the new child.
        if self.budget.accounting_unknown {
            self.restart_for_request(
                LeanWorkerRestartReason::ImportAccountingUnknown,
                OPERATION,
                cancellation,
            )?;
        }
        let before_restarts = self.stats.restarts;
        // A hint, not a decision: it only picks which admission checks run
        // before the frame goes out. What actually happened is whichever of
        // `HostSessionOpened` / `HostSessionReused` comes back, and the import
        // accounting below follows that, not this.
        let expect_reuse = self.reuse_hint_enabled && self.open_session_keys.iter().any(|key| key == config);
        self.prepare_request(!expect_reuse, OPERATION, cancellation)?;
        // `prepare_request` may have replaced the child — `max_requests`,
        // `idle_timeout`, and `max_rss_kib` all restart from inside it. A fresh
        // child necessarily imports, and reading that as "this worker binary
        // does not reuse sessions" would permanently disable the hint for a
        // reason that has nothing to do with the binary.
        let expect_reuse = expect_reuse && self.stats.restarts == before_restarts;
        let import_started = Instant::now();
        let mode = match config.mode() {
            LeanWorkerSessionMode::Capability {
                package,
                lib_name,
                manifest_path,
            } => HostSessionMode::Capability {
                package: package.clone(),
                lib_name: lib_name.clone(),
                manifest_path: manifest_path.as_ref().map(|path| path_string(path)),
            },
            LeanWorkerSessionMode::ShimsOnly => HostSessionMode::ShimsOnly,
        };
        self.send_request_cancellable(
            Request::OpenHostSession {
                project_root: config.project_root_string(),
                mode,
                imports: config.imports().to_vec(),
                import_profile: config.import_profile(),
            },
            OPERATION,
            cancellation,
        )?;
        self.record_request(false);
        // Every non-confirming outcome — an error response, a read failure, a
        // cancellation — leaves the child in a state the parent did not observe:
        // on the mismatch path it parks its outgoing session and may evict from
        // the pool before the import fails, and the parent cannot tell which
        // keys survived. Forgetting all of them is the safe direction, because a
        // hint that under-predicts costs one admission check while one that
        // over-predicts skips one.
        //
        // Only on those paths, though. Clearing *before* the read — which was
        // right when this was a single `Option` being invalidated — would cap
        // the list at one key no matter its capacity, and a one-key hint is
        // exactly the mispredict-every-switch behaviour the list replaces.
        let response = match self.read_response_with_progress(OPERATION, progress, cancellation) {
            Ok(response) => response,
            Err(err) => {
                self.open_session_keys.clear();
                return Err(err);
            }
        };
        match response {
            Response::HostArtifactEpochChanged if allow_retry => {
                // No body ran. A full process cycle invalidates every pooled
                // environment and process-global Lean cache after any writer.
                self.restart_for_request(LeanWorkerRestartReason::Explicit, OPERATION, cancellation)?;
                self.open_worker_session_attempt(config, cancellation, progress, false)
            }
            Response::HeaderImportRecycle if allow_retry => {
                let reason = self
                    .budget
                    .import_limit_reached(&self.config.restart_policy)
                    .unwrap_or_else(|| LeanWorkerRestartReason::ImportResidue {
                        residue_bytes: self.budget.import_residue_bytes,
                        limit_bytes: self.config.restart_policy.max_import_residue_bytes.unwrap_or(0),
                    });
                self.restart_for_request(reason, OPERATION, cancellation)?;
                self.open_worker_session_attempt(config, cancellation, progress, false)
            }
            Response::HostSessionOpened { import_stats } => {
                if expect_reuse && !self.reuse_observed && self.open_session_keys.last() == Some(config) {
                    // Predicted a reuse, got an import, this child has never
                    // reused anything, and the *immediately preceding* open used
                    // this very key: it is a pre-reuse worker binary. Stop
                    // predicting for this generation so the `max_imports` check
                    // is never skipped again.
                    //
                    // The consecutive-key condition is what keeps this from
                    // firing on a healthy child. Any other miss has an innocent
                    // explanation the hint should survive — an entry evicted for
                    // capacity, or one dropped because a later import registered
                    // an environment extension underneath it. Without the
                    // condition, a child whose first two profiles merely differ
                    // in modules disables its own hint during warm-up, after
                    // which every open is admitted as `import_like` and the pool
                    // is dead on arrival.
                    //
                    // A consecutive identical reopen is the one miss no
                    // eviction can produce: nothing imports between the two
                    // calls, so the session cannot have gone stale, and the LRU
                    // cannot have evicted the entry that is currently at its
                    // most-recently-used end.
                    self.reuse_hint_enabled = false;
                }
                self.record_import(&import_stats);
                let session_open_import_elapsed = import_started.elapsed();
                self.stats.last_session_open_import_elapsed = Some(session_open_import_elapsed);
                if self.stats.restarts > before_restarts
                    && let Some(timing) = self.stats.last_replacement_timing.as_mut()
                {
                    timing.session_open_import = session_open_import_elapsed;
                }
                self.stats.last_import_stats = Some(import_stats);
                self.note_open_session_key(config);
                Ok(())
            }
            Response::HostSessionReused { import_stats } => {
                // No import happened, so nothing that measures imports moves:
                // not `stats.imports`, not `imports_since_restart`, and not
                // `last_session_open_import_elapsed`, which would otherwise
                // start reporting a round trip as an import time.
                self.reuse_observed = true;
                self.stats.last_import_stats = Some(import_stats);
                self.note_open_session_key(config);
                Ok(())
            }
            other => {
                self.open_session_keys.clear();
                Err(unexpected_response(OPERATION, &other))
            }
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_elaborate(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerElabResult, LeanWorkerError> {
        self.round_trip(
            "worker_elaborate",
            Request::Elaborate {
                source: source.to_owned(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Elaboration { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_kernel_check(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerKernelResult, LeanWorkerError> {
        self.round_trip(
            "worker_kernel_check",
            Request::KernelCheck {
                source: source.to_owned(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::KernelCheck { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_declaration_kinds(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        self.round_trip(
            "worker_declaration_kinds",
            Request::DeclarationKinds {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Strings { values } => Ok(values),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_declaration_names(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        self.round_trip(
            "worker_declaration_names",
            Request::DeclarationNames {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Strings { values } => Ok(values),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_infer_type(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<LeanWorkerRendered>, LeanWorkerError> {
        self.round_trip(
            "worker_infer_type",
            Request::InferType {
                source: source.to_owned(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::MetaExpr { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_whnf(
        &mut self,
        source: &str,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<LeanWorkerRendered>, LeanWorkerError> {
        self.round_trip(
            "worker_whnf",
            Request::Whnf {
                source: source.to_owned(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::MetaExpr { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_is_def_eq(
        &mut self,
        lhs: &str,
        rhs: &str,
        transparency: LeanWorkerMetaTransparency,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerMetaResult<bool>, LeanWorkerError> {
        self.round_trip(
            "worker_is_def_eq",
            Request::IsDefEq {
                lhs: lhs.to_owned(),
                rhs: rhs.to_owned(),
                transparency,
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::MetaBool { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_describe(
        &mut self,
        name: &str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Option<LeanWorkerDeclarationRow>, LeanWorkerError> {
        self.round_trip(
            "worker_describe",
            Request::Describe { name: name.to_owned() },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::Declaration { row } => Ok(row),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_search_declarations(
        &mut self,
        search: &LeanWorkerDeclarationSearch,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationSearchResult, LeanWorkerError> {
        self.round_trip(
            "worker_search_declarations",
            Request::SearchDeclarations { search: search.clone() },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationSearch { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_declaration_type(
        &mut self,
        name: &str,
        max_bytes: usize,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Option<LeanWorkerDeclarationType>, LeanWorkerError> {
        self.round_trip(
            "worker_declaration_type",
            Request::DeclarationType {
                name: name.to_owned(),
                max_bytes,
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationType { row } => Ok(row),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_inspect_declaration(
        &mut self,
        request: &LeanWorkerDeclarationInspectionRequest,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationInspectionResult, LeanWorkerError> {
        self.round_trip(
            "worker_inspect_declaration",
            Request::InspectDeclaration {
                request: request.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationInspection { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_attempt_proof(
        &mut self,
        request: &LeanWorkerProofAttemptRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerProofAttemptResult, LeanWorkerError> {
        self.round_trip(
            "worker_attempt_proof",
            Request::AttemptProof {
                request: request.clone(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ProofAttempt { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_verify_declaration(
        &mut self,
        request: &LeanWorkerDeclarationVerificationRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationResult, LeanWorkerError> {
        const OPERATION: &str = "worker_verify_declaration";
        let outcome = self.round_trip(
            OPERATION,
            Request::VerifyDeclaration {
                request: request.clone(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationVerification { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        );
        match outcome {
            Ok(result) => Ok(result),
            // A read-only verification must never surface a child abort as a hard
            // error: the worker's own (best-effort) screen could not prevent a
            // residual metavariable panic. The read path has already replaced the
            // child, so report the honest degraded verdict. Verification is
            // monotone, so relabeling a non-result to `BudgetExceeded` never
            // downgrades an `Accepted`.
            Err(LeanWorkerError::ChildPanicOrAbort { .. }) => Ok(LeanWorkerDeclarationVerificationResult::Ok {
                verification_status: LeanWorkerDeclarationVerificationStatus::BudgetExceeded,
                facts: Box::new(LeanWorkerDeclarationVerificationFacts::unavailable()),
                imports: Vec::new(),
            }),
            Err(err) => Err(err),
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_verify_declaration_batch(
        &mut self,
        request: &LeanWorkerDeclarationVerificationBatchRequest,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDeclarationVerificationBatchResult, LeanWorkerError> {
        const OPERATION: &str = "worker_verify_declaration_batch";
        let outcome = self.round_trip(
            OPERATION,
            Request::VerifyDeclarationBatch {
                request: request.clone(),
                options: options.clone(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationVerificationBatch { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        );
        match outcome {
            Ok(result) => Ok(result),
            Err(LeanWorkerError::ChildPanicOrAbort { .. }) => {
                Ok(degraded_declaration_verification_batch_result(request))
            }
            Err(err) => Err(err),
        }
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "filter is cheap to clone, passed by value matches caller shape"
    )]
    pub(crate) fn worker_list_declarations_strings(
        &mut self,
        filter: LeanWorkerDeclarationFilter,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<String>, LeanWorkerError> {
        const OPERATION: &str = "worker_list_declarations_strings";
        check_cancelled(OPERATION, cancellation)?;
        self.prepare_host_request(false, OPERATION, cancellation, progress)?;
        self.send_request_cancellable(
            Request::ListDeclarationsStrings {
                filter,
                progress: progress.is_some(),
            },
            OPERATION,
            cancellation,
        )?;
        self.record_request(false);
        let collector = DeclarationNameCollector::default();
        let response = self.read_response_with_events(OPERATION, progress, cancellation, Some(&collector))?;
        if let Some(message) = collector.decode_error.lock().ok().and_then(|guard| guard.clone()) {
            return Err(LeanWorkerError::Protocol { message });
        }
        match response {
            Response::RowsComplete { count } => {
                let names = collector.into_inner();
                let observed = u64::try_from(names.len()).unwrap_or(u64::MAX);
                if observed != count {
                    return Err(LeanWorkerError::Protocol {
                        message: format!(
                            "worker_list_declarations_strings: parent collected {observed} rows but child reported \
                             {count}"
                        ),
                    });
                }
                Ok(names)
            }
            other => Err(unexpected_response(OPERATION, &other)),
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_describe_bulk(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<Vec<LeanWorkerDeclarationRow>, LeanWorkerError> {
        self.round_trip(
            "worker_describe_bulk",
            Request::DescribeBulk {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
                progress: progress.is_some(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::DeclarationBulk { rows } => Ok(rows),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_process_module_query(
        &mut self,
        source: &str,
        query: LeanWorkerModuleQuery,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryOutcome, LeanWorkerError> {
        self.round_trip(
            "worker_process_module_query",
            Request::ProcessModuleQuery {
                source: source.to_owned(),
                query,
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ProcessModuleQuery { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_process_module_query_batch(
        &mut self,
        source: &str,
        selectors: &[LeanWorkerModuleQuerySelector],
        budgets: &LeanWorkerOutputBudgets,
        options: &LeanWorkerElabOptions,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleQueryBatchOutcome, LeanWorkerError> {
        const OPERATION: &str = "worker_process_module_query_batch";
        let outcome = self.round_trip(
            OPERATION,
            Request::ProcessModuleQueryBatch {
                source: source.to_owned(),
                selectors: selectors.to_vec(),
                budgets: budgets.clone(),
                options: options.clone(),
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ProcessModuleQueryBatch { outcome } => Ok(outcome),
                other => Err(unexpected_response(operation, &other)),
            },
        );
        match outcome {
            Ok(outcome) => Ok(outcome),
            // As with verification: a child abort during a read-only proof-state
            // batch becomes a per-selector degraded item, not a hard error.
            Err(LeanWorkerError::ChildPanicOrAbort { .. }) => {
                let resource = worker_resource_facts(
                    "worker_child_abort",
                    true,
                    Some(OPERATION),
                    &self.stats,
                    self.stats.last_rss_kib,
                    None,
                    None,
                );
                Ok(degraded_query_batch_outcome(selectors, resource))
            }
            Err(err) => Err(err),
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_clear_module_snapshot_cache(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerModuleSnapshotCacheClearResult, LeanWorkerError> {
        self.round_trip(
            "worker_clear_module_snapshot_cache",
            Request::ClearModuleSnapshotCache,
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::ModuleSnapshotCacheCleared { result } => Ok(result),
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    pub(crate) fn worker_host_runtime_metadata(
        &mut self,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerHostRuntimeMetadata, LeanWorkerError> {
        const OPERATION: &str = "worker_host_runtime_metadata";
        // Session opening just established the generation. Do not recycle in
        // between its acknowledgement and this metadata-only inspection.
        check_cancelled(OPERATION, cancellation)?;
        self.send_request_cancellable(Request::HostRuntimeMetadata, OPERATION, cancellation)?;
        // This inspection is part of opening a session, not another body
        // request for max_requests/import policy. Charging it could force a
        // recycle immediately before the body whose session we just checked.
        let response = self.read_response_with_progress(OPERATION, progress, cancellation)?;
        if let Response::HostRuntimeMetadata { metadata } = response {
            Ok(metadata)
        } else {
            Err(unexpected_response(OPERATION, &response))
        }
    }

    /// Last metadata validated for this generation; absent before a checked
    /// session or after any shutdown/replacement attempt.
    #[must_use]
    pub fn host_runtime_metadata(&self) -> Option<&LeanWorkerHostRuntimeMetadata> {
        self.host_runtime_metadata.as_ref()
    }

    pub(crate) fn worker_capability_metadata(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerCapabilityMetadata, LeanWorkerError> {
        let request_json = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability metadata request JSON encode failed: {err}"),
        })?;
        self.round_trip(
            "worker_capability_metadata",
            Request::CapabilityMetadata {
                export: export.to_owned(),
                request_json,
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::CapabilityMetadata { metadata } => Ok(metadata),
                Response::CapabilityMetadataMalformed { message } => {
                    Err(LeanWorkerError::CapabilityMetadataMalformed { message })
                }
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "round_trip deliberately collapses per-method Response wildcards into a uniform unexpected_response \
                  branch; a new variant surfaces at runtime, not compile time"
    )]
    pub(crate) fn worker_capability_doctor(
        &mut self,
        export: &str,
        request: &serde_json::Value,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<LeanWorkerDoctorReport, LeanWorkerError> {
        let request_json = serde_json::to_string(request).map_err(|err| LeanWorkerError::Protocol {
            message: format!("worker capability doctor request JSON encode failed: {err}"),
        })?;
        self.round_trip(
            "worker_capability_doctor",
            Request::CapabilityDoctor {
                export: export.to_owned(),
                request_json,
            },
            false,
            cancellation,
            progress,
            |response, operation| match response {
                Response::CapabilityDoctor { report } => Ok(report),
                Response::CapabilityDoctorMalformed { message } => {
                    Err(LeanWorkerError::CapabilityDoctorMalformed { message })
                }
                other => Err(unexpected_response(operation, &other)),
            },
        )
    }

    fn prepare_host_request(
        &mut self,
        import_like: bool,
        operation: &'static str,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
    ) -> Result<(), LeanWorkerError> {
        let config = self.open_session_keys.last().cloned();
        let generation = self.stats.restarts;
        self.prepare_request(import_like, operation, cancellation)?;
        if self.stats.restarts != generation
            && let Some(config) = config
        {
            self.open_worker_session(&config, cancellation, progress)?;
        }
        Ok(())
    }

    /// Run one Request/Response round-trip and project the response into a
    /// typed value.
    ///
    /// Centralizes the cancel-check → send → record → read sequence so every
    /// `worker_*` helper above delegates here instead of repeating five
    /// identical lines plus a 22-variant wildcard arm. The `extract` closure
    /// receives the response together with the operation name; it returns the
    /// typed value, the typed error variant the protocol expects (e.g.,
    /// `*Malformed`), or `unexpected_response(operation, &other)` for any
    /// wire variant the operation never expects.
    fn round_trip<R>(
        &mut self,
        operation: &'static str,
        request: Request,
        import_like: bool,
        cancellation: Option<&LeanWorkerCancellationToken>,
        progress: Option<&dyn LeanWorkerProgressSink>,
        extract: impl FnOnce(Response, &'static str) -> Result<R, LeanWorkerError>,
    ) -> Result<R, LeanWorkerError> {
        check_cancelled(operation, cancellation)?;
        let session_config = self.open_session_keys.last().cloned();
        self.prepare_host_request(import_like, operation, cancellation, progress)?;
        self.send_request_cancellable(request.clone(), operation, cancellation)?;
        self.record_request(import_like);
        let mut response = self.read_response_with_progress(operation, progress, cancellation);
        if let (Err(error), Some(config)) = (&response, session_config.as_ref())
            && worker_session_missing(error)
        {
            // The child lost the session this request needs. Replace it, open
            // the session again and resend once; a second miss is returned.
            self.restart_for_request(
                LeanWorkerRestartReason::SessionMissing { operation },
                operation,
                cancellation,
            )?;
            self.open_worker_session(config, cancellation, progress)?;
            check_cancelled(operation, cancellation)?;
            self.send_request_cancellable(request.clone(), operation, cancellation)?;
            self.record_request(import_like);
            response = self.read_response_with_progress(operation, progress, cancellation);
        }
        let mut response = response?;
        if matches!(response, Response::HeaderImportRecycle) {
            let config = session_config.ok_or_else(|| LeanWorkerError::Protocol {
                message: "header recycle requested without an active session".to_owned(),
            })?;
            let reason = if self.budget.accounting_unknown {
                LeanWorkerRestartReason::ImportAccountingUnknown
            } else {
                LeanWorkerRestartReason::ImportResidue {
                    residue_bytes: self.budget.import_residue_bytes,
                    limit_bytes: self.config.restart_policy.max_import_residue_bytes.unwrap_or(0),
                }
            };
            self.restart_for_request(reason, operation, cancellation)?;
            self.open_worker_session(&config, cancellation, progress)?;
            // Exactly one retry: the new generation admits its first necessary
            // header even when that header alone exceeds the byte budget.
            check_cancelled(operation, cancellation)?;
            self.send_request_cancellable(request, operation, cancellation)?;
            self.record_request(import_like);
            response = self.read_response_with_progress(operation, progress, cancellation)?;
        }
        extract(response, operation)
    }
}

/// Whether the child refused a session request because it holds no session.
fn worker_session_missing(error: &LeanWorkerError) -> bool {
    matches!(error, LeanWorkerError::Worker { code, .. } if code == "lean_rs.worker.session_missing")
}

/// Synthesise a degraded module-query batch outcome after a child abort: every
/// requested selector reports `BudgetExceeded` so the caller sees an honest
/// "could not complete under resource pressure" per id rather than a hard error.
pub(in crate::supervisor) fn degraded_query_batch_outcome(
    selectors: &[LeanWorkerModuleQuerySelector],
    resource: LeanWorkerResourceExhaustedFacts,
) -> LeanWorkerModuleQueryBatchOutcome {
    let items = selectors
        .iter()
        .map(|selector| LeanWorkerModuleQueryBatchItem::BudgetExceeded {
            id: selector.id().to_owned(),
            message: "worker aborted during module query; result degraded under resource pressure".to_owned(),
        })
        .collect();
    LeanWorkerModuleQueryBatchOutcome::Ok {
        result: LeanWorkerModuleQueryBatchEnvelope {
            items,
            total_truncated: false,
        },
        imports: Vec::new(),
        facts: LeanWorkerModuleQueryCacheFacts {
            resource: Some(Box::new(resource)),
            ..LeanWorkerModuleQueryCacheFacts::uncached(0)
        },
    }
}

pub(in crate::supervisor) fn degraded_declaration_verification_batch_result(
    request: &LeanWorkerDeclarationVerificationBatchRequest,
) -> LeanWorkerDeclarationVerificationBatchResult {
    LeanWorkerDeclarationVerificationBatchResult::Ok {
        results: request
            .targets
            .iter()
            .map(|target| LeanWorkerDeclarationVerificationBatchRow {
                id: target.id.clone(),
                target: target.target.clone(),
                verification_status: LeanWorkerDeclarationVerificationStatus::BudgetExceeded,
                facts: Box::new(LeanWorkerDeclarationVerificationFacts::unavailable()),
            })
            .collect(),
        imports: Vec::new(),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Return the fixture capability's manifest path, building it only on a cache miss.
///
/// A build rewrites the manifest even when nothing changed, and a sealed test
/// deployment checks the manifest's file identity, so a ready cache is only probed.
fn fixture_capability_manifest(fixture_root: &Path) -> Result<PathBuf, LeanWorkerError> {
    let builder = || {
        lean_toolchain::CargoLeanCapability::new(fixture_root, "LeanRsFixture")
            .package("lean_rs_fixture")
            .module("LeanRsFixture")
            .export_signature(fixture_mul_signature("lean_rs_fixture_u64_mul"))
            .export_signature(fixture_panic_signature("lean_rs_fixture_panic_unit"))
    };
    let failure = |diagnostic| LeanWorkerError::CapabilityBuild { diagnostic };
    let built = match builder().probe_ready().map_err(failure)? {
        Some(built) => built,
        None => builder().build_quiet().map_err(failure)?,
    };
    Ok(built.manifest_path().to_path_buf())
}

/// Parent-side collector that decodes per-row JSON-string payloads from
/// `list_declarations_strings` into an owned `Vec<String>`.
///
/// Each name lands as its own `Message::DataRow` so the 1 MiB protocol frame
/// cap binds per-name (any single Lean name is well under that) rather than
/// per-response. Decoding failures or sink panics are surfaced through the
/// usual `LeanWorkerError::Protocol` / `DataSinkPanic` paths.
#[derive(Debug, Default)]
struct DeclarationNameCollector {
    names: Mutex<Vec<String>>,
    decode_error: Mutex<Option<String>>,
}

impl DeclarationNameCollector {
    fn into_inner(self) -> Vec<String> {
        self.names.into_inner().unwrap_or_default()
    }
}

impl LeanWorkerRawDataSink for DeclarationNameCollector {
    fn report(&self, row: LeanWorkerRawDataRow) {
        match serde_json::from_str::<String>(row.payload.get()) {
            Ok(name) => {
                if let Ok(mut guard) = self.names.lock() {
                    guard.push(name);
                }
            }
            Err(err) => {
                if let Ok(mut slot) = self.decode_error.lock()
                    && slot.is_none()
                {
                    *slot = Some(format!("list_declarations_strings row payload decode failed: {err}"));
                }
            }
        }
    }
}
