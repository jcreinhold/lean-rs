//! Checked typed bindings for the bundled `lean-rs-host` shim exports.
//!
//! This module is the single Rust source of truth for the standard host
//! shim C-ABI surface. The same `host_shim_exports!` rows generate the
//! artifact-manifest signatures and the cached typed call handles used by
//! `LeanSession`, so adding or changing a shim symbol updates both sides
//! together.

use lean_rs::module::{
    DecodeCallResult, LeanArgs, LeanCapability, LeanCheckedExportError, LeanExportSignature, LeanExported, LeanIo,
};
use lean_rs::{LeanDeclaration, LeanExpr, LeanName, Obj};

use crate::host::declaration_search::{
    DeclarationInspectionRequest, DeclarationInspectionResult, DeclarationSearchRequest, DeclarationSearchResult,
};
use crate::host::elaboration::LeanElabFailure;
use crate::host::evidence::{EvidenceStatus, LeanEvidence, LeanKernelOutcome, ProofSummary};
use crate::host::meta::{LeanMetaResponse, LeanMetaTransparency};
use crate::host::process::{
    DeclarationVerificationBatchOutcome, DeclarationVerificationBatchRequest, DeclarationVerificationOutcome,
    DeclarationVerificationRequest, ModuleQuery, ModuleQueryBatchCachedOutcome, ModuleQueryBatchOutcome,
    ModuleQueryOutcome, ModuleQueryOutputBudgets, ModuleQuerySelector, ModuleSnapshotCacheClearResult,
    ProofAttemptOutcome, ProofAttemptRequest,
};
use crate::host::session::{LeanDeclarationFilter, LeanImportStats, LeanSourceRange};

