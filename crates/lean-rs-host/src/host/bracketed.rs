//! Bracketed no-extension imports for one-shot lightweight queries.
//!
//! This module intentionally does not expose `Lean.Environment`, `Expr`,
//! `ConstantInfo`, `Name`, or any other Lean-owned handle. The Lean shim imports
//! with `loadExts := false`, serializes the requested declaration metadata, frees
//! compacted regions, and only then returns these Rust-owned values.

use lean_rs::Obj;
use lean_rs::abi::structure::{alloc_ctor_with_objects, take_ctor_objects, view};
use lean_rs::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error, sealed};
use lean_rs::error::LeanResult;
use serde::Deserialize;

use crate::host::capabilities::LeanCapabilities;
use crate::host::progress::{LeanProgressSink, ProgressBridge};
use crate::host::session::{LeanSession, with_session_import_lock};
use crate::host::shim_bindings::{HostShimBindings, binding_error_to_lean_error};

/// Closed request for the bracketed no-extension import path.
///
/// The request is intentionally limited to declaration metadata lookups by
/// name. It does not admit parser, elaborator, pretty-printer, extension, or
/// arbitrary callback work because the imported compacted regions are freed
/// before Rust receives the result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanBracketedImportRequest {
    /// Fully-qualified declaration names to inspect inside the bracket.
    pub declaration_names: Vec<String>,
}

impl LeanBracketedImportRequest {
    #[must_use]
    pub fn new(declaration_names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            declaration_names: declaration_names.into_iter().map(Into::into).collect(),
        }
    }
}

impl<'lean> IntoLean<'lean> for LeanBracketedImportRequest {
    fn into_lean(self, runtime: &'lean lean_rs::LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(runtime, 0, [self.declaration_names.into_lean(runtime)])
    }
}

impl sealed::SealedAbi for LeanBracketedImportRequest {}

impl<'lean> LeanAbi<'lean> for LeanBracketedImportRequest {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean lean_rs::LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean lean_rs::LeanRuntime) -> LeanResult<Self> {
        Err(conversion_error(
            "LeanBracketedImportRequest cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

/// Result of one bracketed no-extension import query.
///
/// Every field is Rust-owned data decoded from the Lean shim's serialized
/// payload. No Lean-owned `Environment`, `Expr`, `Name`, `ConstantInfo`, or
/// extension state escapes the `loadExts := false` / `freeRegions` bracket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanBracketedImportResult {
    /// Import attribution captured before the compacted regions are freed.
    pub import_stats: crate::host::session::LeanImportStats,
    /// Per-request declaration metadata serialized inside the bracket.
    pub declarations: Vec<LeanBracketedDeclarationInfo>,
    /// Operations this path deliberately refuses because they require a normal
    /// full session with loaded environment extensions.
    pub rejected_operations: Vec<LeanBracketedRejectedOperation>,
    /// Whether the Lean shim reached the `Environment.freeRegions` cleanup
    /// point before returning the serialized result to Rust.
    pub free_regions_ran: bool,
}

impl<'lean> TryFromLean<'lean> for LeanBracketedImportResult {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let free_regions_ran = {
            let ctor = view(&obj).ctor_shape(0, 3, "BracketedImportResult")?;
            ctor.bool(0, "BracketedImportResult.freeRegionsRan")?
        };
        let [import_stats, declarations, rejected_operations] =
            take_ctor_objects::<3>(obj, 0, "BracketedImportResult")?;
        Ok(Self {
            import_stats: crate::host::session::LeanImportStats::try_from_lean(import_stats)?,
            declarations: Vec::<LeanBracketedDeclarationInfo>::try_from_lean(declarations)?,
            rejected_operations: Vec::<LeanBracketedRejectedOperation>::try_from_lean(rejected_operations)?,
            free_regions_ran,
        })
    }
}

impl LeanBracketedImportResult {
    pub(crate) fn query(
        capabilities: &LeanCapabilities<'_, '_>,
        imports: &[&str],
        request: LeanBracketedImportRequest,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Self> {
        let search_paths = LeanSession::import_search_paths(capabilities)?;
        let imports_owned: Vec<String> = imports.iter().map(|&import| import.to_owned()).collect();
        let declaration_names = request.declaration_names;
        with_session_import_lock(|| {
            let shims = HostShimBindings::resolve(capabilities.shim_capability())
                .map_err(|err| binding_error_to_lean_error(&err))?;
            if let Some(sink) = progress {
                let bridge = ProgressBridge::new(sink, "bracketed-import", Some(3))?;
                let (handle, trampoline) = bridge.abi_parts();
                let raw = shims.bracketed_import_query.call(
                    search_paths,
                    imports_owned,
                    declaration_names,
                    handle,
                    trampoline,
                )?;
                Self::from_json(&bridge.decode::<String>(raw)?)
            } else {
                let raw = shims
                    .bracketed_import_query
                    .call(search_paths, imports_owned, declaration_names, 0, 0)?;
                match Result::<String, u8>::try_from_lean(raw)? {
                    Ok(json) => Self::from_json(&json),
                    Err(status) => Err(lean_rs::__host_internals::host_internal(format!(
                        "bracketed import query returned callback status {status} without a registered callback"
                    ))),
                }
            }
        })
    }

    fn from_json(raw: &str) -> LeanResult<Self> {
        let wire: WireBracketedImportResult = serde_json::from_str(raw).map_err(|err| {
            lean_rs::__host_internals::host_internal(format!("bracketed import query returned malformed JSON: {err}"))
        })?;
        Ok(wire.into())
    }
}

/// Serialized declaration metadata produced inside the bracket.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct LeanBracketedDeclarationInfo {
    pub name: String,
    pub exists: bool,
    pub kind: Option<String>,
    pub module: Option<String>,
    pub raw_type: Option<String>,
}

impl<'lean> TryFromLean<'lean> for LeanBracketedDeclarationInfo {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let exists = {
            let ctor = view(&obj).ctor_shape(0, 4, "BracketedDeclarationInfo")?;
            ctor.bool(0, "BracketedDeclarationInfo.existsDecl")?
        };
        let [name, kind, module, raw_type] = take_ctor_objects::<4>(obj, 0, "BracketedDeclarationInfo")?;
        Ok(Self {
            name: String::try_from_lean(name)?,
            exists,
            kind: Option::<String>::try_from_lean(kind)?,
            module: Option::<String>::try_from_lean(module)?,
            raw_type: Option::<String>::try_from_lean(raw_type)?,
        })
    }
}

