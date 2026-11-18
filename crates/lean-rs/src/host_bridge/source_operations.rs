//! Closed source requests and the native calls admitted by their attestations.
use super::module_requests::{
    DeclarationVerificationBatchRequest, DeclarationVerificationRequest, ModuleQuery, ModuleQueryOutputBudgets,
    ModuleQuerySelector, ProofAttemptRequest,
};
use super::{HostEnvironment, HostShimBindings, HostSourceOptions, SourceContext, bind};
use crate::abi::traits::IntoLean;
use crate::module::{LeanCheckedExportError, LeanExported, LeanIo, TrustedHostCapability, TrustedHostImports};
use crate::{LeanResult, LeanRuntime, Obj};

/// Exact module source and its single projection.
pub struct HostModuleQuery {
    /// Source bytes interpreted as UTF-8 by Lean.
    pub source: String,
    /// Projection requested after elaboration.
    pub query: ModuleQuery,
}
/// Ordered projections over one module source.
#[derive(Clone)]
pub struct HostModuleBatch {
    /// Module source.
    pub source: String,
    /// Ordered selectors.
    pub selectors: Vec<ModuleQuerySelector>,
    /// Projection output limits.
    pub budgets: ModuleQueryOutputBudgets,
}
/// Cached module request including the exact policy passed to the shim.
pub struct HostCachedModuleBatch {
    /// Source and projections.
    pub batch: HostModuleBatch,
    /// Complete serialized snapshot-cache policy.
    pub policy: String,
}

/// Immutable request, operation, environment and path resolution context.
pub struct TrustedModuleSource<'lean, 'cap, R> {
    context: SourceContext<'lean, 'cap>,
    request: R,
}

fn encode_selectors<'lean>(selectors: &[ModuleQuerySelector], runtime: &'lean LeanRuntime) -> Vec<Obj<'lean>> {
    selectors.iter().cloned().map(|s| s.into_lean(runtime)).collect()
}

