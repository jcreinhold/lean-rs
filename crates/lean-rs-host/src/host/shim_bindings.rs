//! Checked typed bindings for the bundled `lean-rs-host` shim exports.
//!
//! The shared `lean_toolchain::host_shim_exports!` declaration rows are the
//! single Rust source of truth for this C-ABI surface. They generate this
//! module's artifact-manifest signatures and cached typed call handles, plus
//! link-free cold-start expectations. All-row tests compare the independent
//! real typed traits with that ABI-only projection.

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

macro_rules! binding_present {
    (mandatory, $binding:expr) => {
        true
    };
    (optional, $binding:expr) => {
        $binding.is_some()
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
            /// Signatures exist only for successfully checked typed handles.
            pub(crate) fn checked_signatures(&self) -> std::collections::BTreeMap<String, serde_json::Value> {
                let mut signatures = std::collections::BTreeMap::new();
                $(
                    if binding_present!($kind, self.$field) {
                        signatures.insert(stringify!($field).to_owned(), signature_for::<$args, $ret>($symbol).to_json());
                    }
                )*
                signatures
            }

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

lean_toolchain::host_shim_exports!(define_host_shim_bindings);

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

    #[test]
    fn compiled_link_free_projection_matches_every_real_typed_host_signature() {
        let actual = signatures_by_symbol(host_shim_export_signatures());
        let projected = lean_toolchain::host_shim_abi::compiled_host_binding_signatures();
        let projected_by_symbol = signatures_by_symbol(projected.values().cloned().collect());
        assert_eq!(actual, projected_by_symbol);
        let expected = lean_toolchain::host_shim_abi::expected_checked_bindings();
        for (operation, fingerprint) in expected {
            let signature = projected.get(&operation).expect("required operation is generated");
            let typed = actual.get(signature.symbol()).expect("real typed signature exists");
            assert_eq!(
                fingerprint,
                lean_toolchain::host_shim_abi::checked_binding_fingerprint(&typed.to_json())
            );
        }
    }

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
