//! One-shot operation for a trusted adapter that has consumed its durable launch grant.

use super::{
    Arc, GenerationDispatchRestriction, HostSessionMode, Instant, LeanWorker, LeanWorkerCancellationToken,
    LeanWorkerDeclarationInspectionRequest, LeanWorkerDeclarationInspectionResult, LeanWorkerError,
    LeanWorkerSessionConfig, LeanWorkerShutdownReport, OwnedChild, Request, Response, ShutdownIntent,
    WorkerSupervisorState, unexpected_response,
};
use crate::crash_ownership::ManagedRequestDeadline;
use lean_rs_worker_protocol::protocol::ManagedInspectionCommand;

pub(crate) type SaveManagedInspection<'a> =
    &'a mut dyn FnMut(&LeanWorkerDeclarationInspectionResult) -> Result<(), LeanWorkerError>;

/// The already observed reply and the separately observed retirement.
#[doc(hidden)]
pub struct ManagedInspectionOutcome {
    pub result: Result<LeanWorkerDeclarationInspectionResult, LeanWorkerError>,
    pub retirement: Result<LeanWorkerShutdownReport, LeanWorkerError>,
}

impl LeanWorker {
    pub(crate) fn managed_guardian_binding(
        &self,
    ) -> Result<lean_toolchain::native_guardian::NativeGuardianDrainBinding, LeanWorkerError> {
        if self.generation_dispatch.is_some() || self.cleanup_pending() {
            return Err(LeanWorkerError::ShutdownInProgress {
                operation: "managed_guardian_binding",
            });
        }
        self.child
            .as_ref()
            .ok_or(LeanWorkerError::ShutdownInProgress {
                operation: "managed_guardian_binding",
            })?
            .managed_guardian_binding()
            .map_err(|error| LeanWorkerError::Setup {
                message: format!("managed guardian binding unavailable: {error}"),
            })
    }

    pub(crate) fn prepare_managed_artifact_read(
        &mut self,
        config: &LeanWorkerSessionConfig,
        expires_ns: u64,
    ) -> Result<lean_toolchain::native_guardian::NativeGuardianDrainBinding, LeanWorkerError> {
        const OPERATION: &str = "prepare_managed_artifact_read";
        if !self.config.shared_artifact_reads
            || self.host_read_active
            || !matches!(self.state, WorkerSupervisorState::Idle { .. })
        {
            return Err(LeanWorkerError::ShutdownInProgress { operation: OPERATION });
        }
        let expected = self.managed_guardian_binding()?;
        let original = ManagedRequestDeadline {
            attempt: [0; 16],
            stage: 0,
            expires_ns,
        };
        let result = (|| {
            let token = managed_token(&original, None)?;
            let response = self.managed_frame(
                Request::PrepareHostRead {
                    project_root: config.project_root_string(),
                },
                OPERATION,
                &token,
                expires_ns,
            )?;
            if !matches!(response, Response::HostReadPrepared) {
                return Err(unexpected_response(OPERATION, &response));
            }
            self.host_read_active = true;
            let observed = self.managed_guardian_binding()?;
            if observed != expected {
                return Err(LeanWorkerError::ShutdownInProgress { operation: OPERATION });
            }
            Ok(expected)
        })();
        if result.is_err() {
            drop(self.shutdown_child(ShutdownIntent::KillOnly));
        }
        result
    }

    pub(crate) fn inspect_admitted_prepared_declaration_once(
        &mut self,
        request: LeanWorkerDeclarationInspectionRequest,
        expected: &lean_toolchain::native_guardian::NativeGuardianDrainBinding,
        arm: &ManagedRequestDeadline,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        save: Option<SaveManagedInspection<'_>>,
    ) -> Result<ManagedInspectionOutcome, LeanWorkerError> {
        self.validate_managed_receiver(expected)?;
        let finish = save.map(|save| {
            (
                ManagedInspectionCommand {
                    generation: expected.generation.clone(),
                    attempt: arm.attempt,
                    stage: arm.stage,
                    command: 1,
                },
                save,
            )
        });
        self.inspect_declaration_managed_once_with(request, arm, Some(config), cancellation, finish, |_| {})
    }

    fn validate_managed_receiver(
        &self,
        expected: &lean_toolchain::native_guardian::NativeGuardianDrainBinding,
    ) -> Result<(), LeanWorkerError> {
        self.child
            .as_ref()
            .ok_or(LeanWorkerError::ShutdownInProgress {
                operation: "worker_inspect_declaration_managed_once",
            })?
            .validate_managed_scope(expected)
            .map_err(|error| LeanWorkerError::Setup {
                message: format!("managed receiver refused: {error}"),
            })
    }

