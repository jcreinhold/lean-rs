//! Concrete safe operations for an explicitly attested bundled host capability.
//!
//! Native symbol binding and typed object reconstruction stay here. Callers
//! supply Rust values or opaque handles; they cannot supply an arbitrary object
//! as an environment, name, expression, declaration, or evidence value.

use core::fmt;

use crate::abi::traits::{LeanAbi, LeanBoxedAbi};
use crate::module::{
    DecodeCallResult, LeanArgs, LeanCheckedExportError, LeanExported, LeanIo, TrustedHostCapability, TrustedHostImports,
};
use crate::{LeanDeclaration, LeanExpr, LeanName, LeanResult, LeanRuntime, Obj};

mod declaration_filter;
pub mod declaration_requests;
pub mod module_requests;
mod source_operations;
pub use declaration_filter::LeanDeclarationFilter;
pub use source_operations::{HostCachedModuleBatch, HostModuleBatch, HostModuleQuery, TrustedModuleSource};
mod import_transaction;
pub use import_transaction::{HostImportTransaction, with_host_import_transaction};

macro_rules! opaque_host_handle {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone)]
        pub struct $name<'lean> {
            obj: Obj<'lean>,
        }

        impl fmt::Debug for $name<'_> {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }

        // SAFETY: only native typed reconstruction can create this handle.
        // Encoding transfers its existing reference; decoding requires the
        // caller-established semantic type and retains the object's lifetime.
        #[allow(unsafe_code, reason = "opaque host objects require a native type assertion")]
        unsafe impl<'lean> LeanBoxedAbi<'lean> for $name<'lean> {
            fn into_boxed(self, _runtime: &'lean LeanRuntime) -> Obj<'lean> {
                self.obj
            }

            unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
                Ok(Self { obj })
            }
        }

        // SAFETY: this host type uses an owned lean_object* at the native ABI;
        // both directions transfer one reference without changing the type.
        #[allow(unsafe_code, reason = "native ABI for an opaque attested host type")]
        unsafe impl<'lean> LeanAbi<'lean> for $name<'lean> {
            type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

            fn into_c(self, _runtime: &'lean LeanRuntime) -> Self::CRepr {
                self.obj.into_raw()
            }

            unsafe fn from_c(value: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
                // SAFETY: the caller established this opaque type, runtime,
                // initialized layout, and ownership of the native result.
                let obj = unsafe { Obj::from_c(value, runtime) }?;
                Ok(Self { obj })
            }
        }
    };
}

opaque_host_handle!(
    HostEnvironment,
    "Opaque imported Lean.Environment; no safe constructor accepts Obj."
);
opaque_host_handle!(
    HostEvidence,
    "Opaque kernel-produced host Evidence; no safe constructor accepts Obj."
);