macro_rules! host_shim_exports {
    ($m:ident) => {
        $m! {
            mandatory session_import => "lean_rs_host_session_import"
                => [(Vec<String>, Vec<String>)] => [LeanIo<Obj<'lean>>];
            mandatory session_import_progress => "lean_rs_host_session_import_progress"
                => [(Vec<String>, Vec<String>, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory session_import_profile => "lean_rs_host_session_import_profile"
                => [(Vec<String>, Vec<String>, bool, u8, bool, bool, bool, String)] => [LeanIo<Obj<'lean>>];
            mandatory session_import_profile_progress => "lean_rs_host_session_import_profile_progress"
                => [(Vec<String>, Vec<String>, bool, u8, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory env_import_stats => "lean_rs_host_env_import_stats"
                => [(Obj<'lean>, String, bool)] => [LeanIo<LeanImportStats>];
            mandatory bracketed_import_query => "lean_rs_host_bracketed_import_query"
                => [(Vec<String>, Vec<String>, Vec<String>, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory name_from_string => "lean_rs_host_name_from_string"
                => [(Obj<'lean>,)] => [LeanName<'lean>];
            mandatory name_to_string => "lean_rs_host_name_to_string"
                => [(LeanName<'lean>,)] => [String];
            mandatory env_query_declaration => "lean_rs_host_env_query_declaration"
                => [(Obj<'lean>, LeanName<'lean>)] => [LeanIo<Option<LeanDeclaration<'lean>>>];
            mandatory env_query_declarations_bulk => "lean_rs_host_env_query_declarations_bulk"
                => [(Obj<'lean>, Vec<LeanName<'lean>>)] => [LeanIo<Vec<Option<LeanDeclaration<'lean>>>>];
            mandatory env_query_declarations_bulk_progress => "lean_rs_host_env_query_declarations_bulk_progress"
                => [(Obj<'lean>, Vec<LeanName<'lean>>, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory env_list_declarations => "lean_rs_host_env_list_declarations"
                => [(Obj<'lean>,)] => [LeanIo<Vec<Obj<'lean>>>];
            mandatory env_list_declarations_filtered => "lean_rs_host_env_list_declarations_filtered"
                => [(Obj<'lean>, LeanDeclarationFilter)] => [LeanIo<Vec<Obj<'lean>>>];
            mandatory env_list_declarations_filtered_progress => "lean_rs_host_env_list_declarations_filtered_progress"
                => [(Obj<'lean>, LeanDeclarationFilter, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory env_declaration_source_range => "lean_rs_host_env_declaration_source_range"
                => [(Obj<'lean>, LeanName<'lean>, Vec<String>)] => [LeanIo<Option<LeanSourceRange>>];
            mandatory env_declaration_type => "lean_rs_host_env_declaration_type"
                => [(Obj<'lean>, LeanName<'lean>)] => [LeanIo<Option<LeanExpr<'lean>>>];
            mandatory env_declaration_type_bulk => "lean_rs_host_env_declaration_type_bulk"
                => [(Obj<'lean>, Vec<String>)] => [LeanIo<Vec<Option<LeanExpr<'lean>>>>];
            mandatory env_declaration_type_bulk_progress => "lean_rs_host_env_declaration_type_bulk_progress"
                => [(Obj<'lean>, Vec<String>, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory env_declaration_kind => "lean_rs_host_env_declaration_kind"
                => [(Obj<'lean>, LeanName<'lean>)] => [LeanIo<String>];
            mandatory env_declaration_kind_bulk => "lean_rs_host_env_declaration_kind_bulk"
                => [(Obj<'lean>, Vec<String>)] => [LeanIo<Vec<Obj<'lean>>>];
            mandatory env_declaration_kind_bulk_progress => "lean_rs_host_env_declaration_kind_bulk_progress"
                => [(Obj<'lean>, Vec<String>, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory env_declaration_name => "lean_rs_host_env_declaration_name"
                => [(Obj<'lean>, LeanName<'lean>)] => [LeanIo<String>];
            mandatory env_declaration_name_bulk => "lean_rs_host_env_declaration_name_bulk"
                => [(Obj<'lean>, Vec<String>)] => [LeanIo<Vec<Obj<'lean>>>];
            mandatory env_declaration_name_bulk_progress => "lean_rs_host_env_declaration_name_bulk_progress"
                => [(Obj<'lean>, Vec<String>, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory env_search_declarations => "lean_rs_host_env_search_declarations"
                => [(Obj<'lean>, DeclarationSearchRequest, Vec<String>)] => [LeanIo<DeclarationSearchResult>];
            optional env_inspect_declaration => "lean_rs_host_env_inspect_declaration"
                => [(Obj<'lean>, DeclarationInspectionRequest, Vec<String>, u64)] => [LeanIo<DeclarationInspectionResult>];
            mandatory env_expr_to_string_raw => "lean_rs_host_env_expr_to_string_raw"
                => [(LeanExpr<'lean>,)] => [String];
            mandatory elaborate => "lean_rs_host_elaborate"
                => [(Obj<'lean>, String, Option<LeanExpr<'lean>>, String, String, u64, usize)]
                => [LeanIo<Result<LeanExpr<'lean>, LeanElabFailure>>];
            mandatory elaborate_bulk => "lean_rs_host_elaborate_bulk"
                => [(Obj<'lean>, Vec<String>, String, String, u64, usize)]
                => [LeanIo<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>>];
            mandatory elaborate_bulk_progress => "lean_rs_host_elaborate_bulk_progress"
                => [(Obj<'lean>, Vec<String>, String, String, u64, usize, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory kernel_check => "lean_rs_host_kernel_check"
                => [(Obj<'lean>, String, String, String, u64, usize)] => [LeanIo<LeanKernelOutcome<'lean>>];
            mandatory kernel_check_progress => "lean_rs_host_kernel_check_progress"
                => [(Obj<'lean>, String, String, String, u64, usize, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory check_evidence => "lean_rs_host_check_evidence"
                => [(Obj<'lean>, LeanEvidence<'lean>)] => [LeanIo<EvidenceStatus>];
            mandatory evidence_summary => "lean_rs_host_evidence_summary"
                => [(Obj<'lean>, LeanEvidence<'lean>)] => [LeanIo<ProofSummary>];
            optional meta_infer_type => "lean_rs_host_meta_infer_type"
                => [(Obj<'lean>, LeanExpr<'lean>, u64, usize, u8)] => [LeanIo<LeanMetaResponse<LeanExpr<'lean>>>];
            optional meta_whnf => "lean_rs_host_meta_whnf"
                => [(Obj<'lean>, LeanExpr<'lean>, u64, usize, u8)] => [LeanIo<LeanMetaResponse<LeanExpr<'lean>>>];
            optional meta_heartbeat_burn => "lean_rs_host_meta_heartbeat_burn"
                => [(Obj<'lean>, LeanExpr<'lean>, u64, usize, u8)] => [LeanIo<LeanMetaResponse<LeanExpr<'lean>>>];
            optional meta_is_def_eq => "lean_rs_host_meta_is_def_eq"
                => [(Obj<'lean>, (LeanExpr<'lean>, LeanExpr<'lean>, LeanMetaTransparency), u64, usize, u8)]
                => [LeanIo<LeanMetaResponse<bool>>];
            optional meta_pp_expr => "lean_rs_host_meta_pp_expr"
                => [(Obj<'lean>, LeanExpr<'lean>, u64, usize, u8)] => [LeanIo<LeanMetaResponse<String>>];
            optional process_module_query => "lean_rs_host_process_module_query"
                => [(Obj<'lean>, String, ModuleQuery, String, String, u64, usize)] => [LeanIo<ModuleQueryOutcome>];
            optional process_module_query_batch => "lean_rs_host_process_module_query_batch"
                => [(
                    Obj<'lean>,
                    String,
                    Vec<ModuleQuerySelector>,
                    ModuleQueryOutputBudgets,
                    String,
                    String,
                    u64,
                    usize,
                )] => [LeanIo<ModuleQueryBatchOutcome>];
            optional process_module_query_batch_cached => "lean_rs_host_process_module_query_batch_cached"
                => [(
                    Obj<'lean>,
                    String,
                    Vec<ModuleQuerySelector>,
                    ModuleQueryOutputBudgets,
                    String,
                    String,
                    u64,
                    usize,
                    String,
                )] => [LeanIo<ModuleQueryBatchCachedOutcome>];
            optional attempt_proof => "lean_rs_host_attempt_proof"
                => [(Obj<'lean>, ProofAttemptRequest, String, String, u64, usize)]
                => [LeanIo<ProofAttemptOutcome>];
            optional verify_declaration => "lean_rs_host_verify_declaration"
                => [(Obj<'lean>, DeclarationVerificationRequest, String, String, u64, usize)]
                => [LeanIo<DeclarationVerificationOutcome>];
            optional verify_declaration_batch => "lean_rs_host_verify_declaration_batch"
                => [(Obj<'lean>, DeclarationVerificationBatchRequest, String, String, u64, usize)]
                => [LeanIo<DeclarationVerificationBatchOutcome>];
            optional clear_module_snapshot_cache => "lean_rs_host_clear_module_snapshot_cache"
                => [()] => [LeanIo<ModuleSnapshotCacheClearResult>];
        }
    };
}

macro_rules! binding_type {
    (mandatory, $args:ty, $ret:ty) => {
        LeanExported<'lean, 'cap, $args, $ret>
    };
    (optional, $args:ty, $ret:ty) => {
        Option<LeanExported<'lean, 'cap, $args, $ret>>
    };
}

macro_rules! resolve_binding {
    (mandatory, $capability:expr, $symbol:literal, $args:ty, $ret:ty) => {
        resolve_required::<$args, $ret>($capability, $symbol)
    };
    (optional, $capability:expr, $symbol:literal, $args:ty, $ret:ty) => {
        resolve_optional::<$args, $ret>($capability, $symbol)
    };
}

macro_rules! define_host_shim_bindings {
    ($($kind:ident $field:ident => $symbol:literal => [$args:ty] => [$ret:ty];)*) => {
        /// Typed call handles for the bundled host shim exports.
        pub(crate) struct HostShimBindings<'lean, 'cap> {
            $(
                pub(crate) $field: binding_type!($kind, $args, $ret),
            )*
        }

        impl<'lean, 'cap> HostShimBindings<'lean, 'cap> {
            /// Resolve every bundled host shim through manifest-checked lookup.
            pub(crate) fn resolve(capability: &'cap LeanCapability<'lean>) -> Result<Self, HostShimBindingError> {
                Ok(Self {
                    $(
                        $field: resolve_binding!($kind, capability, $symbol, $args, $ret)?,
                    )*
                })
            }
        }

        /// Manifest signatures for every bundled host shim symbol Rust uses.
        #[allow(
            clippy::extra_unused_lifetimes,
            reason = "signature rows mention 'lean in the generated ABI types"
        )]
        pub(crate) fn host_shim_export_signatures<'lean>() -> Vec<LeanExportSignature> {
            vec![$(signature_for::<$args, $ret>($symbol)),*]
        }
    };
}

host_shim_exports!(define_host_shim_bindings);

/// Checked binding resolution failure.
#[derive(Debug)]
pub(crate) struct HostShimBindingError {
    symbol: &'static str,
    source: LeanCheckedExportError,
}

impl HostShimBindingError {
    fn new(symbol: &'static str, source: LeanCheckedExportError) -> Self {
        Self { symbol, source }
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &LeanCheckedExportError {
        &self.source
    }
}

impl std::fmt::Display for HostShimBindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to resolve checked bundled host shim '{}': {}",
            self.symbol, self.source
        )
    }
}

impl std::error::Error for HostShimBindingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn signature_for<'lean, Args, R>(symbol: &str) -> LeanExportSignature
where
    Args: LeanArgs<'lean>,
    R: DecodeCallResult<'lean>,
{
    LeanExportSignature::function(symbol, Args::export_abi_args(), R::export_abi_return())
}

fn resolve_required<'lean, 'cap, Args, R>(
    capability: &'cap LeanCapability<'lean>,
    symbol: &'static str,
) -> Result<LeanExported<'lean, 'cap, Args, R>, HostShimBindingError>
where
    Args: LeanArgs<'lean>,
    R: DecodeCallResult<'lean>,
{
    capability
        .exported::<Args, R>(symbol)
        .map_err(|err| HostShimBindingError::new(symbol, err))
}

fn resolve_optional<'lean, 'cap, Args, R>(
    capability: &'cap LeanCapability<'lean>,
    symbol: &'static str,
) -> Result<Option<LeanExported<'lean, 'cap, Args, R>>, HostShimBindingError>
where
    Args: LeanArgs<'lean>,
    R: DecodeCallResult<'lean>,
{
    match capability.exported::<Args, R>(symbol) {
        Ok(handle) => Ok(Some(handle)),
        Err(LeanCheckedExportError::MissingSymbol { .. }) => Ok(None),
        Err(err) => Err(HostShimBindingError::new(symbol, err)),
    }
}

pub(crate) fn binding_error_to_lean_error(err: &HostShimBindingError) -> lean_rs::LeanError {
    lean_rs::__host_internals::host_module_init(err.to_string())
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions should fail loudly and include precise fixture context"
)]
mod tests {
    use super::{HostShimBindings, host_shim_export_signatures};
    use lean_rs::LeanRuntime;
    use lean_rs::module::{LeanBuiltCapability, LeanCapability, LeanCheckedExportError};
    use lean_toolchain::{
        CapabilityManifest, LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention,
        LeanExportReturnAbi, LeanExportSignature,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn shim_source_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("shims")
            .join("lean-rs-host-shims")
            .join("LeanRsHostShims")
    }

    fn exported_symbols_from_sources() -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut stack = vec![shim_source_dir()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).expect("read shim source dir") {
                let entry = entry.expect("read shim source entry");
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(std::ffi::OsStr::to_str) != Some("lean") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read shim source");
                for line in text.lines() {
                    let Some(start) = line.find("@[export lean_rs_host_") else {
                        continue;
                    };
                    let after_export = &line[start + "@[export ".len()..];
                    let Some(end) = after_export.find(']') else {
                        continue;
                    };
                    out.insert(after_export[..end].to_owned());
                }
            }
        }
        out
    }

    fn runtime() -> &'static LeanRuntime {
        LeanRuntime::init().expect("Lean runtime initialisation must succeed")
    }

    #[test]
    fn generated_signature_table_matches_packaged_shim_exports() {
        let generated = host_shim_export_signatures()
            .into_iter()
            .map(|signature| signature.symbol().to_owned())
            .collect::<BTreeSet<_>>();
        let source = exported_symbols_from_sources();

        assert_eq!(generated, source);
    }

    #[test]
    fn bundled_shim_manifest_contains_generated_signatures() {
        let built = crate::host::lake::LakeProject::shim_capability(host_shim_export_signatures())
            .expect("bundled shim capability builds");
        let manifest = CapabilityManifest::read(built.manifest_path()).expect("manifest parses");
        let generated = signatures_by_symbol(host_shim_export_signatures());
        let manifest_exports = signatures_by_symbol(manifest.exports);

        assert_eq!(manifest_exports, generated);
    }

    #[test]
    fn binding_resolution_fails_on_manifest_signature_mismatch() {
        let built = crate::host::lake::LakeProject::shim_capability(host_shim_export_signatures())
            .expect("bundled shim capability builds");
        let manifest_path = mismatched_manifest_path();
        let mut manifest_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(built.manifest_path()).expect("read generated manifest"))
                .expect("manifest json parses");
        let mut signatures = host_shim_export_signatures();
        let mismatched = signatures
            .iter_mut()
            .find(|signature| signature.symbol() == "lean_rs_host_name_from_string")
            .expect("name_from_string signature is generated");
        *mismatched = wrong_name_from_string_signature();
        manifest_json["exports"] =
            serde_json::Value::Array(signatures.iter().map(LeanExportSignature::to_json).collect::<Vec<_>>());
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest_json).expect("encode mismatched manifest"),
        )
        .expect("write mismatched manifest");

        let capability =
            LeanCapability::from_build_manifest(runtime(), LeanBuiltCapability::manifest_path(&manifest_path))
                .expect("mismatched manifest opens before checked export lookup");
        let Err(err) = HostShimBindings::resolve(&capability) else {
            panic!("signature mismatch must fail binding construction");
        };

        assert!(
            matches!(
                err.source(),
                LeanCheckedExportError::SignatureMismatch { symbol, .. }
                    if symbol == "lean_rs_host_name_from_string"
            ),
            "expected name_from_string signature mismatch, got {err:?}",
        );

        drop(std::fs::remove_file(manifest_path));
    }

    fn mismatched_manifest_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "lean-rs-host-mismatched-shim-manifest-{}-{nanos}.json",
            std::process::id()
        ))
    }

    fn wrong_name_from_string_signature() -> LeanExportSignature {
        LeanExportSignature::function(
            "lean_rs_host_name_from_string",
            vec![LeanExportArgAbi::new(LeanExportAbiRepr::U8, LeanExportOwnership::None)],
            LeanExportReturnAbi::new(
                LeanExportAbiRepr::U8,
                LeanExportOwnership::None,
                LeanExportResultConvention::Pure,
            ),
        )
    }

    fn signatures_by_symbol(signatures: Vec<LeanExportSignature>) -> BTreeMap<String, LeanExportSignature> {
        signatures
            .into_iter()
            .map(|signature| (signature.symbol().to_owned(), signature))
            .collect()
    }
}