    fn managed_frame(
        &mut self,
        request: Request,
        operation: &'static str,
        token: &LeanWorkerCancellationToken,
        expires_ns: u64,
    ) -> Result<Response, LeanWorkerError> {
        let now = Instant::now();
        self.write_request_bounded_inner(
            request,
            operation,
            Some(token),
            self.config.request_timeout,
            Some(expires_ns),
        )?;
        self.request_started = Some(now);
        self.record_request(false);
        self.read_response_with_progress(operation, None, Some(token))
    }

    fn open_managed_artifact_session(
        &mut self,
        config: &LeanWorkerSessionConfig,
        token: &LeanWorkerCancellationToken,
        expires_ns: u64,
    ) -> Result<(), LeanWorkerError> {
        const OPEN: &str = "open_managed_artifact_session";
        let project_root = config.project_root_string();
        let response = self.managed_frame(
            Request::OpenHostSession {
                project_root: project_root.clone(),
                mode: HostSessionMode::ShimsOnly,
                imports: config.imports().to_vec(),
                import_profile: config.import_profile(),
            },
            OPEN,
            token,
            expires_ns,
        )?;
        if !matches!(
            response,
            Response::HostSessionOpened { .. } | Response::HostSessionReused { .. }
        ) {
            return Err(unexpected_response(OPEN, &response));
        }
        const METADATA: &str = "managed_host_runtime_metadata";
        let response = self.managed_frame(Request::HostRuntimeMetadata, METADATA, token, expires_ns)?;
        let Response::HostRuntimeMetadata { metadata } = response else {
            return Err(unexpected_response(METADATA, &response));
        };
        if let Some(expected) = config.host_runtime_expectation() {
            expected
                .validate(&metadata)
                .map_err(|message| LeanWorkerError::Protocol { message })?;
        }
        if !metadata.artifact_read_epochs.as_ref().is_some_and(|epochs| {
            epochs
                .iter()
                .any(|epoch| epoch.project_root == project_root && !epoch.epoch.is_empty())
        }) {
            return Err(LeanWorkerError::Protocol {
                message: "worker did not report actual project artifact read custody".to_owned(),
            });
        }
        self.host_runtime_metadata = Some(metadata);
        Ok(())
    }

    /// Inspect a declaration using the binding from a trusted host's already
    /// consumed durable grant. The facts have no launch authority on their own.
    /// The caller keeps the original grant private and consumes it exactly once.
    ///
    /// # Errors
    /// Refuses a mismatched receiver or unavailable generation. After accepting
    /// the exact receiver, dispatch and retirement outcomes are recorded separately.
    #[doc(hidden)]
    pub fn inspect_admitted_declaration_once(
        &mut self,
        request: LeanWorkerDeclarationInspectionRequest,
        expected: &lean_toolchain::native_guardian::NativeGuardianDrainBinding,
        attempt: [u8; 16],
        stage: u64,
        expires_ns: u64,
    ) -> Result<ManagedInspectionOutcome, LeanWorkerError> {
        self.validate_managed_receiver(expected)?;
        self.inspect_declaration_managed_once(
            request,
            &ManagedRequestDeadline {
                attempt,
                stage,
                expires_ns,
            },
            None,
        )
    }

    /// Execute exactly one managed inspection on this already-open generation.
    /// Preserve the typed outcome and report retirement separately.
    fn inspect_declaration_managed_once(
        &mut self,
        request: LeanWorkerDeclarationInspectionRequest,
        arm: &ManagedRequestDeadline,
        prepared: Option<&LeanWorkerSessionConfig>,
    ) -> Result<ManagedInspectionOutcome, LeanWorkerError> {
        self.inspect_declaration_managed_once_with(request, arm, prepared, None, None, |_| {})
    }