// Each declaration below pairs a fixed Environment.lean export with its complete
// argument and result types. Both the cache and the callable method are generated
// from that same row so a caller cannot substitute its own signature.
macro_rules! declaration_queries {
    ($($name:ident($arg:ident: $input:ty) -> $output:ty = $symbol:literal;)+) => {
        struct DeclarationQueries<'lean, 'cap> {
            $($name: LeanExported<'lean, 'cap, (HostEnvironment<'lean>, $input), LeanIo<$output>>,)+
        }

        impl<'lean, 'cap> DeclarationQueries<'lean, 'cap> {
            fn resolve(capability: &'cap TrustedHostCapability<'lean>, receipts: &mut Receipts) -> Result<Self, LeanCheckedExportError> {
                #[allow(unsafe_code, reason = "fixed Environment.lean contracts on the attested image")]
                // SAFETY: each declaration query row matches the named bundled export including
                // its owned Environment, Name or array input and the complete IO result type.
                unsafe { Ok(Self { $($name: bind(capability, receipts, $symbol)?,)+ }) }
            }
        }

        impl<'lean> HostShimBindings<'lean, '_> {
            $(
                #[doc = concat!("Invoke the bundled declaration query `", $symbol, "`.")]
                ///
                /// # Errors
                ///
                /// Propagates native IO and result conversion errors.
                #[allow(unsafe_code, reason = "typed handles and native codecs enforce the fixed query contract")]
                pub fn $name(&self, environment: &HostEnvironment<'lean>, $arg: $input) -> LeanResult<$output> {
                    // SAFETY: the private binding and the row establish the complete native signature.
                    // Cloning supplies the environment reference consumed by this synchronous call.
                    unsafe { self.queries.$name.call(environment.clone(), $arg) }
                }
            )+
        }
    };
}

declaration_queries! {
    list_declarations_filtered(filter: LeanDeclarationFilter) -> Vec<LeanName<'lean>> = "lean_rs_host_env_list_declarations_filtered";
    query_declaration(name: LeanName<'lean>) -> Option<LeanDeclaration<'lean>> = "lean_rs_host_env_query_declaration";
    query_declarations_bulk(names: Vec<LeanName<'lean>>) -> Vec<Option<LeanDeclaration<'lean>>> = "lean_rs_host_env_query_declarations_bulk";
    declaration_type(name: LeanName<'lean>) -> Option<LeanExpr<'lean>> = "lean_rs_host_env_declaration_type";
    declaration_type_bulk(names: Vec<String>) -> Vec<Option<LeanExpr<'lean>>> = "lean_rs_host_env_declaration_type_bulk";
    declaration_kind(name: LeanName<'lean>) -> String = "lean_rs_host_env_declaration_kind";
    declaration_kind_bulk(names: Vec<String>) -> Vec<String> = "lean_rs_host_env_declaration_kind_bulk";
    declaration_name(name: LeanName<'lean>) -> String = "lean_rs_host_env_declaration_name";
    declaration_name_bulk(names: Vec<String>) -> Vec<String> = "lean_rs_host_env_declaration_name_bulk";
}

macro_rules! progress_queries {
    ($($name:ident($arg:ident: $input:ty) -> $output:ty = $symbol:literal;)+) => {
        struct ProgressQueries<'lean, 'cap> {
            $($name: LeanExported<'lean, 'cap, (HostEnvironment<'lean>, $input, usize, usize),
                LeanIo<Result<$output, u8>>>,)+
        }

        impl<'lean, 'cap> ProgressQueries<'lean, 'cap> {
            fn resolve(capability: &'cap TrustedHostCapability<'lean>, receipts: &mut Receipts) -> Result<Self, LeanCheckedExportError> {
                #[allow(unsafe_code, reason = "fixed bundled progress shim contracts")]
                // SAFETY: each export accepts a synchronous progress callback and returns
                // IO (Except UInt8 result) with the indicated complete native payload.
                unsafe { Ok(Self { $($name: bind(capability, receipts, $symbol)?,)+ }) }
            }
        }

        impl<'lean> HostShimBindings<'lean, '_> {
            $(
                #[doc = concat!("Invoke `", $symbol, "` with a scoped progress callback.")]
                ///
                /// # Errors
                ///
                /// Returns native IO, conversion or callback errors.
                #[allow(unsafe_code, reason = "scoped callback supplies both slots of the attested progress protocol")]
                pub fn $name(&self, env: &HostEnvironment<'lean>, $arg: $input,
                    callback: &crate::LeanProgressCallback<'_>) -> LeanResult<$output> {
                    let (handle, trampoline) = callback.abi_parts();
                    // SAFETY: both slots come from the same live registration. The token attests
                    // synchronous use, the progress payload and the exact argument and result types.
                    let result = unsafe { self.progress.$name.call(env.clone(), $arg, handle, trampoline) }?;
                    callback.finish_result(result)
                }
            )+
        }
    };
}

progress_queries! {
    list_declarations_filtered_progress(filter: LeanDeclarationFilter) -> Vec<LeanName<'lean>> = "lean_rs_host_env_list_declarations_filtered_progress";
    query_declarations_bulk_progress(names: Vec<LeanName<'lean>>) -> Vec<Option<LeanDeclaration<'lean>>> = "lean_rs_host_env_query_declarations_bulk_progress";
    declaration_type_bulk_progress(names: Vec<String>) -> Vec<Option<LeanExpr<'lean>>> = "lean_rs_host_env_declaration_type_bulk_progress";
    declaration_kind_bulk_progress(names: Vec<String>) -> Vec<String> = "lean_rs_host_env_declaration_kind_bulk_progress";
    declaration_name_bulk_progress(names: Vec<String>) -> Vec<String> = "lean_rs_host_env_declaration_name_bulk_progress";
}

// Optional exports are absent only on a checked missing-symbol result. A wrong
// signature, missing signature metadata, or wrong symbol kind remains an error.
macro_rules! cache_operations {
    ($($field:ident($($arg:ident : $ty:ty),*) -> $ret:ty = $symbol:literal;)+) => {
        struct CacheOperations<'lean, 'cap> {
            $($field: Option<LeanExported<'lean, 'cap, ($($ty,)*), LeanIo<$ret>>>,)+
        }

        impl<'lean, 'cap> CacheOperations<'lean, 'cap> {
            fn resolve(capability: &'cap TrustedHostCapability<'lean>, receipts: &mut Receipts) -> Result<Self, LeanCheckedExportError> {
                #[allow(unsafe_code, reason = "fixed bundled cache and runtime metadata contracts")]
                // SAFETY: the closed rows match the exact exports in Environment.lean and InfoTree.lean.
                unsafe { Ok(Self { $($field: match bind(capability, receipts, $symbol) {
                    Ok(binding) => Some(binding),
                    Err(LeanCheckedExportError::MissingSymbol { .. }) => None,
                    Err(error) => return Err(error),
                },)+ }) }
            }
        }

        impl<'lean> HostShimBindings<'lean, '_> {
            $(
                #[doc = concat!("Call the optional bundled export `", $symbol, "` under import admission.")]
                ///
                /// # Errors
                ///
                /// Returns admission, native IO or conversion errors.
                #[allow(unsafe_code, reason = "fixed cache operation with native typed inputs and result")]
                pub fn $field(&self, $($arg: $ty),*) -> LeanResult<Option<$ret>> {
                    let Some(call) = &self.cache.$field else { return Ok(None) };
                    with_host_import_transaction(|_| {
                        // SAFETY: the cached export uses this exact row and the shared transaction
                        // serializes access to the process-wide shim state.
                        unsafe { call.call($($arg),*) }.map(Some)
                    })
                }
            )+
        }
    };
}

cache_operations! {
    configure_header_import_budget(session_bytes: u64, limit_bytes: u64) -> () = "lean_rs_host_configure_header_import_budget";
    header_import_state(unit: ()) -> String = "lean_rs_host_header_import_state";
    clear_module_header_cache(environment: HostEnvironment<'lean>) -> () = "lean_rs_host_clear_module_header_cache";
    runtime_version() -> String = "lean_rs_host_runtime_version";
    clear_module_snapshot_cache(unit: ()) -> Obj<'lean> = "lean_rs_host_clear_module_snapshot_cache";
}

/// Exact bundled elaborator options, including all context used to interpret source.
#[derive(Clone, Debug)]
pub struct HostSourceOptions {
    /// Lean namespace of the source.
    pub namespace: String,
    /// File label used by the parser.
    pub file: String,
    /// Heartbeat budget forwarded to Lean.
    pub heartbeats: u64,
    /// Upper bound on diagnostic bytes.
    pub diagnostic_bytes: usize,
    /// Serialized Lean options.
    pub options_json: String,
}

#[derive(Clone)]
struct SourceContext<'lean, 'cap> {
    capability: &'cap TrustedHostCapability<'lean>,
    environment: HostEnvironment<'lean>,
    imports: crate::module::HostImportIdentity,
    resolved_paths: Vec<String>,
    options: HostSourceOptions,
}

/// Immutable single-term request for one environment and import attestation.
pub struct TrustedHostTerm<'lean, 'cap> {
    context: SourceContext<'lean, 'cap>,
    source: String,
    expected: Option<LeanExpr<'lean>>,
}

/// Immutable declaration source admitted specifically for command elaboration and kernel checking.
pub struct TrustedHostKernelRequest<'lean, 'cap> {
    context: SourceContext<'lean, 'cap>,
    declaration: String,
}

/// Ordered term batch with one environment, resolution context and options snapshot.
pub struct TrustedHostTermBatch<'lean, 'cap> {
    context: SourceContext<'lean, 'cap>,
    sources: Vec<String>,
}

impl<'lean, 'cap> TrustedHostTermBatch<'lean, 'cap> {
    /// Number of terms admitted in this batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }
    /// Whether the batch has zero terms.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
    /// Extract exactly the captured terms in order for cancellable per-term dispatch.
    pub fn terms(&self) -> impl Iterator<Item = TrustedHostTerm<'lean, 'cap>> + '_ {
        self.sources.iter().map(|source| TrustedHostTerm {
            context: self.context.clone(),
            source: source.clone(),
            expected: None,
        })
    }
}

/// Result of decoding the bundled kernel checker's four-branch native result.
pub enum HostKernelOutcome<'lean> {
    /// Kernel-produced evidence.
    Checked(HostEvidence<'lean>),
    /// Diagnostic payload for rejected input.
    Rejected(Obj<'lean>),
    /// Diagnostic payload for an unavailable checker.
    Unavailable(Obj<'lean>),
    /// Diagnostic payload for unsupported input.
    Unsupported(Obj<'lean>),
}

impl<'lean> TrustedHostCapability<'lean> {
    fn source_context<'cap>(
        &'cap self,
        environment: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        resolved_paths: Vec<String>,
        options: HostSourceOptions,
    ) -> LeanResult<SourceContext<'lean, 'cap>> {
        imports.check_capability(self)?;
        if resolved_paths
            .iter()
            .any(|path| !std::path::Path::new(path).is_absolute())
        {
            return Err(crate::LeanError::module_init(
                "source scope requires absolute resolved search paths",
            ));
        }
        Ok(SourceContext {
            capability: self,
            environment: environment.clone(),
            imports: imports.identity(),
            resolved_paths,
            options,
        })
    }

    /// Attest the exact term, expected type, settings and complete resolved native search context.
    ///
    /// # Safety
    ///
    /// The environment must come from this import scope. Establish and maintain custody of all artifacts
    /// reachable by the exact term, elaborators, plugins, options and expected type in this environment.
    /// Supply the entire effective search path, including sysroot entries, in resolution order.
    /// Native execution must preserve runtime, ownership and callback invariants for returned values
    /// and process-global registrations throughout their lifetimes.
    ///
    /// # Errors
    ///
    /// Rejects another capability's import scope or relative search roots.
    #[allow(unsafe_code, reason = "application attests source-dependent native execution")]
    pub unsafe fn term_scope<'cap>(
        &'cap self,
        environment: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        resolved_paths: Vec<String>,
        source: String,
        expected: Option<LeanExpr<'lean>>,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedHostTerm<'lean, 'cap>> {
        Ok(TrustedHostTerm {
            context: self.source_context(environment, imports, resolved_paths, options)?,
            source,
            expected,
        })
    }

    /// Attest an ordered batch and its individual no-expected-type elaborations.
    ///
    /// # Safety
    ///
    /// Meet the custody, runtime and complete path obligations of [`Self::term_scope`] for each term,
    /// its standalone elaboration, and the synchronous batch traversal with or without progress.
    ///
    /// # Errors
    ///
    /// Rejects another capability's scope or relative roots.
    #[allow(
        unsafe_code,
        reason = "application attests the exact batch and each cancellable suboperation"
    )]
    pub unsafe fn term_batch_scope<'cap>(
        &'cap self,
        environment: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        resolved_paths: Vec<String>,
        sources: Vec<String>,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedHostTermBatch<'lean, 'cap>> {
        Ok(TrustedHostTermBatch {
            context: self.source_context(environment, imports, resolved_paths, options)?,
            sources,
        })
    }

    /// Attest command elaboration and kernel checking for this exact declaration.
    ///
    /// # Safety
    ///
    /// Meet the artifact, runtime and complete path obligations of [`Self::term_scope`] for the
    /// entire command frontend, namespace prelude, initializers and kernel execution selected by this
    /// exact source and options, both with and without a synchronous progress callback.
    ///
    /// # Errors
    ///
    /// Rejects another capability's scope or relative roots.
    #[allow(unsafe_code, reason = "application attests command-frontend native execution")]
    pub unsafe fn kernel_scope<'cap>(
        &'cap self,
        environment: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        resolved_paths: Vec<String>,
        declaration: String,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedHostKernelRequest<'lean, 'cap>> {
        Ok(TrustedHostKernelRequest {
            context: self.source_context(environment, imports, resolved_paths, options)?,
            declaration,
        })
    }
}

/// Decoded native `MetaResponse` with the success payload type fixed by the service.
pub enum HostMetaOutcome<'lean, T> {
    /// Successful typed payload.
    Ok(T),
    /// Ordinary failure diagnostics.
    Failed(Obj<'lean>),
    /// Heartbeat exhaustion diagnostics.
    Timeout(Obj<'lean>),
    /// Unsupported-service diagnostics.
    Unsupported(Obj<'lean>),
}

macro_rules! meta_operations {
    ($($field:ident($req:ty) -> $out:ty = $symbol:literal;)+) => {
        struct MetaOperations<'lean, 'cap> {
            $($field: Option<LeanExported<'lean, 'cap, (HostEnvironment<'lean>, $req, u64, usize, u8), LeanIo<Obj<'lean>>>>,)+
        }
        impl<'lean, 'cap> MetaOperations<'lean, 'cap> {
            fn resolve(capability: &'cap TrustedHostCapability<'lean>, receipts: &mut Receipts) -> Result<Self, LeanCheckedExportError> {
                #[allow(unsafe_code, reason = "closed Meta.lean contracts on the actual attested image")]
                // SAFETY: all five exports take the typed environment, exact request and scalar budget slots
                // shown here and return IO (MetaResponse output) for the indicated concrete output type.
                unsafe { Ok(Self { $($field: match bind(capability, receipts, $symbol) {
                    Ok(binding) => Some(binding),
                    Err(LeanCheckedExportError::MissingSymbol { .. }) => None,
                    Err(error) => return Err(error),
                },)+ }) }
            }
        }
        impl<'lean> HostShimBindings<'lean, '_> {
            $(
                #[doc = concat!("Invoke the optional `", $symbol, "` service.")]
                ///
                /// # Errors
                ///
                /// Returns native IO or conversion failures.
                #[allow(unsafe_code, reason = "private fixed binding establishes the native `MetaResponse` payload type")]
                pub fn $field(&self, env: &HostEnvironment<'lean>, request: $req, heartbeats: u64,
                    diagnostic_bytes: usize, transparency: u8) -> LeanResult<Option<HostMetaOutcome<'lean, $out>>> {
                    let Some(call) = &self.meta.$field else { return Ok(None) };
                    // SAFETY: the request has the fixed row's type; this attested export returns MetaResponse output.
                    let raw = unsafe { call.call(env.clone(), request, heartbeats, diagnostic_bytes, transparency) }?;
                    let tag = crate::abi::structure::view(&raw).ctor()?.tag();
                    let [payload] = crate::abi::structure::take_ctor_objects::<1>(raw, tag, "MetaResponse")?;
                    Ok(Some(match tag {
                        // SAFETY: MetaResponse.ok from this exact export carries the row's native output.
                        0 => HostMetaOutcome::Ok(unsafe { <$out as LeanBoxedAbi>::from_boxed(payload) }?),
                        1 => HostMetaOutcome::Failed(payload),
                        2 => HostMetaOutcome::Timeout(payload),
                        3 => HostMetaOutcome::Unsupported(payload),
                        _ => return Err(crate::abi::traits::conversion_error(format!("unknown MetaResponse tag {tag}"))),
                    }))
                }
            )+
        }
    }
}

meta_operations! {
    meta_infer_type(LeanExpr<'lean>) -> LeanExpr<'lean> = "lean_rs_host_meta_infer_type";
    meta_whnf(LeanExpr<'lean>) -> LeanExpr<'lean> = "lean_rs_host_meta_whnf";
    meta_heartbeat_burn(LeanExpr<'lean>) -> LeanExpr<'lean> = "lean_rs_host_meta_heartbeat_burn";
    meta_is_def_eq((LeanExpr<'lean>, LeanExpr<'lean>, u8)) -> bool = "lean_rs_host_meta_is_def_eq";
    meta_pp_expr(LeanExpr<'lean>) -> String = "lean_rs_host_meta_pp_expr";
}

/// Closed host bindings borrowed from the exact attested loaded capability.
///
/// Every field is private. Safe callers cannot change native signatures, supply
/// arbitrary symbols, or replace the capability after binding.
pub struct HostShimBindings<'lean, 'cap> {
    capability: &'cap TrustedHostCapability<'lean>,
    module_sources: source_operations::ModuleOperations<'lean, 'cap>,
    meta: MetaOperations<'lean, 'cap>,
    cache: CacheOperations<'lean, 'cap>,
    bracketed_import:
        LeanExported<'lean, 'cap, (Vec<String>, Vec<String>, Vec<String>, usize, usize), LeanIo<Result<String, u8>>>,
    queries: DeclarationQueries<'lean, 'cap>,
    progress: ProgressQueries<'lean, 'cap>,
    session_import: LeanExported<'lean, 'cap, (Vec<String>, Vec<String>), LeanIo<HostEnvironment<'lean>>>,
    import_paths: Option<
        LeanExported<
            'lean,
            'cap,
            (
                Vec<String>,
                Vec<String>,
                bool,
                u8,
                bool,
                Option<(bool, bool, String)>,
                Option<Vec<String>>,
                bool,
            ),
            LeanIo<(Option<HostEnvironment<'lean>>, Vec<String>)>,
        >,
    >,
    import_profile: LeanExported<
        'lean,
        'cap,
        (Vec<String>, Vec<String>, bool, u8, bool, bool, bool, String),
        LeanIo<HostEnvironment<'lean>>,
    >,
    import_progress:
        LeanExported<'lean, 'cap, (Vec<String>, Vec<String>, usize, usize), LeanIo<Result<HostEnvironment<'lean>, u8>>>,
    import_profile_progress: LeanExported<
        'lean,
        'cap,
        (Vec<String>, Vec<String>, bool, u8, usize, usize),
        LeanIo<Result<HostEnvironment<'lean>, u8>>,
    >,
    import_stats: LeanExported<'lean, 'cap, (HostEnvironment<'lean>, String, bool), LeanIo<Obj<'lean>>>,
    registry_epoch: LeanExported<'lean, 'cap, ((),), LeanIo<u64>>,
    source_range:
        LeanExported<'lean, 'cap, (HostEnvironment<'lean>, LeanName<'lean>, Vec<String>), LeanIo<Option<Obj<'lean>>>>,
    expr_to_string: LeanExported<'lean, 'cap, (LeanExpr<'lean>,), String>,
    restore_source_paths: LeanExported<'lean, 'cap, (Vec<String>, Obj<'lean>), LeanIo<()>>,
    elaborate: LeanExported<
        'lean,
        'cap,
        (
            HostEnvironment<'lean>,
            String,
            Option<LeanExpr<'lean>>,
            String,
            String,
            u64,
            usize,
            String,
        ),
        LeanIo<Result<LeanExpr<'lean>, Obj<'lean>>>,
    >,
    elaborate_batch: LeanExported<
        'lean,
        'cap,
        (HostEnvironment<'lean>, Vec<String>, String, String, u64, usize, String),
        LeanIo<Vec<Result<LeanExpr<'lean>, Obj<'lean>>>>,
    >,
    elaborate_batch_progress: LeanExported<
        'lean,
        'cap,
        (
            HostEnvironment<'lean>,
            Vec<String>,
            String,
            String,
            u64,
            usize,
            String,
            usize,
            usize,
        ),
        LeanIo<Result<Vec<Result<LeanExpr<'lean>, Obj<'lean>>>, u8>>,
    >,
    kernel: LeanExported<
        'lean,
        'cap,
        (HostEnvironment<'lean>, String, String, String, u64, usize, String),
        LeanIo<Obj<'lean>>,
    >,
    kernel_progress: LeanExported<
        'lean,
        'cap,
        (
            HostEnvironment<'lean>,
            String,
            String,
            String,
            u64,
            usize,
            String,
            usize,
            usize,
        ),
        LeanIo<Result<Obj<'lean>, u8>>,
    >,
    search: LeanExported<'lean, 'cap, (HostEnvironment<'lean>, Obj<'lean>, Vec<String>), LeanIo<Obj<'lean>>>,
    inspect:
        Option<LeanExported<'lean, 'cap, (HostEnvironment<'lean>, Obj<'lean>, Vec<String>, u64), LeanIo<Obj<'lean>>>>,
    receipts: std::collections::BTreeMap<String, serde_json::Value>,
    name_from_string: LeanExported<'lean, 'cap, (String,), LeanName<'lean>>,
    name_to_string: LeanExported<'lean, 'cap, (LeanName<'lean>,), String>,
    list_declarations: LeanExported<'lean, 'cap, (HostEnvironment<'lean>,), LeanIo<Vec<LeanName<'lean>>>>,
    check_evidence: LeanExported<'lean, 'cap, (HostEnvironment<'lean>, HostEvidence<'lean>), LeanIo<Obj<'lean>>>,
    evidence_summary: LeanExported<'lean, 'cap, (HostEnvironment<'lean>, HostEvidence<'lean>), LeanIo<Obj<'lean>>>,
}

impl<'lean, 'cap> HostShimBindings<'lean, 'cap> {
    /// Bind concrete operations from an immutable host-contract attestation.
    ///
    /// # Errors
    ///
    /// Returns the checked lookup error for a missing symbol or incompatible
    /// C slot/kind. Semantic trust comes from `capability`, not those checks.
    #[allow(
        unsafe_code,
        reason = "the retained token attests these exact fixed native contracts"
    )]
    pub fn resolve(capability: &'cap TrustedHostCapability<'lean>) -> Result<Self, LeanCheckedExportError> {
        let mut receipts = Receipts::new();
        // SAFETY: the token attests the complete contracts of the fixed SDK
        // host symbols. Rust strings/vectors and opaque typed handles enforce
        // the argument invariants; the decoder types match their Lean results.
        unsafe {
            Ok(Self {
                capability,
                module_sources: source_operations::ModuleOperations::resolve(capability, &mut receipts)?,
                meta: MetaOperations::resolve(capability, &mut receipts)?,
                cache: CacheOperations::resolve(capability, &mut receipts)?,
                bracketed_import: bind(capability, &mut receipts, "lean_rs_host_bracketed_import_query")?,
                queries: DeclarationQueries::resolve(capability, &mut receipts)?,
                progress: ProgressQueries::resolve(capability, &mut receipts)?,
                session_import: bind(capability, &mut receipts, "lean_rs_host_session_import")?,
                import_paths: {
                    const SYMBOL: &str = "lean_rs_host_session_import_paths_v1";
                    optional_import_binding(bind(capability, &mut receipts, SYMBOL), || {
                        capability
                            .capability()
                            .library()
                            .resolve_function_symbol(SYMBOL)
                            .is_ok()
                    })?
                },
                import_profile: bind(capability, &mut receipts, "lean_rs_host_session_import_profile")?,
                import_progress: bind(capability, &mut receipts, "lean_rs_host_session_import_progress")?,
                import_profile_progress: bind(
                    capability,
                    &mut receipts,
                    "lean_rs_host_session_import_profile_progress",
                )?,
                import_stats: bind(capability, &mut receipts, "lean_rs_host_env_import_stats")?,
                registry_epoch: bind(capability, &mut receipts, "lean_rs_host_extension_registry_epoch")?,
                source_range: bind(capability, &mut receipts, "lean_rs_host_env_declaration_source_range")?,
                search: bind(capability, &mut receipts, "lean_rs_host_env_search_declarations")?,
                inspect: match bind(capability, &mut receipts, "lean_rs_host_env_inspect_declaration") {
                    Ok(binding) => Some(binding),
                    Err(LeanCheckedExportError::MissingSymbol { .. }) => None,
                    Err(error) => return Err(error),
                },
                expr_to_string: bind(capability, &mut receipts, "lean_rs_host_env_expr_to_string_raw")?,
                restore_source_paths: bind(capability, &mut receipts, "lean_rs_host_restore_source_search_path")?,
                elaborate: bind(capability, &mut receipts, "lean_rs_host_elaborate")?,
                elaborate_batch: bind(capability, &mut receipts, "lean_rs_host_elaborate_bulk")?,
                elaborate_batch_progress: bind(capability, &mut receipts, "lean_rs_host_elaborate_bulk_progress")?,
                kernel: bind(capability, &mut receipts, "lean_rs_host_kernel_check")?,
                kernel_progress: bind(capability, &mut receipts, "lean_rs_host_kernel_check_progress")?,
                name_from_string: bind(capability, &mut receipts, "lean_rs_host_name_from_string")?,
                name_to_string: bind(capability, &mut receipts, "lean_rs_host_name_to_string")?,
                list_declarations: bind(capability, &mut receipts, "lean_rs_host_env_list_declarations")?,
                check_evidence: bind(capability, &mut receipts, "lean_rs_host_check_evidence")?,
                evidence_summary: bind(capability, &mut receipts, "lean_rs_host_evidence_summary")?,
                receipts,
            })
        }
    }

    /// Use the attested profile to import and observe its installed paths, or to
    /// compare prospective paths without importing. Legacy shims return `None`.
    ///
    /// # Errors
    /// Returns scope mismatch, native IO or result conversion errors.
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "fixed optional import contract from the loaded host shim")]
    pub fn session_import_paths_in(
        &self,
        _transaction: &mut HostImportTransaction,
        scope: &TrustedHostImports<'lean, '_>,
        expected: Option<&[String]>,
        compare_only: bool,
    ) -> LeanResult<Option<(Option<HostEnvironment<'lean>>, Vec<String>)>> {
        let Some(call) = &self.import_paths else {
            return Ok(None);
        };
        let (paths, imports) = scope.native_inputs(self.capability)?;
        let options = scope.options();
        // SAFETY: the attestation covers these exact inputs. The versioned export returns
        // an owned Prod (Option Environment) (Array String) at the matching runtime.
        unsafe {
            call.call(
                paths,
                imports,
                options.import_all,
                options.level,
                options.load_extensions,
                (options != &crate::module::HostImportOptions::default())
                    .then(|| (options.profiler, options.trace_profiler, options.trace_output.clone())),
                expected.map(<[String]>::to_vec),
                compare_only,
            )
        }
        .map(Some)
    }

    /// Import modules and return a typed environment owned by the caller.
    ///
    /// # Errors
    ///
    /// Returns scope mismatch, admission errors, native import failures, or
    /// result conversion errors.
    pub fn session_import(&self, scope: &TrustedHostImports<'lean, '_>) -> LeanResult<HostEnvironment<'lean>> {
        with_host_import_transaction(|transaction| self.session_import_in(transaction, scope))
    }

    /// Import inside a caller's existing transaction, retaining admission for epoch updates.
    ///
    /// # Errors
    ///
    /// Returns scope mismatch, native import failures, or result conversion errors.
    #[allow(unsafe_code, reason = "fixed synchronous import contract with owned String arrays")]
    pub fn session_import_in(
        &self,
        _transaction: &mut HostImportTransaction,
        scope: &TrustedHostImports<'lean, '_>,
    ) -> LeanResult<HostEnvironment<'lean>> {
        let (search_paths, imports) = scope.native_inputs(self.capability)?;
        let options = scope.options();
        // SAFETY: the scope selects the attested module graph and exact profile. Both
        // bindings take owned Array String values and return an owned IO Environment.
        unsafe {
            if options == &crate::module::HostImportOptions::default() {
                self.session_import.call(search_paths, imports)
            } else {
                self.import_profile.call(
                    search_paths,
                    imports,
                    options.import_all,
                    options.level,
                    options.load_extensions,
                    options.profiler,
                    options.trace_profiler,
                    options.trace_output.clone(),
                )
            }
        }
    }

    /// Import the exact scoped profile while reporting synchronous progress.
    ///
    /// # Errors
    ///
    /// Rejects profiles unsupported by the progress export and propagates native or callback errors.
    #[allow(unsafe_code, reason = "fixed import contract with a borrowed synchronous callback")]
    pub fn session_import_progress_in(
        &self,
        _transaction: &mut HostImportTransaction,
        scope: &TrustedHostImports<'lean, '_>,
        callback: &crate::LeanProgressCallback<'_>,
    ) -> LeanResult<HostEnvironment<'lean>> {
        let (paths, imports) = scope.native_inputs(self.capability)?;
        let options = scope.options();
        if !options.load_extensions || options.profiler || options.trace_profiler || !options.trace_output.is_empty() {
            return Err(crate::LeanError::module_init(
                "progress import cannot execute the attested profiler or extension settings",
            ));
        }
        let (handle, trampoline) = callback.abi_parts();
        // SAFETY: both progress slots come from one live registration. Both exports return
        // IO (Except UInt8 Environment); the attested inputs select all native import options.
        let result = unsafe {
            if options == &crate::module::HostImportOptions::default() {
                self.import_progress.call(paths, imports, handle, trampoline)
            } else {
                self.import_profile_progress
                    .call(paths, imports, options.import_all, options.level, handle, trampoline)
            }
        }?;
        callback.finish_result(result)
    }

    /// Read the imported environment's ordinary statistics record.
    ///
    /// # Errors
    ///
    /// Propagates a native IO error or decoding failure.
    #[allow(unsafe_code, reason = "typed environment and exact statistics record ABI")]
    pub fn import_stats(
        &self,
        env: &HostEnvironment<'lean>,
        level: String,
        extensions: bool,
    ) -> LeanResult<Obj<'lean>> {
        // SAFETY: the typed environment supplies an owned reference and the export returns an owned record.
        unsafe { self.import_stats.call(env.clone(), level, extensions) }
    }

    /// Read the registry epoch while holding process-wide import admission.
    ///
    /// # Errors
    ///
    /// Propagates a native IO or conversion error.
    #[allow(unsafe_code, reason = "fixed Unit to IO UInt64 export")]
    pub fn registry_epoch_in(&self, _transaction: &mut HostImportTransaction) -> LeanResult<u64> {
        // SAFETY: the retained image implements the fixed IO UInt64 epoch query.
        unsafe { self.registry_epoch.call(()) }
    }

    /// Run a bracketed import and return the copied JSON after the compacted regions are freed.
    ///
    /// # Errors
    ///
    /// Returns admission, scope mismatch, native IO, callback, or conversion errors.
    #[allow(unsafe_code, reason = "attested bracketed import with copied String output")]
    pub fn bracketed_import_query(
        &self,
        scope: &crate::module::TrustedBracketedImports<'lean, '_>,
        declarations: Vec<String>,
        callback: Option<&crate::LeanProgressCallback<'_>>,
    ) -> LeanResult<String> {
        let (paths, imports) = scope.0.native_inputs(self.capability)?;
        let (handle, trampoline) = callback.map_or((0, 0), crate::LeanProgressCallback::abi_parts);
        with_host_import_transaction(|_| {
            // SAFETY: both slots are a live synchronous registration or the documented zero pair.
            // The exact scope supplies the import graph. The shim copies the returned JSON out
            // of the compacted regions before returning IO (Except UInt8 String).
            let result = unsafe {
                self.bracketed_import
                    .call(paths, imports, declarations, handle, trampoline)
            }?;
            match callback {
                Some(callback) => callback.finish_result(result),
                None => result.map_err(|status| {
                    crate::LeanError::internal(format!(
                        "bracketed import returned callback status {status} without a registered callback"
                    ))
                }),
            }
        })
    }

    /// Retrieve the ordinary source-range record for a declaration.
    ///
    /// # Errors
    ///
    /// Returns a native IO or conversion error.
    #[allow(unsafe_code, reason = "fixed Environment, Name and Array String query contract")]
    pub fn declaration_source_range(
        &self,
        env: &HostEnvironment<'lean>,
        name: LeanName<'lean>,
        roots: Vec<String>,
    ) -> LeanResult<Option<Obj<'lean>>> {
        // SAFETY: typed handles and owned source roots match the fixed bundled export.
        unsafe { self.source_range.call(env.clone(), name, roots) }
    }

    /// Render a typed expression through the bundled raw expression printer.
    ///
    /// # Errors
    ///
    /// Returns a native call or conversion error.
    #[allow(unsafe_code, reason = "fixed Expr to String contract")]
    pub fn expr_to_string(&self, expr: &LeanExpr<'lean>) -> LeanResult<String> {
        // SAFETY: cloning the opaque expression supplies the owned Expr consumed by the export.
        unsafe { self.expr_to_string.call(expr.clone()) }
    }

    /// Elaborate the exact attested term in its original session.
    ///
    /// # Errors
    ///
    /// Rejects a mismatched session and propagates admission, native IO and conversion errors.
    #[allow(
        unsafe_code,
        reason = "closed term export with immutable inputs and restored resolution context"
    )]
    pub fn elaborate(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        term: &TrustedHostTerm<'lean, '_>,
    ) -> LeanResult<Result<LeanExpr<'lean>, Obj<'lean>>> {
        self.with_source(env, imports, &term.context, |environment, options| {
            // SAFETY: the attested request supplies the complete term, expected type, and options.
            unsafe {
                self.elaborate.call(
                    environment.clone(),
                    term.source.clone(),
                    term.expected.clone(),
                    options.namespace.clone(),
                    options.file.clone(),
                    options.heartbeats,
                    options.diagnostic_bytes,
                    options.options_json.clone(),
                )
            }
        })
    }

    fn with_source<T>(
        &self,
        environment: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        context: &SourceContext<'lean, '_>,
        call: impl FnOnce(&HostEnvironment<'lean>, &HostSourceOptions) -> LeanResult<T>,
    ) -> LeanResult<T> {
        if !core::ptr::eq(context.capability, self.capability)
            || imports.identity() != context.imports
            || environment.obj.as_raw_borrowed() != context.environment.obj.as_raw_borrowed()
        {
            return Err(crate::LeanError::module_init(
                "source attestation belongs to another environment or import scope",
            ));
        }
        with_host_import_transaction(|_| {
            #[allow(
                unsafe_code,
                reason = "fixed search-path restoration under exclusive import admission"
            )]
            // SAFETY: the exact resolved path is retained by the source scope; the shim consumes Array String.
            unsafe {
                self.restore_source_paths
                    .call(context.resolved_paths.clone(), imports.native_identity.clone())
            }?;
            call(&context.environment, &context.options)
        })
    }

    /// Elaborate the immutable batch with an optional scoped progress callback.
    ///
    /// # Errors
    ///
    /// Returns scope mismatch, admission, native IO, callback or conversion errors.
    #[allow(
        unsafe_code,
        reason = "fixed batch contracts with captured inputs and typed nested results"
    )]
    pub fn elaborate_bulk(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        batch: &TrustedHostTermBatch<'lean, '_>,
        callback: Option<&crate::LeanProgressCallback<'_>>,
    ) -> LeanResult<Vec<Result<LeanExpr<'lean>, Obj<'lean>>>> {
        self.with_source(env, imports, &batch.context, |env, o| {
            // SAFETY: both fixed exports consume the captured Array String and options and produce an array
            // of Except ElabFailure Expr. The progress variant uses only its live callback's native slots.
            unsafe {
                match callback {
                    Some(cb) => {
                        let (h, t) = cb.abi_parts();
                        cb.finish_result(self.elaborate_batch_progress.call(
                            env.clone(),
                            batch.sources.clone(),
                            o.namespace.clone(),
                            o.file.clone(),
                            o.heartbeats,
                            o.diagnostic_bytes,
                            o.options_json.clone(),
                            h,
                            t,
                        )?)
                    }
                    None => self.elaborate_batch.call(
                        env.clone(),
                        batch.sources.clone(),
                        o.namespace.clone(),
                        o.file.clone(),
                        o.heartbeats,
                        o.diagnostic_bytes,
                        o.options_json.clone(),
                    ),
                }
            }
        })
    }

    /// Check the exact command request and return evidence only on the checked branch.
    ///
    /// # Errors
    ///
    /// Returns scope mismatch, admission, native IO, callback or conversion errors.
    #[allow(
        unsafe_code,
        reason = "kernel result provenance follows the fixed attested native export"
    )]
    pub fn kernel_check(
        &self,
        env: &HostEnvironment<'lean>,
        imports: &TrustedHostImports<'lean, '_>,
        request: &TrustedHostKernelRequest<'lean, '_>,
        callback: Option<&crate::LeanProgressCallback<'_>>,
    ) -> LeanResult<HostKernelOutcome<'lean>> {
        self.with_source(env, imports, &request.context, |env, o| {
            // SAFETY: the captured declaration and options supply the full frontend request.
            // The progress variant returns Except UInt8 KernelOutcome with the same live registration.
            let result = unsafe {
                match callback {
                    Some(cb) => {
                        let (h, t) = cb.abi_parts();
                        cb.finish_result(self.kernel_progress.call(
                            env.clone(),
                            request.declaration.clone(),
                            o.namespace.clone(),
                            o.file.clone(),
                            o.heartbeats,
                            o.diagnostic_bytes,
                            o.options_json.clone(),
                            h,
                            t,
                        )?)
                    }
                    None => self.kernel.call(
                        env.clone(),
                        request.declaration.clone(),
                        o.namespace.clone(),
                        o.file.clone(),
                        o.heartbeats,
                        o.diagnostic_bytes,
                        o.options_json.clone(),
                    ),
                }
            }?;
            let tag = crate::abi::structure::view(&result).ctor()?.tag();
            let [obj] = crate::abi::structure::take_ctor_objects::<1>(result, tag, "KernelOutcome")?;
            match tag {
                0 => Ok(HostKernelOutcome::Checked(HostEvidence { obj })),
                1 => Ok(HostKernelOutcome::Rejected(obj)),
                2 => Ok(HostKernelOutcome::Unavailable(obj)),
                3 => Ok(HostKernelOutcome::Unsupported(obj)),
                _ => Err(crate::abi::traits::conversion_error(format!(
                    "unknown KernelOutcome tag {tag}"
                ))),
            }
        })
    }

    /// Return signatures derived from typed handles that were actually resolved.
    #[must_use]
    pub fn checked_signatures(&self) -> std::collections::BTreeMap<String, serde_json::Value> {
        self.receipts.clone()
    }

    /// Search the imported environment using a concrete declaration search request.
    ///
    /// # Errors
    ///
    /// Propagates native IO and conversion errors.
    #[allow(
        unsafe_code,
        reason = "owned concrete search DTO supplies the fixed native request layout"
    )]
    pub fn search_declarations(
        &self,
        env: &HostEnvironment<'lean>,
        request: declaration_requests::DeclarationSearchRequest,
        roots: Vec<String>,
    ) -> LeanResult<Obj<'lean>> {
        use crate::abi::traits::IntoLean;
        // SAFETY: only the closed owned search request is encoded for this slot; the result is ordinary data.
        unsafe {
            self.search
                .call(env.clone(), request.into_lean(env.obj.runtime()), roots)
        }
    }

    /// Whether the attested library provides the optional declaration inspection operation.
    #[must_use]
    pub fn supports_declaration_inspection(&self) -> bool {
        self.inspect.is_some()
    }

    /// Inspect a declaration with the concrete inspection request and heartbeat budget.
    ///
    /// # Errors
    ///
    /// Propagates native IO and conversion errors.
    #[allow(
        unsafe_code,
        reason = "closed inspection DTO and owned environment match the fixed export"
    )]
    pub fn inspect_declaration(
        &self,
        env: &HostEnvironment<'lean>,
        request: declaration_requests::DeclarationInspectionRequest,
        roots: Vec<String>,
        heartbeats: u64,
    ) -> LeanResult<Option<Obj<'lean>>> {
        use crate::abi::traits::IntoLean;
        let Some(call) = &self.inspect else { return Ok(None) };
        // SAFETY: the SDK's concrete inspection DTO is encoded here and the fixed export returns ordinary data.
        unsafe { call.call(env.clone(), request.into_lean(env.obj.runtime()), roots, heartbeats) }.map(Some)
    }

    /// Construct a Name through the bundled Lean implementation.
    ///
    /// # Errors
    ///
    /// Returns a native call or conversion error.
    #[allow(unsafe_code, reason = "fixed String-to-Name native contract")]
    pub fn name_from_string(&self, source: String) -> LeanResult<LeanName<'lean>> {
        // SAFETY: Rust String produces the exact Lean String representation;
        // the attested export returns an owned Lean.Name.
        unsafe { self.name_from_string.call(source) }
    }

    /// Render a valid opaque Name through Lean.
    ///
    /// # Errors
    ///
    /// Returns a native call or conversion error.
    #[allow(unsafe_code, reason = "fixed Name-to-String native contract")]
    pub fn name_to_string(&self, name: &LeanName<'lean>) -> LeanResult<String> {
        // SAFETY: private handle construction establishes Name's layout, and
        // cloning supplies the reference consumed by the attested export.
        unsafe { self.name_to_string.call(name.clone()) }
    }

    /// Return typed declaration names from an imported environment.
    ///
    /// # Errors
    ///
    /// Returns a Lean IO failure or native result conversion error.
    #[allow(unsafe_code, reason = "fixed Environment-to-IO-Array-Name native contract")]
    pub fn list_declarations(&self, environment: &HostEnvironment<'lean>) -> LeanResult<Vec<LeanName<'lean>>> {
        // SAFETY: the environment's private constructor establishes its type;
        // the attested export returns IO (Array Name), including every element.
        unsafe { self.list_declarations.call(environment.clone()) }
    }

    /// Recheck typed evidence and return its ordinary status data.
    ///
    /// # Errors
    ///
    /// Returns a Lean IO failure or native result conversion error.
    #[allow(unsafe_code, reason = "fixed Environment/Evidence recheck contract")]
    pub fn check_evidence(
        &self,
        environment: &HostEnvironment<'lean>,
        evidence: &HostEvidence<'lean>,
    ) -> LeanResult<Obj<'lean>> {
        // SAFETY: both opaque arguments retain their native types and owned
        // references; the attested result is owned status data, not a handle.
        unsafe { self.check_evidence.call(environment.clone(), evidence.clone()) }
    }

    /// Project typed evidence to ordinary summary data for checked inspection.
    ///
    /// # Errors
    ///
    /// Returns a Lean IO failure or native result conversion error.
    #[allow(unsafe_code, reason = "fixed Environment/Evidence summary contract")]
    pub fn evidence_summary(
        &self,
        environment: &HostEnvironment<'lean>,
        evidence: &HostEvidence<'lean>,
    ) -> LeanResult<Obj<'lean>> {
        // SAFETY: both opaque arguments have the fixed contract's types; the
        // returned object is owned ordinary data for checked Rust inspection.
        unsafe { self.evidence_summary.call(environment.clone(), evidence.clone()) }
    }
}