/// Candidate operation deliberately excluded from the bracketed API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct LeanBracketedRejectedOperation {
    pub operation: String,
    pub reason: String,
}

impl<'lean> TryFromLean<'lean> for LeanBracketedRejectedOperation {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [operation, reason] = take_ctor_objects::<2>(obj, 0, "BracketedRejectedOperation")?;
        Ok(Self {
            operation: String::try_from_lean(operation)?,
            reason: String::try_from_lean(reason)?,
        })
    }
}

#[derive(Deserialize)]
struct WireBracketedImportResult {
    import_stats: WireImportStats,
    declarations: Vec<LeanBracketedDeclarationInfo>,
    rejected_operations: Vec<LeanBracketedRejectedOperation>,
    free_regions_ran: bool,
}

impl From<WireBracketedImportResult> for LeanBracketedImportResult {
    fn from(value: WireBracketedImportResult) -> Self {
        Self {
            import_stats: value.import_stats.into(),
            declarations: value.declarations,
            rejected_operations: value.rejected_operations,
            free_regions_ran: value.free_regions_ran,
        }
    }
}

#[derive(Deserialize)]
struct WireImportStats {
    direct_import_names: Vec<String>,
    effective_module_count: u64,
    compacted_region_count: u64,
    memory_mapped_region_count: u64,
    compacted_region_bytes: u64,
    memory_mapped_region_bytes: u64,
    non_memory_mapped_region_bytes: u64,
    imported_bytes: u64,
    imported_constant_count: u64,
    extension_count: u64,
    total_imported_extension_entries: u64,
    import_level: String,
    import_all: bool,
    load_exts: bool,
}

impl From<WireImportStats> for crate::host::session::LeanImportStats {
    fn from(value: WireImportStats) -> Self {
        Self {
            direct_import_names: value.direct_import_names,
            effective_module_count: value.effective_module_count,
            compacted_region_count: value.compacted_region_count,
            memory_mapped_region_count: value.memory_mapped_region_count,
            compacted_region_bytes: value.compacted_region_bytes,
            memory_mapped_region_bytes: value.memory_mapped_region_bytes,
            non_memory_mapped_region_bytes: value.non_memory_mapped_region_bytes,
            imported_bytes: value.imported_bytes,
            imported_constant_count: value.imported_constant_count,
            extension_count: value.extension_count,
            total_imported_extension_entries: value.total_imported_extension_entries,
            import_level: value.import_level,
            import_all: value.import_all,
            load_exts: value.load_exts,
        }
    }
}
