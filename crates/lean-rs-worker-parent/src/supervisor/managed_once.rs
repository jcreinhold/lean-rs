//! One-shot operation for a trusted adapter that has consumed its durable launch grant.

use super::{
    Arc, GenerationDispatchRestriction, Instant, LeanWorker, LeanWorkerCancellationToken,
    LeanWorkerDeclarationInspectionRequest, LeanWorkerDeclarationInspectionResult, LeanWorkerError,
    LeanWorkerShutdownReport, OwnedChild, Request, Response, ShutdownIntent, WorkerSupervisorState,
    unexpected_response,
};
use crate::crash_ownership::ManagedRequestDeadline;

/// The already observed reply and the separately observed retirement.
#[doc(hidden)]
pub struct ManagedInspectionOutcome {
    pub result: Result<LeanWorkerDeclarationInspectionResult, LeanWorkerError>,
    pub retirement: Result<LeanWorkerShutdownReport, LeanWorkerError>,
}

impl LeanWorker {
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
        self.child
            .as_ref()
            .ok_or(LeanWorkerError::ShutdownInProgress {
                operation: "worker_inspect_declaration_managed_once",
            })?
            .validate_managed_scope(expected)
            .map_err(|error| LeanWorkerError::Setup {
                message: format!("managed receiver refused: {error}"),
            })?;
        self.inspect_declaration_managed_once(
            request,
            &ManagedRequestDeadline {
                attempt,
                stage,
                expires_ns,
            },
        )
    }

    /// Execute exactly one managed inspection on this already-open generation.
    /// Preserve the typed outcome and report retirement separately.
    fn inspect_declaration_managed_once(
        &mut self,
        request: LeanWorkerDeclarationInspectionRequest,
        arm: &ManagedRequestDeadline,
    ) -> Result<ManagedInspectionOutcome, LeanWorkerError> {
        const OPERATION: &str = "worker_inspect_declaration_managed_once";
        if self.generation_dispatch.is_some()
            || !matches!(self.state, WorkerSupervisorState::Idle { .. })
            || self.open_session_keys.is_empty()
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
            let child = self
                .child
                .as_mut()
                .ok_or(LeanWorkerError::ShutdownInProgress { operation: OPERATION })?;
            child.arm_managed_request(arm).map_err(|error| LeanWorkerError::Setup {
                message: format!("managed request arm refused: {error}"),
            })?;
            let now = Instant::now();
            let remaining = arm.remaining().map_err(|error| LeanWorkerError::Setup {
                message: error.to_string(),
            })?;
            let end = now.checked_add(remaining).ok_or_else(|| LeanWorkerError::Setup {
                message: "managed request deadline exceeds local clock".to_owned(),
            })?;
            let token = LeanWorkerCancellationToken::new().with_deadline(end);
            self.write_request_bounded_inner(
                Request::InspectDeclaration { request },
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
            if let Response::DeclarationInspection { result } = response {
                Ok(result)
            } else {
                Err(unexpected_response(OPERATION, &response))
            }
        })();
        self.seal_generation_dispatch_internal();
        let retirement = self.shutdown_child(ShutdownIntent::KillOnly);
        Ok(ManagedInspectionOutcome { result, retirement })
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "managed_once_tests.rs"]
mod tests;