type Receipts = std::collections::BTreeMap<String, serde_json::Value>;

/// Only a manifest and image that both predate this export qualify for fallback.
fn optional_import_binding<T>(
    checked: Result<T, LeanCheckedExportError>,
    image_has_symbol: impl FnOnce() -> bool,
) -> Result<Option<T>, LeanCheckedExportError> {
    match checked {
        Ok(binding) => Ok(Some(binding)),
        Err(LeanCheckedExportError::MissingSignatureMetadata { .. }) if !image_has_symbol() => Ok(None),
        Err(error) => Err(error),
    }
}

/// # Safety
/// The caller must pair this exact symbol with its complete native types.
#[allow(
    unsafe_code,
    reason = "private binding island requires the closed contract assertion"
)]
unsafe fn bind<'lean, 'cap, Args, R>(
    capability: &'cap TrustedHostCapability<'lean>,
    receipts: &mut Receipts,
    symbol: &str,
) -> Result<LeanExported<'lean, 'cap, Args, R>, LeanCheckedExportError>
where
    Args: LeanArgs<'lean>,
    R: DecodeCallResult<'lean>,
{
    // SAFETY: the caller selected the fixed symbol's actual Lean types; the
    // token attests its native implementation and retains the exact image.
    let binding = unsafe { capability.capability().exported(symbol) }?;
    let signature =
        crate::module::LeanExportSignature::function(symbol, Args::export_abi_args(), R::export_abi_return());
    receipts.insert(
        symbol.trim_start_matches("lean_rs_host_").to_owned(),
        signature.to_json(),
    );
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::{LeanCheckedExportError, optional_import_binding};

    #[test]
    fn optional_import_binding_requires_a_matched_manifest_and_image() {
        let symbol = "lean_rs_host_session_import_paths_v1";
        assert!(matches!(optional_import_binding(Ok(()), || true), Ok(Some(()))));
        let missing_metadata = || LeanCheckedExportError::MissingSignatureMetadata { symbol: symbol.into() };
        assert!(matches!(
            optional_import_binding::<()>(Err(missing_metadata()), || false),
            Ok(None)
        ));
        assert!(matches!(
            optional_import_binding::<()>(Err(missing_metadata()), || true),
            Err(LeanCheckedExportError::MissingSignatureMetadata { .. })
        ));
        let advertised = LeanCheckedExportError::MissingSymbol {
            symbol: symbol.into(),
            source: crate::LeanError::symbol_lookup(symbol),
        };
        assert!(matches!(
            optional_import_binding::<()>(Err(advertised), || false),
            Err(LeanCheckedExportError::MissingSymbol { .. })
        ));
    }
}