    fn inspect_declaration_managed_once_with(
        &mut self,
        request: LeanWorkerDeclarationInspectionRequest,
        arm: &ManagedRequestDeadline,
        prepared: Option<&LeanWorkerSessionConfig>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        finish: Option<(ManagedInspectionCommand, SaveManagedInspection<'_>)>,
        before_retirement: impl FnOnce(&mut Self),
    ) -> Result<ManagedInspectionOutcome, LeanWorkerError> {
        const OPERATION: &str = "worker_inspect_declaration_managed_once";
        if self.generation_dispatch.is_some()
            || !matches!(self.state, WorkerSupervisorState::Idle { .. })
            || prepared.map_or_else(|| self.open_session_keys.is_empty(), |_| !self.host_read_active)
            || !self.child.as_ref().is_some_and(OwnedChild::has_guardian)
        {
            return Err(LeanWorkerError::ShutdownInProgress { operation: OPERATION });
        }
        let native_generation_id = self
            .current_native_generation_id()
            .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?
            .to_owned();
        self.generation_dispatch = Some(GenerationDispatchRestriction {
            owner: Arc::clone(&self.request_scope),
            native_generation_id,
            generation: self.generation,
            sealed: false,
        });
        let result = (|| {
            let token = managed_token(arm, cancellation)?;
            self.check_request_cancelled(OPERATION, Some(&token))?;
            let child = self
                .child
                .as_mut()
                .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?;
            let ordinal = finish.as_ref().map(|(command, _)| command.command);
            child
                .arm_managed_request(arm, ordinal)
                .map_err(|error| LeanWorkerError::Setup {
                    message: format!("managed request arm refused: {error}"),
                })?;
            if let Some(config) = prepared {
                self.open_managed_artifact_session(config, &token, arm.expires_ns)?;
                #[cfg(all(test, target_os = "macos"))]
                tests::before_prepared_inspection_dispatch(self, arm);
            }
            let now = Instant::now();
            self.write_request_bounded_inner(
                match &finish {
                    Some((command, _)) => Request::InspectDeclarationFenced {
                        command: command.clone(),
                        request,
                    },
                    None => Request::InspectDeclaration { request },
                },
                OPERATION,
                Some(&token),
                self.config.request_timeout,
                Some(arm.expires_ns),
            )?;
            self.request_started = Some(now);
            self.record_request(false);
            #[cfg(all(test, target_os = "macos"))]
            tests::after_real_dispatch(self, arm);

            let response = self.read_response_with_progress(OPERATION, None, Some(&token))?;
            let (command, save) = match finish {
                Some((command, save)) => (Some(command), Some(save)),
                None => (None, None),
            };
            let result = inspection_terminal(command, response, OPERATION)?;
            if let (Some(ordinal), Some(save)) = (ordinal, save) {
                self.check_request_cancelled(OPERATION, Some(&token))?;
                // This development route always retires. Seal before invoking
                // the caller so even an unwinding save cannot reopen dispatch.
                self.seal_generation_dispatch_internal();
                save(&result)?;
                self.check_request_cancelled(OPERATION, Some(&token))?;
                self.child
                    .as_mut()
                    .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?
                    .finish_managed_request(arm, ordinal)
                    .map_err(|error| LeanWorkerError::Setup {
                        message: format!("managed request finish unconfirmed: {error}"),
                    })?;
            }
            Ok(result)
        })();
        before_retirement(self);
        self.seal_generation_dispatch_internal();
        let retirement = self.shutdown_child(ShutdownIntent::KillOnly);
        Ok(ManagedInspectionOutcome { result, retirement })
    }
}

fn inspection_terminal(
    expected: Option<ManagedInspectionCommand>,
    response: Response,
    operation: &'static str,
) -> Result<LeanWorkerDeclarationInspectionResult, LeanWorkerError> {
    match (expected, response) {
        (None, Response::DeclarationInspection { result }) => Ok(result),
        (Some(expected), Response::DeclarationInspectionFinished { command, result }) if expected == command => {
            Ok(*result)
        }
        (_, response) => Err(unexpected_response(operation, &response)),
    }
}

#[test]
fn inspection_terminal_requires_the_exact_fence() {
    let binding = ManagedInspectionCommand {
        generation: "original".to_owned(),
        attempt: [7; 16],
        stage: 3,
        command: 1,
    };
    let result = LeanWorkerDeclarationInspectionResult::Unsupported;
    let fenced = |command| Response::DeclarationInspectionFinished {
        command,
        result: Box::new(result.clone()),
    };
    let ordinary = || Response::DeclarationInspection { result: result.clone() };
    assert!(inspection_terminal(Some(binding.clone()), fenced(binding.clone()), "test").is_ok());
    assert!(inspection_terminal(None, ordinary(), "test").is_ok());
    assert!(inspection_terminal(Some(binding.clone()), ordinary(), "test").is_err());
    for wrong in [
        ManagedInspectionCommand {
            generation: "other".to_owned(),
            ..binding.clone()
        },
        ManagedInspectionCommand {
            attempt: [8; 16],
            ..binding.clone()
        },
        ManagedInspectionCommand {
            stage: 4,
            ..binding.clone()
        },
        ManagedInspectionCommand {
            command: 2,
            ..binding.clone()
        },
    ] {
        assert!(inspection_terminal(Some(binding.clone()), fenced(wrong), "test").is_err());
    }
}

fn managed_token(
    arm: &ManagedRequestDeadline,
    cancellation: Option<&LeanWorkerCancellationToken>,
) -> Result<LeanWorkerCancellationToken, LeanWorkerError> {
    let now = Instant::now();
    let remaining = arm.remaining().map_err(|error| LeanWorkerError::Setup {
        message: error.to_string(),
    })?;
    let end = now.checked_add(remaining).ok_or_else(|| LeanWorkerError::Setup {
        message: "managed request deadline exceeds local clock".to_owned(),
    })?;
    Ok(cancellation.cloned().unwrap_or_default().with_deadline(end))
}

#[cfg(all(test, target_os = "macos"))]
#[path = "managed_once_tests.rs"]
mod tests;
