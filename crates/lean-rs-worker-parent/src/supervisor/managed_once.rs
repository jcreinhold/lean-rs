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

/// A managed command leaves its exact generation reusable only after durable end.
#[doc(hidden)]
#[allow(
    clippy::large_enum_variant,
    reason = "keep the existing one-shot retirement report inline"
)]
pub enum ManagedWarmInspectionOutcome {
    /// Exact finish and durable end were confirmed on the current owner.
    Reusable(LeanWorkerDeclarationInspectionResult),
    /// Retirement was requested. Inspect its separate result for closure.
    Retired(ManagedInspectionOutcome),
}

pub(crate) struct ManagedFinish<'a> {
    pub(crate) command: ManagedInspectionCommand,
    pub(crate) save: SaveManagedInspection<'a>,
    pub(crate) end: Option<&'a mut dyn FnMut() -> Result<(), LeanWorkerError>>,
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

    /// Read-only continuation check. The mutable dispatch still consumes the
    /// next separately admitted command and repeats the native binding checks.
    pub(crate) fn validate_managed_warm_continuation(
        &self,
        expected: &lean_toolchain::native_guardian::NativeGuardianDrainBinding,
        predecessor: &ManagedInspectionCommand,
    ) -> Result<(), LeanWorkerError> {
        if self.completed_warm_command() != Some(predecessor)
            || predecessor.generation != expected.generation
            || !matches!(self.state, WorkerSupervisorState::Idle { .. })
            || !self.host_read_active
        {
            return Err(LeanWorkerError::ShutdownInProgress {
                operation: "validate_managed_warm_continuation",
            });
        }
        self.validate_managed_receiver(expected)
    }

    fn completed_warm_command(&self) -> Option<&ManagedInspectionCommand> {
        let pin = self.generation_dispatch.as_ref()?;
        if !pin.sealed
            || !Arc::ptr_eq(&pin.owner, &self.request_scope)
            || pin.generation != self.generation
            || self.current_native_generation_id() != Some(pin.native_generation_id.as_str())
        {
            return None;
        }
        pin.warm_end.as_ref()
    }

    pub(crate) fn prepare_managed_artifact_read(
        &mut self,
        config: &LeanWorkerSessionConfig,
        expires_ns: u64,
        admitted: Option<(&str, [u8; 16], u64)>,
    ) -> Result<lean_toolchain::native_guardian::NativeGuardianDrainBinding, LeanWorkerError> {
        const OPERATION: &str = "prepare_managed_artifact_read";
        if !self.config.shared_artifact_reads
            || self.host_read_active
            || !matches!(self.state, WorkerSupervisorState::Idle { .. })
        {
            return Err(LeanWorkerError::ShutdownInProgress { operation: OPERATION });
        }
        let expected = self.managed_guardian_binding()?;
        if admitted.is_some_and(|(boot, _, _)| !boot.eq_ignore_ascii_case(&expected.boot)) {
            return Err(LeanWorkerError::Setup {
                message: "managed preparation boot differs".to_owned(),
            });
        }
        let original = ManagedRequestDeadline {
            attempt: admitted.map_or([0; 16], |(_, attempt, _)| attempt),
            stage: admitted.map_or(0, |(_, _, stage)| stage),
            expires_ns,
        };
        let result = (|| {
            let token = managed_token(&original, None)?;
            let project_root = config.project_root_string();
            let request = if admitted.is_some() {
                let child = self
                    .child
                    .as_mut()
                    .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?;
                child
                    .prepare_managed_request(&original)
                    .map_err(|error| LeanWorkerError::Setup {
                        message: format!("managed guardian preparation refused: {error}"),
                    })?;
                Request::PrepareBoundHostRead { project_root }
            } else {
                Request::PrepareHostRead { project_root }
            };
            let response = self.managed_frame(request, OPERATION, &token, expires_ns)?;
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
        let finish = save.map(|save| ManagedFinish {
            command: ManagedInspectionCommand {
                generation: expected.generation.clone(),
                attempt: arm.attempt,
                stage: arm.stage,
                command: 1,
            },
            save,
            end: None,
        });
        self.inspect_declaration_managed_once_with(request, arm, Some(config), cancellation, finish, |_| {})
    }

    pub(crate) fn inspect_admitted_prepared_declaration_warm(
        &mut self,
        request: LeanWorkerDeclarationInspectionRequest,
        expected: &lean_toolchain::native_guardian::NativeGuardianDrainBinding,
        arm: &ManagedRequestDeadline,
        config: &LeanWorkerSessionConfig,
        cancellation: Option<&LeanWorkerCancellationToken>,
        finish: ManagedFinish<'_>,
    ) -> Result<ManagedWarmInspectionOutcome, LeanWorkerError> {
        self.validate_managed_receiver(expected)?;
        if finish.command.generation != expected.generation {
            return Err(LeanWorkerError::Protocol {
                message: "managed grant generation differs".to_owned(),
            });
        }
        self.inspect_declaration_managed(request, arm, Some(config), cancellation, Some(finish), |_| {})
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
        finish: Option<ManagedFinish<'_>>,
        before_retirement: impl FnOnce(&mut Self),
    ) -> Result<ManagedInspectionOutcome, LeanWorkerError> {
        match self.inspect_declaration_managed(request, arm, prepared, cancellation, finish, before_retirement)? {
            ManagedWarmInspectionOutcome::Retired(outcome) => Ok(outcome),
            ManagedWarmInspectionOutcome::Reusable(_) => Err(LeanWorkerError::Protocol {
                message: "one-shot inspection unexpectedly retained its generation".to_owned(),
            }),
        }
    }

    fn inspect_declaration_managed(
        &mut self,
        request: LeanWorkerDeclarationInspectionRequest,
        arm: &ManagedRequestDeadline,
        prepared: Option<&LeanWorkerSessionConfig>,
        cancellation: Option<&LeanWorkerCancellationToken>,
        finish: Option<ManagedFinish<'_>>,
        before_retirement: impl FnOnce(&mut Self),
    ) -> Result<ManagedWarmInspectionOutcome, LeanWorkerError> {
        const OPERATION: &str = "worker_inspect_declaration_managed_once";
        let warm = finish.as_ref().filter(|finish| finish.end.is_some());
        let admitted = if self.generation_dispatch.is_some() {
            warm.zip(self.completed_warm_command()).is_some_and(|(next, prior)| {
                next.command.generation == prior.generation
                    && next.command.command > prior.command
                    && (next.command.attempt, next.command.stage) != (prior.attempt, prior.stage)
            })
        } else {
            warm.is_none_or(|first| first.command.command == 1)
        };
        if !admitted
            || !matches!(self.state, WorkerSupervisorState::Idle { .. })
            || prepared.map_or_else(|| self.open_session_keys.is_empty(), |_| !self.host_read_active)
            || !self.child.as_ref().is_some_and(OwnedChild::has_guardian)
        {
            return Err(LeanWorkerError::ShutdownInProgress { operation: OPERATION });
        }
        let resuming = self.generation_dispatch.is_some();
        let native_generation_id = self
            .current_native_generation_id()
            .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?
            .to_owned();
        self.generation_dispatch = Some(GenerationDispatchRestriction {
            owner: Arc::clone(&self.request_scope),
            native_generation_id,
            generation: self.generation,
            sealed: false,
            warm_end: None,
        });
        let warm_command = warm.map(|finish| finish.command.clone());
        let result = (|| {
            let token = managed_token(arm, cancellation)?;
            self.check_request_cancelled(OPERATION, Some(&token))?;
            let child = self
                .child
                .as_mut()
                .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?;
            let ordinal = finish.as_ref().map(|finish| finish.command.command);
            let armed = if let Some(command) = &warm_command {
                child.arm_warm_request(arm, command.command)
            } else {
                child.arm_managed_request(arm, ordinal)
            };
            armed.map_err(|error| LeanWorkerError::Setup {
                message: format!("managed request arm refused: {error}"),
            })?;
            #[cfg(all(test, target_os = "macos"))]
            if let Some(command) = &warm_command {
                tests::refuse_stale_warm_finish(child, command);
            }
            if let Some(config) = prepared.filter(|_| !resuming) {
                self.open_managed_artifact_session(config, &token, arm.expires_ns)?;
                #[cfg(all(test, target_os = "macos"))]
                tests::before_prepared_inspection_dispatch(self, arm);
            }
            let now = Instant::now();
            self.write_request_bounded_inner(
                match &finish {
                    Some(finish) => Request::InspectDeclarationFenced {
                        command: finish.command.clone(),
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
            let expected = finish.as_ref().map(|finish| finish.command.clone());
            let result = inspection_terminal(expected, response, OPERATION)?;
            if let Some(finish) = finish {
                self.check_request_cancelled(OPERATION, Some(&token))?;
                // Seal before either callback, including on unwind.
                self.seal_generation_dispatch_internal();
                (finish.save)(&result)?;
                self.check_request_cancelled(OPERATION, Some(&token))?;
                self.child
                    .as_mut()
                    .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?
                    .finish_managed_request(arm, finish.command.command)
                    .map_err(|error| LeanWorkerError::Setup {
                        message: format!("managed request finish unconfirmed: {error}"),
                    })?;
                if let Some(end) = finish.end {
                    end()?;
                }
            }
            Ok(result)
        })();
        if let Some(command) = warm_command
            && result.is_ok()
            && let Some(pin) = &mut self.generation_dispatch
        {
            pin.warm_end = Some(command);
            return result.map(ManagedWarmInspectionOutcome::Reusable);
        }
        before_retirement(self);
        self.seal_generation_dispatch_internal();
        let retirement = self.shutdown_child(ShutdownIntent::KillOnly);
        Ok(ManagedWarmInspectionOutcome::Retired(ManagedInspectionOutcome {
            result,
            retirement,
        }))
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
