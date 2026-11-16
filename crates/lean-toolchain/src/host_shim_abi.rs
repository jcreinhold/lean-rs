//! Source-owned host shim ABI metadata, with no Lean runtime linkage or I/O.
//!
//! One declaration table drives both real typed host bindings and this closed
//! ABI-only projection. All-row parity tests in lean-rs-host check it against
//! LeanArgs/DecodeCallResult, independently of these marker implementations.
use crate::LeanExportSignature;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Shared typed host export rows. The callback resolves host type names in
/// its own scope; the pure projection substitutes closed ABI-only markers.
#[doc(hidden)]
#[macro_export]
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
            mandatory extension_registry_epoch => "lean_rs_host_extension_registry_epoch"
                => [()] => [LeanIo<u64>];
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
                => [(Obj<'lean>, String, Option<LeanExpr<'lean>>, String, String, u64, usize, String)]
                => [LeanIo<Result<LeanExpr<'lean>, LeanElabFailure>>];
            mandatory elaborate_bulk => "lean_rs_host_elaborate_bulk"
                => [(Obj<'lean>, Vec<String>, String, String, u64, usize, String)]
                => [LeanIo<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>>];
            mandatory elaborate_bulk_progress => "lean_rs_host_elaborate_bulk_progress"
                => [(Obj<'lean>, Vec<String>, String, String, u64, usize, String, usize, usize)] => [LeanIo<Obj<'lean>>];
            mandatory kernel_check => "lean_rs_host_kernel_check"
                => [(Obj<'lean>, String, String, String, u64, usize, String)] => [LeanIo<LeanKernelOutcome<'lean>>];
            mandatory kernel_check_progress => "lean_rs_host_kernel_check_progress"
                => [(Obj<'lean>, String, String, String, u64, usize, String, usize, usize)] => [LeanIo<Obj<'lean>>];
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
                => [(Obj<'lean>, String, ModuleQuery, String, String, u64, usize, String)] => [LeanIo<ModuleQueryOutcome>];
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
                    String,
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
                    String,
                )] => [LeanIo<ModuleQueryBatchCachedOutcome>];
            optional attempt_proof => "lean_rs_host_attempt_proof"
                => [(Obj<'lean>, ProofAttemptRequest, String, String, u64, usize, String)]
                => [LeanIo<ProofAttemptOutcome>];
            optional verify_declaration => "lean_rs_host_verify_declaration"
                => [(Obj<'lean>, DeclarationVerificationRequest, String, String, u64, usize, String)]
                => [LeanIo<DeclarationVerificationOutcome>];
            optional verify_declaration_batch => "lean_rs_host_verify_declaration_batch"
                => [(Obj<'lean>, DeclarationVerificationBatchRequest, String, String, u64, usize, String)]
                => [LeanIo<DeclarationVerificationBatchOutcome>];
            optional configure_header_import_budget => "lean_rs_host_configure_header_import_budget"
                => [(u64, u64)] => [LeanIo<()>];
            optional header_import_state => "lean_rs_host_header_import_state"
                => [()] => [LeanIo<String>];
            optional clear_module_header_cache => "lean_rs_host_clear_module_header_cache"
                => [(Obj<'lean>,)] => [LeanIo<()>];
            optional runtime_version => "lean_rs_host_runtime_version"
                => [()] => [LeanIo<String>];
            optional clear_module_snapshot_cache => "lean_rs_host_clear_module_snapshot_cache"
                => [()] => [LeanIo<ModuleSnapshotCacheClearResult>];
        }
    };
}

/// Canonical fingerprint shared by compiled expectations and loaded receipts.
///
/// Object keys are sorted recursively, independently of `serde_json`'s optional
/// `preserve_order` feature. No file or actual loaded manifest is consulted.
#[must_use]
pub fn checked_binding_fingerprint(signature: &serde_json::Value) -> String {
    fn ordered(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(fields) => {
                let sorted: BTreeMap<_, _> = fields.iter().collect();
                let mut result = serde_json::Map::new();
                for (key, value) in sorted {
                    result.insert(key.clone(), ordered(value));
                }
                serde_json::Value::Object(result)
            }
            serde_json::Value::Array(values) => serde_json::Value::Array(values.iter().map(ordered).collect()),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => value.clone(),
        }
    }
    let bytes = ordered(signature).to_string();
    let mut fingerprint = String::with_capacity(64);
    for byte in Sha256::digest(bytes.as_bytes()) {
        let _ = write!(fingerprint, "{byte:02x}");
    }
    fingerprint
}

/// Compiled expectations for every host row, used to validate the projection.
#[doc(hidden)]
#[must_use]
pub fn compiled_host_binding_signatures() -> BTreeMap<String, LeanExportSignature> {
    projected::signatures()
}

/// The proof workflow's required checked bindings, available before first load.
///
/// Selection names reference generated fields, so removing a required export
/// cannot silently produce an incomplete expected contract.
#[must_use]
pub fn expected_checked_bindings() -> BTreeMap<String, String> {
    let bindings = projected::bindings();
    BTreeMap::from([
        (
            "process_module_query_batch_cached".to_owned(),
            checked_binding_fingerprint(&bindings.process_module_query_batch_cached.to_json()),
        ),
        (
            "verify_declaration_batch".to_owned(),
            checked_binding_fingerprint(&bindings.verify_declaration_batch.to_json()),
        ),
        (
            "runtime_version".to_owned(),
            checked_binding_fingerprint(&bindings.runtime_version.to_json()),
        ),
    ])
}

mod projected {
    use super::{BTreeMap, LeanExportSignature};
    use crate::{
        LeanExportAbiRepr as Repr, LeanExportArgAbi, LeanExportOwnership as Ownership,
        LeanExportResultConvention as Convention, LeanExportReturnAbi,
    };
    use std::marker::PhantomData;
    struct Object<'lean>(PhantomData<&'lean ()>);
    type Obj<'lean> = Object<'lean>;
    type LeanName<'lean> = Object<'lean>;
    type LeanExpr<'lean> = Object<'lean>;
    type LeanEvidence<'lean> = Object<'lean>;
    type LeanDeclaration<'lean> = Object<'lean>;
    type LeanKernelOutcome<'lean> = Object<'lean>;
    struct LeanIo<T>(PhantomData<T>);
    type LeanMetaResponse<T> = LeanIo<T>;
    macro_rules! boxed_types {
        ($($name:ident),* $(,)?)=>{$(type $name=Object<'static>;)*};
    }
    boxed_types! {
        DeclarationInspectionRequest,DeclarationInspectionResult,DeclarationSearchRequest,DeclarationSearchResult,
        DeclarationVerificationBatchOutcome,DeclarationVerificationBatchRequest,DeclarationVerificationOutcome,
        DeclarationVerificationRequest,EvidenceStatus,LeanDeclarationFilter,LeanElabFailure,LeanImportStats,
        LeanMetaTransparency,LeanSourceRange,ModuleQuery,ModuleQueryBatchCachedOutcome,ModuleQueryBatchOutcome,
        ModuleQueryOutcome,ModuleQueryOutputBudgets,ModuleQuerySelector,ModuleSnapshotCacheClearResult,
        ProofAttemptOutcome,ProofAttemptRequest,ProofSummary,
    }
    trait Argument {
        const REPR: Repr;
    }
    impl Argument for Object<'_> {
        const REPR: Repr = Repr::LeanObject;
    }
    impl Argument for String {
        const REPR: Repr = Repr::LeanObject;
    }
    impl<T> Argument for Vec<T> {
        const REPR: Repr = Repr::LeanObject;
    }
    impl<T> Argument for Option<T> {
        const REPR: Repr = Repr::LeanObject;
    }
    impl<A, B, C> Argument for (A, B, C) {
        const REPR: Repr = Repr::LeanObject;
    }
    impl Argument for bool {
        const REPR: Repr = Repr::U8;
    }
    impl Argument for u8 {
        const REPR: Repr = Repr::U8;
    }
    impl Argument for u64 {
        const REPR: Repr = Repr::U64;
    }
    impl Argument for usize {
        const REPR: Repr = Repr::USize;
    }
    fn ownership(repr: Repr) -> Ownership {
        if repr == Repr::LeanObject {
            Ownership::Owned
        } else {
            Ownership::None
        }
    }
    trait Arguments {
        fn abi() -> Vec<LeanExportArgAbi>;
    }
    impl Arguments for () {
        fn abi() -> Vec<LeanExportArgAbi> {
            Vec::new()
        }
    }
    macro_rules! arguments {
        ($($name:ident),+)=>{
            impl<$($name:Argument),+> Arguments for ($($name,)+) {
                fn abi()->Vec<LeanExportArgAbi> { vec![$(LeanExportArgAbi::new($name::REPR,ownership($name::REPR))),+] }
            }
        };
    }
    arguments!(A);
    arguments!(A, B);
    arguments!(A, B, C);
    arguments!(A, B, C, D);
    arguments!(A, B, C, D, E);
    arguments!(A, B, C, D, E, F);
    arguments!(A, B, C, D, E, F, G);
    arguments!(A, B, C, D, E, F, G, H);
    arguments!(A, B, C, D, E, F, G, H, I);
    arguments!(A, B, C, D, E, F, G, H, I, J);
    trait Return {
        fn abi() -> LeanExportReturnAbi;
    }
    impl<T> Return for LeanIo<T> {
        fn abi() -> LeanExportReturnAbi {
            LeanExportReturnAbi::new(Repr::LeanObject, Ownership::Owned, Convention::IoResult)
        }
    }
    impl Return for String {
        fn abi() -> LeanExportReturnAbi {
            LeanExportReturnAbi::new(Repr::LeanObject, Ownership::Owned, Convention::Pure)
        }
    }
    impl Return for Object<'_> {
        fn abi() -> LeanExportReturnAbi {
            LeanExportReturnAbi::new(Repr::LeanObject, Ownership::Owned, Convention::Pure)
        }
    }
    fn signature<A: Arguments, R: Return>(symbol: &str) -> LeanExportSignature {
        LeanExportSignature::function(symbol, A::abi(), R::abi())
    }
    macro_rules! project {
        ($($kind:ident $field:ident => $symbol:literal => [$args:ty] => [$ret:ty];)*)=>{
            pub(super) struct Bindings { $(pub(super) $field:LeanExportSignature,)* }
            #[allow(clippy::extra_unused_lifetimes,reason="shared typed rows use 'lean in ABI-only marker types")]
            pub(super) fn bindings<'lean>()->Bindings { Bindings { $($field:signature::<$args,$ret>($symbol),)* } }
            pub(super) fn signatures()->BTreeMap<String,LeanExportSignature> {
                let bindings=bindings();
                BTreeMap::from([$( (stringify!($field).to_owned(),bindings.$field), )*])
            }
        };
    }
    crate::host_shim_exports!(project);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_contract_is_complete_and_source_owned() {
        let expected = expected_checked_bindings();
        assert_eq!(
            expected.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "process_module_query_batch_cached",
                "runtime_version",
                "verify_declaration_batch"
            ]
        );
        assert!(
            expected
                .values()
                .all(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        );
    }
    #[test]
    fn signature_hash_is_independent_of_object_insertion_order() {
        let mut left = serde_json::Map::new();
        left.insert("z".to_owned(), serde_json::json!({"b":2,"a":1}));
        left.insert("a".to_owned(), serde_json::json!([1, 2]));
        let mut right = serde_json::Map::new();
        right.insert("a".to_owned(), serde_json::json!([1, 2]));
        right.insert("z".to_owned(), serde_json::json!({"a":1,"b":2}));
        assert_eq!(
            checked_binding_fingerprint(&serde_json::Value::Object(left)),
            checked_binding_fingerprint(&serde_json::Value::Object(right))
        );
    }
}