pub(super) struct ModuleOperations<'lean, 'cap> {
    process_module_query: Option<
        LeanExported<
            'lean,
            'cap,
            (
                HostEnvironment<'lean>,
                String,
                Obj<'lean>,
                String,
                String,
                u64,
                usize,
                String,
            ),
            LeanIo<Obj<'lean>>,
        >,
    >,
    process_module_query_batch: Option<
        LeanExported<
            'lean,
            'cap,
            (
                HostEnvironment<'lean>,
                String,
                Vec<Obj<'lean>>,
                Obj<'lean>,
                String,
                String,
                u64,
                usize,
                String,
            ),
            LeanIo<Obj<'lean>>,
        >,
    >,
    process_module_query_batch_cached: Option<
        LeanExported<
            'lean,
            'cap,
            (
                HostEnvironment<'lean>,
                String,
                Vec<Obj<'lean>>,
                Obj<'lean>,
                String,
                String,
                u64,
                usize,
                String,
                String,
            ),
            LeanIo<Obj<'lean>>,
        >,
    >,
    attempt_proof: Option<
        LeanExported<
            'lean,
            'cap,
            (HostEnvironment<'lean>, Obj<'lean>, String, String, u64, usize, String),
            LeanIo<Obj<'lean>>,
        >,
    >,
    verify_declaration: Option<
        LeanExported<
            'lean,
            'cap,
            (HostEnvironment<'lean>, Obj<'lean>, String, String, u64, usize, String),
            LeanIo<Obj<'lean>>,
        >,
    >,
    verify_declaration_batch: Option<
        LeanExported<
            'lean,
            'cap,
            (HostEnvironment<'lean>, Obj<'lean>, String, String, u64, usize, String),
            LeanIo<Obj<'lean>>,
        >,
    >,
}
impl<'lean, 'cap> ModuleOperations<'lean, 'cap> {
    pub(super) fn resolve(
        capability: &'cap TrustedHostCapability<'lean>,
        receipts: &mut super::Receipts,
    ) -> Result<Self, LeanCheckedExportError> {
        macro_rules! optional {
            ($symbol:literal) => {{
                #[allow(unsafe_code, reason = "fixed bundled module operation signatures")]
                // SAFETY: each literal is paired with its private field's complete native request and output contract.
                match unsafe { bind(capability, receipts, $symbol) } {
                    Ok(binding) => Some(binding),
                    Err(LeanCheckedExportError::MissingSymbol { .. }) => None,
                    Err(error) => return Err(error),
                }
            }};
        }
        Ok(Self {
            process_module_query: optional!("lean_rs_host_process_module_query"),
            process_module_query_batch: optional!("lean_rs_host_process_module_query_batch"),
            process_module_query_batch_cached: optional!("lean_rs_host_process_module_query_batch_cached"),
            attempt_proof: optional!("lean_rs_host_attempt_proof"),
            verify_declaration: optional!("lean_rs_host_verify_declaration"),
            verify_declaration_batch: optional!("lean_rs_host_verify_declaration_batch"),
        })
    }
}
macro_rules! source_scope {
    ($method:ident, $request:ty, $description:literal) => {
        #[doc = $description]
        ///
        /// # Safety
        ///
        /// Attest the actual native dependencies, process-global cache contents reachable by this operation,
        /// environment provenance and all resolution roots as required by [`Self::term_scope`]. The
        /// assertion covers header processing, commands, plugins, generated proof overlays and verification
        /// expressions selected by the complete immutable request. Maintain custody throughout its use.
        ///
        /// # Errors
        ///
        /// Rejects another capability's import scope or relative resolved roots.
        #[allow(
            unsafe_code,
            reason = "application attests exact source-selected native execution"
        )]
        pub unsafe fn $method<'cap>(
            &'cap self,
            env: &HostEnvironment<'lean>,
            imports: &TrustedHostImports<'lean, '_>,
            paths: Vec<String>,
            request: $request,
            options: HostSourceOptions,
        ) -> LeanResult<TrustedModuleSource<'lean, 'cap, $request>> {
            Ok(TrustedModuleSource {
                context: self.source_context(env, imports, paths, options)?,
                request,
            })
        }
    };
}
impl<'lean> TrustedHostCapability<'lean> {
    source_scope!(
        module_query_scope,
        HostModuleQuery,
        "Attest the complete request for `lean_rs_host_process_module_query`."
    );
    source_scope!(
        module_batch_scope,
        HostModuleBatch,
        "Attest the complete request for `lean_rs_host_process_module_query_batch`."
    );
    source_scope!(
        cached_module_batch_scope,
        HostCachedModuleBatch,
        "Attest the complete request for `lean_rs_host_process_module_query_batch_cached`."
    );
    source_scope!(
        proof_attempt_scope,
        ProofAttemptRequest,
        "Attest the complete request for `lean_rs_host_attempt_proof`."
    );
    source_scope!(
        verification_scope,
        DeclarationVerificationRequest,
        "Attest the complete request for `lean_rs_host_verify_declaration`."
    );
    source_scope!(
        verification_batch_scope,
        DeclarationVerificationBatchRequest,
        "Attest the complete request for `lean_rs_host_verify_declaration_batch`."
    );
}
impl<'lean> HostShimBindings<'lean, '_> {
    /// Dispatch the exact attested `process_module_query` request.
    ///
    /// # Errors
    ///
    /// Returns scope, native IO, or conversion errors.
    #[allow(
        unsafe_code,
        reason = "captured source and fixed request codecs establish every native slot"
    )]
    pub fn process_module_query(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        scope: &TrustedModuleSource<'lean, '_, HostModuleQuery>,
    ) -> LeanResult<Option<Obj<'lean>>> {
        let Some(call) = &self.module_sources.process_module_query else {
            return Ok(None);
        };
        self.with_source(env, imports, &scope.context, |env, o| {
            let r = &scope.request;
            let rt = env.obj.runtime();
            // SAFETY: only the fixed owned request DTO can encode the objects supplied to this export.
            // The captured source, complete options and search paths are admitted together.
            unsafe {
                call.call(
                    env.clone(),
                    r.source.clone(),
                    r.query.clone().into_lean(rt),
                    o.namespace.clone(),
                    o.file.clone(),
                    o.heartbeats,
                    o.diagnostic_bytes,
                    o.options_json.clone(),
                )
            }
            .map(Some)
        })
    }
    /// Dispatch the exact attested `process_module_query_batch` request.
    ///
    /// # Errors
    ///
    /// Returns scope, native IO, or conversion errors.
    #[allow(
        unsafe_code,
        reason = "captured source and fixed request codecs establish every native slot"
    )]
    pub fn process_module_query_batch(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        scope: &TrustedModuleSource<'lean, '_, HostModuleBatch>,
    ) -> LeanResult<Option<Obj<'lean>>> {
        let Some(call) = &self.module_sources.process_module_query_batch else {
            return Ok(None);
        };
        self.with_source(env, imports, &scope.context, |env, o| {
            let r = &scope.request;
            let rt = env.obj.runtime();
            // SAFETY: only the fixed owned request DTO can encode the objects supplied to this export.
            // The captured source, complete options and search paths are admitted together.
            unsafe {
                call.call(
                    env.clone(),
                    r.source.clone(),
                    encode_selectors(&r.selectors, rt),
                    r.budgets.clone().into_lean(rt),
                    o.namespace.clone(),
                    o.file.clone(),
                    o.heartbeats,
                    o.diagnostic_bytes,
                    o.options_json.clone(),
                )
            }
            .map(Some)
        })
    }
    /// Dispatch the exact attested `process_module_query_batch_cached` request.
    ///
    /// # Errors
    ///
    /// Returns scope, native IO, or conversion errors.
    #[allow(
        unsafe_code,
        reason = "captured source and fixed request codecs establish every native slot"
    )]
    pub fn process_module_query_batch_cached(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        scope: &TrustedModuleSource<'lean, '_, HostCachedModuleBatch>,
    ) -> LeanResult<Option<Obj<'lean>>> {
        let Some(call) = &self.module_sources.process_module_query_batch_cached else {
            return Ok(None);
        };
        self.with_source(env, imports, &scope.context, |env, o| {
            let r = &scope.request;
            let rt = env.obj.runtime();
            // SAFETY: only the fixed owned request DTO can encode the objects supplied to this export.
            // The captured source, complete options and search paths are admitted together.
            unsafe {
                call.call(
                    env.clone(),
                    r.batch.source.clone(),
                    encode_selectors(&r.batch.selectors, rt),
                    r.batch.budgets.clone().into_lean(rt),
                    o.namespace.clone(),
                    o.file.clone(),
                    o.heartbeats,
                    o.diagnostic_bytes,
                    o.options_json.clone(),
                    r.policy.clone(),
                )
            }
            .map(Some)
        })
    }
    /// Dispatch the exact attested `attempt_proof` request.
    ///
    /// # Errors
    ///
    /// Returns scope, native IO, or conversion errors.
    #[allow(
        unsafe_code,
        reason = "captured source and fixed request codecs establish every native slot"
    )]
    pub fn attempt_proof(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        scope: &TrustedModuleSource<'lean, '_, ProofAttemptRequest>,
    ) -> LeanResult<Option<Obj<'lean>>> {
        let Some(call) = &self.module_sources.attempt_proof else {
            return Ok(None);
        };
        self.with_source(env, imports, &scope.context, |env, o| {
            let r = &scope.request;
            let rt = env.obj.runtime();
            // SAFETY: only the fixed owned request DTO can encode the objects supplied to this export.
            // The captured source, complete options and search paths are admitted together.
            unsafe {
                call.call(
                    env.clone(),
                    r.clone().into_lean(rt),
                    o.namespace.clone(),
                    o.file.clone(),
                    o.heartbeats,
                    o.diagnostic_bytes,
                    o.options_json.clone(),
                )
            }
            .map(Some)
        })
    }
    /// Dispatch the exact attested `verify_declaration` request.
    ///
    /// # Errors
    ///
    /// Returns scope, native IO, or conversion errors.
    #[allow(
        unsafe_code,
        reason = "captured source and fixed request codecs establish every native slot"
    )]
    pub fn verify_declaration(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        scope: &TrustedModuleSource<'lean, '_, DeclarationVerificationRequest>,
    ) -> LeanResult<Option<Obj<'lean>>> {
        let Some(call) = &self.module_sources.verify_declaration else {
            return Ok(None);
        };
        self.with_source(env, imports, &scope.context, |env, o| {
            let r = &scope.request;
            let rt = env.obj.runtime();
            // SAFETY: only the fixed owned request DTO can encode the objects supplied to this export.
            // The captured source, complete options and search paths are admitted together.
            unsafe {
                call.call(
                    env.clone(),
                    r.clone().into_lean(rt),
                    o.namespace.clone(),
                    o.file.clone(),
                    o.heartbeats,
                    o.diagnostic_bytes,
                    o.options_json.clone(),
                )
            }
            .map(Some)
        })
    }
    /// Dispatch the exact attested `verify_declaration_batch` request.
    ///
    /// # Errors
    ///
    /// Returns scope, native IO, or conversion errors.
    #[allow(
        unsafe_code,
        reason = "captured source and fixed request codecs establish every native slot"
    )]
    pub fn verify_declaration_batch(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        scope: &TrustedModuleSource<'lean, '_, DeclarationVerificationBatchRequest>,
    ) -> LeanResult<Option<Obj<'lean>>> {
        let Some(call) = &self.module_sources.verify_declaration_batch else {
            return Ok(None);
        };
        self.with_source(env, imports, &scope.context, |env, o| {
            let r = &scope.request;
            let rt = env.obj.runtime();
            // SAFETY: only the fixed owned request DTO can encode the objects supplied to this export.
            // The captured source, complete options and search paths are admitted together.
            unsafe {
                call.call(
                    env.clone(),
                    r.clone().into_lean(rt),
                    o.namespace.clone(),
                    o.file.clone(),
                    o.heartbeats,
                    o.diagnostic_bytes,
                    o.options_json.clone(),
                )
            }
            .map(Some)
        })
    }
}
impl TrustedModuleSource<'_, '_, HostModuleQuery> {
    /// Item count for host dispatch statistics.
    pub fn item_count(&self) -> usize {
        0
    }
}
impl TrustedModuleSource<'_, '_, HostModuleBatch> {
    /// Item count for host dispatch statistics.
    pub fn item_count(&self) -> usize {
        self.request.selectors.len()
    }
}
impl TrustedModuleSource<'_, '_, HostCachedModuleBatch> {
    /// Item count for host dispatch statistics.
    pub fn item_count(&self) -> usize {
        self.request.batch.selectors.len()
    }
}
impl TrustedModuleSource<'_, '_, ProofAttemptRequest> {
    /// Item count for host dispatch statistics.
    pub fn item_count(&self) -> usize {
        self.request.candidates.len()
    }
}
impl TrustedModuleSource<'_, '_, DeclarationVerificationRequest> {
    /// Item count for host dispatch statistics.
    pub fn item_count(&self) -> usize {
        0
    }
}
impl TrustedModuleSource<'_, '_, DeclarationVerificationBatchRequest> {
    /// Item count for host dispatch statistics.
    pub fn item_count(&self) -> usize {
        self.request.targets.len()
    }
}
