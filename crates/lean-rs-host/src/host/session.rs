//! `LeanSession`—a long-lived Lean session over an imported
//! environment.
//!
//! A [`LeanSession`] holds an imported `Lean.Environment` value (as an
//! opaque `Obj<'lean>`) plus a borrow of its parent
//! [`crate::host::LeanCapabilities`]. Each typed query method
//! ([`LeanSession::query_declaration`], …) dispatches through a
//! manifest-checked typed host-shim binding cached on the session—one
//! struct-field read, one FFI call, no per-query `dlsym`.
//!
//! ## Capability contract
//!
//! The bundled host shim dylib that [`crate::host::LeanCapabilities`] loads
//! exports thirty-two **mandatory** `@[export]` symbols and may export ten
//! **optional** symbols (checked when session bindings are constructed)—
//! five bounded `MetaM` services plus declaration inspection, module-query
//! entry points, and cache control:
//!
//! | C symbol                                               | Mandatory? | Lean signature                                                                                                                                                       |
//! | ------------------------------------------------------ | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
//! | `lean_rs_host_session_import`                          | yes        | `Array String -> Array String -> IO Environment`                                                                                                                           |
//! | `lean_rs_host_session_import_paths_v1`                 | optional   | Profiled import or comparison returning `IO (Option Environment × Array String)` |
//! | `lean_rs_host_session_import_plugin_v1`                | optional   | Same result with one application-attested plugin |
//! | `lean_rs_host_session_import_progress`                 | yes        | `Array String -> Array String -> USize -> USize -> IO (Except UInt8 Environment)`                                                                                    |
//! | `lean_rs_host_session_import_profile`                  | yes        | `Array String -> Array String -> Bool -> UInt8 -> Bool -> Bool -> Bool -> String -> IO Environment`                                                                 |
//! | `lean_rs_host_session_import_profile_progress`         | yes        | `Array String -> Array String -> Bool -> UInt8 -> USize -> USize -> IO (Except UInt8 Environment)`                                                                  |
//! | `lean_rs_host_env_import_stats`                        | yes        | `Environment -> String -> Bool -> IO ImportStats`                                                                                                                   |
//! | `lean_rs_host_bracketed_import_query`                  | yes        | `Array String -> Array String -> Array String -> USize -> USize -> IO (Except UInt8 String)`                                                                        |
//! | `lean_rs_host_name_from_string`                        | yes        | `String -> Name`                                                                                                                                                     |
//! | `lean_rs_host_name_to_string`                          | yes        | `Name -> String`                                                                                                                                                     |
//! | `lean_rs_host_env_query_declaration`                   | yes        | `Environment -> Name -> IO (Option Declaration)`                                                                                                                     |
//! | `lean_rs_host_env_query_declarations_bulk`             | yes        | `Environment -> Array Name -> IO (Array (Option Declaration))`                                                                                                       |
//! | `lean_rs_host_env_query_declarations_bulk_progress`    | yes        | `Environment -> Array Name -> USize -> USize -> IO (Except UInt8 (Array (Option Declaration)))`                                                                      |
//! | `lean_rs_host_env_list_declarations`                   | yes        | `Environment -> IO (Array Name)`                                                                                                                                     |
//! | `lean_rs_host_env_list_declarations_filtered`          | yes        | `Environment -> DeclarationFilter -> IO (Array Name)`                                                                                                                |
//! | `lean_rs_host_env_list_declarations_filtered_progress` | yes        | `Environment -> DeclarationFilter -> USize -> USize -> IO (Except UInt8 (Array Name))`                                                                               |
//! | `lean_rs_host_env_declaration_source_range`            | yes        | `Environment -> Name -> Array String -> IO (Option SourceRange)`                                                                                                     |
//! | `lean_rs_host_env_declaration_type`                    | yes        | `Environment -> Name -> IO (Option Expr)`                                                                                                                            |
//! | `lean_rs_host_env_declaration_type_bulk`               | yes        | `Environment -> Array String -> IO (Array (Option Expr))`                                                                                                            |
//! | `lean_rs_host_env_declaration_type_bulk_progress`      | yes        | `Environment -> Array String -> USize -> USize -> IO (Except UInt8 (Array (Option Expr)))`                                                                           |
//! | `lean_rs_host_env_declaration_kind`                    | yes        | `Environment -> Name -> IO String`                                                                                                                                   |
//! | `lean_rs_host_env_declaration_kind_bulk`               | yes        | `Environment -> Array String -> IO (Array String)`                                                                                                                   |
//! | `lean_rs_host_env_declaration_kind_bulk_progress`      | yes        | `Environment -> Array String -> USize -> USize -> IO (Except UInt8 (Array String))`                                                                                  |
//! | `lean_rs_host_env_declaration_name`                    | yes        | `Environment -> Name -> IO String`                                                                                                                                   |
//! | `lean_rs_host_env_declaration_name_bulk`               | yes        | `Environment -> Array String -> IO (Array String)`                                                                                                                   |
//! | `lean_rs_host_env_declaration_name_bulk_progress`      | yes        | `Environment -> Array String -> USize -> USize -> IO (Except UInt8 (Array String))`                                                                                  |
//! | `lean_rs_host_env_search_declarations`                 | yes        | `Environment -> DeclarationSearchRequest -> Array String -> IO DeclarationSearchResult`                                                                              |
//! | `lean_rs_host_env_inspect_declaration`                 | optional   | `Environment -> DeclarationInspectionRequest -> Array String -> IO DeclarationInspectionResult`                                                                       |
//! | `lean_rs_host_env_expr_to_string_raw`                  | yes        | `Expr -> String`                                                                                                                                                     |
//! | `lean_rs_host_elaborate`                               | yes        | `Environment -> String -> Option Expr -> String -> String -> UInt64 -> USize -> IO (Except ElabFailure Expr)`                                                        |
//! | `lean_rs_host_elaborate_bulk`                          | yes        | `Environment -> Array String -> String -> String -> UInt64 -> USize -> IO (Array (Except ElabFailure Expr))`                                                         |
//! | `lean_rs_host_elaborate_bulk_progress`                 | yes        | `Environment -> Array String -> String -> String -> UInt64 -> USize -> USize -> USize -> IO (Except UInt8 (Array (Except ElabFailure Expr)))`                        |
//! | `lean_rs_host_kernel_check`                            | yes        | `Environment -> String -> String -> String -> UInt64 -> USize -> IO KernelOutcome`                                                                                   |
//! | `lean_rs_host_kernel_check_progress`                   | yes        | `Environment -> String -> String -> String -> UInt64 -> USize -> USize -> USize -> IO (Except UInt8 KernelOutcome)`                                                  |
//! | `lean_rs_host_check_evidence`                          | yes        | `Environment -> Evidence -> IO EvidenceStatus`                                                                                                                       |
//! | `lean_rs_host_evidence_summary`                        | yes        | `Environment -> Evidence -> IO ProofSummary`                                                                                                                         |
//! | `lean_rs_host_meta_infer_type`                         | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                                                                          |
//! | `lean_rs_host_meta_whnf`                               | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                                                                          |
//! | `lean_rs_host_meta_heartbeat_burn`                     | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                                                                          |
//! | `lean_rs_host_meta_is_def_eq`                          | optional   | `Environment -> (Expr × Expr × UInt8) -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Bool)`                                                                         |
//! | `lean_rs_host_meta_pp_expr`                            | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse String)`                                                                                        |
//! | `lean_rs_host_process_module_query`                    | optional   | `Environment -> String -> ModuleQuery -> String -> String -> UInt64 -> USize -> IO ModuleQueryOutcome`                                                               |
//! | `lean_rs_host_process_module_query_batch`              | optional   | `Environment -> String -> Array ModuleQuerySelector -> ModuleQueryOutputBudgets -> String -> String -> UInt64 -> USize -> IO ModuleQueryBatchOutcome`                |
//! | `lean_rs_host_process_module_query_batch_cached`       | optional   | `Environment -> String -> Array ModuleQuerySelector -> ModuleQueryOutputBudgets -> String -> String -> UInt64 -> USize -> String -> IO ModuleQueryBatchCachedOutcome`|
//! | `lean_rs_host_verify_declaration_batch`                | optional   | `Environment -> DeclarationVerificationBatchRequest -> String -> String -> UInt64 -> USize -> IO DeclarationVerificationBatchOutcome`                                |
//! | `lean_rs_host_clear_module_snapshot_cache`             | optional   | `Unit -> IO ModuleSnapshotCacheClearResult`                                                                                                                          |
//!
//! Missing **mandatory** symbols surface at `load_capabilities` as
//! [`lean_rs::HostStage::Link`]—failures bind to the capability's load,
//! not to the first query. Missing **optional** symbols degrade
//! gracefully: [`LeanSession::run_meta`] returns
//! [`crate::host::meta::LeanMetaResponse::Unsupported`] against a service whose
//! binding did not resolve, [`LeanSession::process_module_query`]
//! returns [`crate::host::process::ModuleQueryOutcome::Unsupported`],
//! and the rest of the capability stays usable.
//! The evidence-side pair (`check_evidence`, `evidence_summary`) is
//! mandatory because any capability that produces a `LeanEvidence`
//! handle via `kernel_check` must also be able to re-validate and
//! summarize it: the missing-symbol case defines no recoverable
//! caller behaviour, so the error is folded into capability load
//! rather than into every call site.
//!
//! Capability contracts are extended additively over time: any future
//! capability symbol becomes a new mandatory or optional row in the
//! table above without renaming or removing existing ones.
//!
//! ## Per-session metrics
//!
//! Every [`LeanSession`] carries a [`SessionStats`] counter that
//! accumulates dispatch events (one FFI call per typed query, plus
//! per-item counts for the bulk methods) and the wall time spent inside
//! `.call(...)`. Snapshot via [`LeanSession::stats`]; reset by dropping
//! the session. `import` itself is **not** counted as a query FFI call;
//! [`LeanSession::import_stats`] reports what the import cost.
//!
//! ## Cancellation
//!
//! Every public method that can enter Lean accepts
//! `Option<&LeanCancellationToken>`. `None` keeps the fastest path and,
//! for bulk methods, keeps the single Lean-side bulk dispatch. `Some`
//! checks the token before host-controlled FFI dispatches; cancellable
//! bulk methods switch to per-item dispatch so they can also check
//! between items. Cancellation is cooperative and cannot interrupt a
//! Lean call already in progress.
//!
//! ## Progress
//!
//! Long-running session operations also accept
//! `Option<&dyn LeanProgressSink>`. `None` allocates no callback handle
//! and preserves the existing fast path. `Some(sink)` delivers
//! phase-local [`crate::host::progress::LeanProgressEvent`] values on
//! the Lean-bound worker thread. A progress sink must not call back into
//! the same session.
//!
//! The Rust side passes the `.olean` search path (resolved by
//! [`crate::host::lake::LakeProject`]) as the first argument to the
//! profile-aware import shim; the Lean shim only has to call
//! `Lean.initSearchPath` and `Lean.importModules` on it. Path-layout
//! knowledge stays in Rust.
//!
//! ## Lifetime story
//!
//! - `LeanSession<'lean, 'c>` borrows `&'c LeanCapabilities<'lean, '_>`.
//! - The session's owned `Obj<'lean>` is independent of `'c`; it carries
//!   one Lean refcount on the imported environment, anchored to the
//!   runtime.
//! - `HostShimBindings<'lean, 'c>` borrows from the manifest-backed shim
//!   capability owned by `LeanCapabilities`; its typed call handles live
//!   exactly as long as the session borrow.

// `run_meta` is `pub` but bounded on `lean_rs::abi::traits::{LeanAbi, TryFromLean}`.
// `LeanAbi` is sealed-public; `TryFromLean` is `pub(crate)`. The bound is a
// crate-internal compatibility requirement, not a downstream extension point
// (the meta-service registry is closed by `host::meta::service`). Same
// precedent as `module::exported.rs`.
#![allow(private_bounds, private_interfaces)]

use core::cell::Cell;
use std::time::Instant;

use crate::host::cancellation::{LeanCancellationToken, check_cancellation};
use crate::host::capabilities::LeanCapabilities;
use crate::host::declaration_search::{
    DeclarationInspectionRequest, DeclarationInspectionResult, DeclarationSearchRequest, DeclarationSearchResult,
};
use crate::host::elaboration::LeanElabFailure;
#[cfg(doc)]
use crate::host::elaboration::LeanElabOptions;
use crate::host::evidence::{EvidenceStatus, LeanEvidence, LeanKernelOutcome, ProofSummary};
use crate::host::meta::{LeanMetaOptions, LeanMetaResponse, LeanMetaService};
use crate::host::process::{
    DeclarationVerificationBatchOutcome, DeclarationVerificationOutcome, ModuleQueryBatchCachedOutcome,
    ModuleQueryBatchOutcome, ModuleQueryOutcome, ModuleSnapshotCacheClearResult, ProofAttemptOutcome,
};
use crate::host::progress::{LeanProgressSink, ProgressBridge, report_progress};
use lean_rs::Obj;
use lean_rs::abi::structure::{take_ctor_objects, view};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
#[cfg(doc)]
use lean_rs::error::HostStage;
use lean_rs::error::LeanResult;
use lean_rs::host_bridge::{HostEnvironment, HostShimBindings, with_host_import_transaction};
use lean_rs::module::{HostImportOptions, TrustedHostImports};
use lean_rs::{LeanDeclaration, LeanExpr, LeanName};

// -- SessionStats: per-session dispatch metrics --------------------------

/// Cumulative dispatch metrics for one [`LeanSession`].
///
/// Snapshot via [`LeanSession::stats`]. Each typed query method records
/// one FFI call; the bulk methods also record the per-item batch
/// size. `elapsed_ns` accumulates the wall time spent inside the inner
/// `.call(...)` dispatch (measured with [`Instant::now`])—it excludes
/// Rust-side argument marshaling, name lookup, and result decoding so
/// the number is comparable across singular and bulk paths.
///
/// `import` is **not** counted. Construction of a session always pays
/// for one import, and [`LeanSession::import_stats`] reports its cost.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionStats {
    /// Number of typed query FFI calls dispatched through this session,
    /// counting each singular call once and each bulk call once
    /// regardless of batch size.
    pub ffi_calls: u64,
    /// Cumulative number of per-item entries processed by the bulk
    /// methods. Singular calls do not contribute. A batch of N items
    /// adds N here and 1 to [`Self::ffi_calls`].
    pub batch_items: u64,
    /// Cumulative nanoseconds spent inside the dispatch `.call(...)`
    /// across every recorded FFI call.
    pub elapsed_ns: u64,
}

/// Lean-native attribution for the imported environment behind a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanImportStats {
    pub direct_import_names: Vec<String>,
    pub effective_module_count: u64,
    pub compacted_region_count: u64,
    pub memory_mapped_region_count: u64,
    pub compacted_region_bytes: u64,
    pub memory_mapped_region_bytes: u64,
    pub non_memory_mapped_region_bytes: u64,
    pub imported_bytes: u64,
    pub imported_constant_count: u64,
    pub extension_count: u64,
    pub total_imported_extension_entries: u64,
    pub import_level: String,
    pub import_all: bool,
    pub load_exts: bool,
}

impl<'lean> TryFromLean<'lean> for LeanImportStats {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let (
            effective_module_count,
            compacted_region_count,
            memory_mapped_region_count,
            compacted_region_bytes,
            memory_mapped_region_bytes,
            non_memory_mapped_region_bytes,
            imported_bytes,
            imported_constant_count,
            extension_count,
            total_imported_extension_entries,
            import_all,
            load_exts,
        ) = {
            let ctor = view(&obj).ctor_shape(0, 2, "ImportStats")?;
            (
                ctor.uint64(0, "ImportStats.effectiveModuleCount")?,
                ctor.uint64(8, "ImportStats.compactedRegionCount")?,
                ctor.uint64(16, "ImportStats.memoryMappedRegionCount")?,
                ctor.uint64(24, "ImportStats.compactedRegionBytes")?,
                ctor.uint64(32, "ImportStats.memoryMappedRegionBytes")?,
                ctor.uint64(40, "ImportStats.nonMemoryMappedRegionBytes")?,
                ctor.uint64(48, "ImportStats.importedBytes")?,
                ctor.uint64(56, "ImportStats.importedConstantCount")?,
                ctor.uint64(64, "ImportStats.extensionCount")?,
                ctor.uint64(72, "ImportStats.totalImportedExtensionEntries")?,
                ctor.bool(80, "ImportStats.importAll")?,
                ctor.bool(81, "ImportStats.loadExts")?,
            )
        };
        let [direct_import_names, import_level] = take_ctor_objects::<2>(obj, 0, "ImportStats")?;
        Ok(Self {
            direct_import_names: Vec::<String>::try_from_lean(direct_import_names)?,
            effective_module_count,
            compacted_region_count,
            memory_mapped_region_count,
            compacted_region_bytes,
            memory_mapped_region_bytes,
            non_memory_mapped_region_bytes,
            imported_bytes,
            imported_constant_count,
            extension_count,
            total_imported_extension_entries,
            import_level: String::try_from_lean(import_level)?,
            import_all,
            load_exts,
        })
    }
}

impl LeanImportStats {
    /// Stable compact attribution fragment for memory guardrail diagnostics.
    #[must_use]
    pub fn memory_diagnostic(&self) -> String {
        format!(
            "import_profile=level:{} import_all:{} load_exts:{} direct_import_count={} direct_imports={} effective_modules={} compacted_regions={} memory_mapped_regions={} compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} imported_constants={} extension_entries={}",
            self.import_level,
            self.import_all,
            self.load_exts,
            self.direct_import_names.len(),
            self.direct_import_names.join(","),
            self.effective_module_count,
            self.compacted_region_count,
            self.memory_mapped_region_count,
            self.compacted_region_bytes,
            self.memory_mapped_region_bytes,
            self.non_memory_mapped_region_bytes,
            self.imported_constant_count,
            self.total_imported_extension_entries
        )
    }
}

/// Closed import levels used by diagnostic profiling imports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeanImportLevel {
    Exported,
    Server,
    Private,
}

impl LeanImportLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exported => "exported",
            Self::Server => "server",
            Self::Private => "private",
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Exported => 0,
            Self::Server => 1,
            Self::Private => 2,
        }
    }
}

/// Closed import profiles for full host sessions.
///
/// Profiles intentionally expose names tied to host semantics rather than raw
/// Lean import knobs. All full-session profiles keep `loadExts := true`; the
/// no-extension variant remains a profiling-only diagnostic path and never
/// produces a normal [`LeanSession`]. Because full sessions load environment
/// extensions, they are not eligible for `Environment.freeRegions` cleanup; use
/// a worker process boundary to reset Lean import state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LeanSessionImportProfile {
    /// Public exported declarations only, with environment extensions loaded.
    ExportedPublic,
    /// Server-level import data without `import all`, with environment
    /// extensions loaded.
    Server,
    /// Private-level import data without `import all`, with environment
    /// extensions loaded.
    #[default]
    Private,
    /// Legacy compatibility import shape: `import all`, private level, with
    /// environment extensions loaded.
    FullPrivateCompat,
}

impl LeanSessionImportProfile {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ExportedPublic => "exported-public",
            Self::Server => "server",
            Self::Private => "private",
            Self::FullPrivateCompat => "full-private-compat",
        }
    }

    pub const fn import_all(self) -> bool {
        matches!(self, Self::FullPrivateCompat)
    }

    pub const fn import_level(self) -> LeanImportLevel {
        match self {
            Self::ExportedPublic => LeanImportLevel::Exported,
            Self::Server => LeanImportLevel::Server,
            Self::Private | Self::FullPrivateCompat => LeanImportLevel::Private,
        }
    }

    pub const fn load_exts(self) -> bool {
        true
    }
}

/// Closed import-mode matrix for profiling diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeanImportProfileMode {
    FullSession(LeanSessionImportProfile),
    ExportedNoExts,
}

impl LeanImportProfileMode {
    /// Exact native options to capture when attesting this import mode.
    #[must_use]
    pub fn native_options(self, profiler: &LeanImportProfilerOptions) -> HostImportOptions {
        HostImportOptions {
            import_all: self.import_all(),
            level: self.import_level().code(),
            load_extensions: self.load_exts(),
            profiler: profiler.profiler,
            trace_profiler: profiler.trace_profiler,
            trace_output: profiler.trace_profiler_output.clone().unwrap_or_default(),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::FullSession(profile) => profile.label(),
            Self::ExportedNoExts => "exported-no-exts",
        }
    }

    pub const fn import_all(self) -> bool {
        match self {
            Self::FullSession(profile) => profile.import_all(),
            Self::ExportedNoExts => false,
        }
    }

    pub const fn import_level(self) -> LeanImportLevel {
        match self {
            Self::FullSession(profile) => profile.import_level(),
            Self::ExportedNoExts => LeanImportLevel::Exported,
        }
    }

    pub const fn load_exts(self) -> bool {
        match self {
            Self::FullSession(profile) => profile.load_exts(),
            Self::ExportedNoExts => false,
        }
    }
}

/// Lean profiler toggles scoped to diagnostic profiling imports.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanImportProfilerOptions {
    pub profiler: bool,
    pub trace_profiler: bool,
    pub trace_profiler_output: Option<String>,
}

impl LeanImportProfilerOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn profiler(mut self, enabled: bool) -> Self {
        self.profiler = enabled;
        self
    }

    #[must_use]
    pub fn trace_profiler(mut self, enabled: bool) -> Self {
        self.trace_profiler = enabled;
        self
    }

    #[must_use]
    pub fn trace_profiler_output(mut self, path: impl Into<String>) -> Self {
        self.trace_profiler_output = Some(path.into());
        self
    }
}

// -- Public source-range / filter types ---------------------------------

/// Source range Lean recorded for a declaration.
///
/// Coordinates are 1-based at every layer, matching the public
/// convention of Lean declaration ranges. `file` is the path or module
/// label Lean/Rust could resolve for the declaration; it is a label for
/// consumers, not a normalized filesystem guarantee.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanSourceRange {
    /// File path or module label recorded for the declaration.
    pub file: String,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_column: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_column: u32,
}

impl<'lean> TryFromLean<'lean> for LeanSourceRange {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [file_o, start_line_o, start_column_o, end_line_o, end_column_o] =
            take_ctor_objects::<5>(obj, 0, "SourceRange")?;
        Ok(Self {
            file: String::try_from_lean(file_o)?,
            start_line: u32::try_from_lean(start_line_o)?,
            start_column: u32::try_from_lean(start_column_o)?,
            end_line: u32::try_from_lean(end_line_o)?,
            end_column: u32::try_from_lean(end_column_o)?,
        })
    }
}

pub use lean_rs::host_bridge::LeanDeclarationFilter;

// -- LeanSession ---------------------------------------------------------

/// A long-lived Lean session over an imported environment.
///
/// Construct via [`LeanCapabilities::session`]. The session owns the
/// imported `Lean.Environment` privately (never exposed) and dispatches
/// each typed query through checked host-shim bindings resolved during
/// construction. Neither [`Send`] nor [`Sync`]: inherited from the
/// contained `Obj<'lean>` and the borrow of `LeanCapabilities`.
pub struct LeanSession<'lean, 'c> {
    capabilities: &'c LeanCapabilities<'lean, 'c>,
    shims: HostShimBindings<'lean, 'c>,
    imports: TrustedHostImports<'lean, 'c>,
    /// The imported `Lean.Environment`. Private—Rust never inspects
    /// the environment directly; every query routes through a Lean
    /// capability export.
    environment: HostEnvironment<'lean>,
    import_stats: LeanImportStats,
    /// Value of the process-global extension-registration stamp at the moment
    /// this session's environment was imported. See
    /// [`Self::extension_registry_epoch`] for what a mismatch means.
    extension_registry_epoch: u64,
    /// Per-session dispatch metrics. `Cell` because every query method
    /// takes `&mut self` but the bulk path can also be invoked through a
    /// shared reference (e.g. inside a fold helper)—keeping the
    /// counter in `Cell` makes the recording uniform without adding an
    /// extra `&mut` borrow at each call site.
    stats: Cell<SessionStats>,
}

/// Actual metadata from a live host session's checked loaded bindings.
#[derive(Debug)]
pub struct LeanHostRuntimeMetadata {
    pub runtime_lean_version: Option<String>,
    pub checked_binding_signatures: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Cumulative source-header import allocations in this Lean process.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
pub struct HeaderImportState {
    /// Successful imported region bytes not backed by memory-mapped files.
    pub non_memory_mapped_bytes: u64,
    /// Fresh successful header imports; cache hits do not increment this.
    pub imports: u64,
    /// An import failed and may have left unmeasured allocations.
    pub accounting_unknown: bool,
    /// The resolver declined a fresh import before starting it.
    pub recycle_requested: bool,
}

#[derive(Clone, Copy)]
enum SessionImport<'a> {
    Ordinary(Option<&'a dyn LeanProgressSink>),
    ObserveRoots(Option<&'a [String]>),
}

fn comparable_roots(roots: &[String]) -> bool {
    roots.iter().all(|path| std::path::Path::new(path).is_absolute())
}

impl<'lean, 'c> LeanSession<'lean, 'c> {
    pub(crate) fn import_search_paths(capabilities: &LeanCapabilities<'lean, '_>) -> Vec<String> {
        let project_paths = capabilities.keyed_project_search_paths().map_or_else(
            || capabilities.host().project().olean_search_paths_for_output(None),
            <[std::path::PathBuf]>::to_vec,
        );
        let mut search_paths: Vec<String> = project_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        search_paths.extend(
            capabilities
                .shim_search_paths()
                .iter()
                .map(|path| path.to_string_lossy().into_owned()),
        );
        search_paths
    }

    /// Import the named modules into a fresh Lean environment and wrap
    /// it as a session.
    ///
    /// The Lean-side `lean_rs_host_session_import` receives the ordered
    /// project, dependency and shim roots and the module names. It adds
    /// the inherited and builtin roots and returns the resulting
    /// environment. Failures surface as
    /// [`lean_rs::LeanError::LeanException`] with the message Lean produced.
    pub(crate) fn import(
        capabilities: &'c LeanCapabilities<'lean, 'c>,
        scope: &TrustedHostImports<'lean, 'c>,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Self> {
        Self::import_with_profile(
            capabilities,
            scope,
            LeanSessionImportProfile::default(),
            cancellation,
            progress,
        )
    }

    pub(crate) fn import_with_profile(
        capabilities: &'c LeanCapabilities<'lean, 'c>,
        scope: &TrustedHostImports<'lean, 'c>,
        profile: LeanSessionImportProfile,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Self> {
        check_cancellation(cancellation)?;
        Self::import_scoped(
            capabilities,
            scope,
            LeanImportProfileMode::FullSession(profile),
            &LeanImportProfilerOptions::default(),
            progress,
        )
    }

    pub(crate) fn import_profiled(
        capabilities: &'c LeanCapabilities<'lean, 'c>,
        scope: &TrustedHostImports<'lean, 'c>,
        mode: LeanImportProfileMode,
        profiler_options: &LeanImportProfilerOptions,
    ) -> LeanResult<Self> {
        Self::import_scoped(capabilities, scope, mode, profiler_options, None)
    }

    fn import_scoped(
        capabilities: &'c LeanCapabilities<'lean, 'c>,
        scope: &TrustedHostImports<'lean, 'c>,
        mode: LeanImportProfileMode,
        profiler: &LeanImportProfilerOptions,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Self> {
        Self::import_with_roots(capabilities, scope, mode, profiler, SessionImport::Ordinary(progress))?
            .map(|(session, _)| session)
            .ok_or_else(|| conversion_error("fresh session import returned no environment"))
    }

    /// Import and return the effective roots observed under this admission. A
    /// known mismatch returns no session. Legacy shims can import without roots.
    ///
    /// # Errors
    /// Returns admission, native import and conversion errors.
    #[doc(hidden)]
    pub fn open_with_retained_roots(
        capabilities: &'c LeanCapabilities<'lean, 'c>,
        scope: &TrustedHostImports<'lean, 'c>,
        profile: LeanSessionImportProfile,
        expected: Option<&[String]>,
    ) -> LeanResult<Option<(Self, Option<Vec<String>>)>> {
        super::capabilities::reject_same_process_cargo_test_session_import()?;
        Self::import_with_roots(
            capabilities,
            scope,
            LeanImportProfileMode::FullSession(profile),
            &LeanImportProfilerOptions::default(),
            SessionImport::ObserveRoots(expected),
        )
    }

    /// Compare the prospective order for this profile with the order retained
    /// by the worker at import. Returns `None` when an order is unavailable.
    ///
    /// # Errors
    /// Returns errors for a different import profile, capability or native failure.
    #[doc(hidden)]
    pub fn compare_import_roots(
        &self,
        scope: &TrustedHostImports<'lean, 'c>,
        expected: &[String],
    ) -> LeanResult<Option<bool>> {
        if scope.plugin() != self.imports.plugin() {
            return Ok(Some(false));
        }
        if scope.imports() != self.imports.imports() || scope.options() != self.imports.options() {
            return Err(conversion_error(
                "root comparison requires the imported names and options",
            ));
        }
        with_host_import_transaction(|transaction| {
            match self
                .shims
                .session_import_paths_in(transaction, scope, Some(expected), true)?
            {
                Some((None, roots)) => Ok(comparable_roots(&roots).then(|| roots == expected)),
                Some((Some(_), _)) => Err(conversion_error("root comparison imported an environment")),
                None => Ok(None),
            }
        })
    }

    fn import_with_roots(
        capabilities: &'c LeanCapabilities<'lean, 'c>,
        scope: &TrustedHostImports<'lean, 'c>,
        mode: LeanImportProfileMode,
        profiler: &LeanImportProfilerOptions,
        request: SessionImport<'_>,
    ) -> LeanResult<Option<(Self, Option<Vec<String>>)>> {
        let _span = tracing::info_span!(target: "lean_rs", "lean_rs.host.session.import",
            profile = mode.label(), imports_len = scope.imports().len())
        .entered();
        scope.check_capability(capabilities.shim_capability())?;
        if scope.options() != &mode.native_options(profiler) {
            return Err(lean_rs::__host_internals::host_module_init(
                "import options differ from attested scope",
            ));
        }
        let shims = HostShimBindings::resolve(capabilities.shim_capability())
            .map_err(|err| lean_rs::__host_internals::host_module_init(err.to_string()))?;
        with_host_import_transaction(|transaction| {
            let (environment, roots) = match request {
                SessionImport::Ordinary(Some(sink)) => {
                    let total = Some(u64::try_from(scope.imports().len()).unwrap_or(u64::MAX));
                    let bridge = ProgressBridge::new(sink, "import", total)?;
                    (
                        shims.session_import_progress_in(transaction, scope, bridge.callback())?,
                        None,
                    )
                }
                SessionImport::ObserveRoots(expected) => {
                    match shims.session_import_paths_in(transaction, scope, expected, false)? {
                        Some((Some(env), roots)) => (env, comparable_roots(&roots).then_some(roots)),
                        Some((None, _)) => return Ok(None),
                        None => (shims.session_import_in(transaction, scope)?, None),
                    }
                }
                SessionImport::Ordinary(None) => (shims.session_import_in(transaction, scope)?, None),
            };
            let import_stats = LeanImportStats::try_from_lean(shims.import_stats(
                &environment,
                mode.import_level().as_str().to_owned(),
                mode.load_exts(),
            )?)?;
            let extension_registry_epoch = shims.registry_epoch_in(transaction)?;
            Ok(Some((
                Self {
                    capabilities,
                    shims,
                    imports: scope.clone(),
                    environment,
                    import_stats,
                    extension_registry_epoch,
                    stats: Cell::new(SessionStats::default()),
                },
                roots,
            )))
        })
    }

    /// Snapshot of this session's accumulated dispatch metrics.
    ///
    /// Returns a copy; the counters keep accumulating after the call.
    /// Use [`SessionStats::default`] to compute a delta across two
    /// snapshots.
    #[must_use]
    pub fn stats(&self) -> SessionStats {
        self.stats.get()
    }

    /// Lean-native attribution for this session's imported environment.
    #[must_use]
    pub fn import_stats(&self) -> &LeanImportStats {
        &self.import_stats
    }

    /// The process-global extension-registration stamp as it stood when this
    /// session's environment was imported.
    ///
    /// Opaque and monotone; only equality against
    /// [`Self::live_extension_registry_epoch`] carries meaning — never compare
    /// two stamps with `<`. When the two differ, a later import registered a
    /// Lean environment extension and this environment's `extensions` array is
    /// permanently shorter than the registry: Lean sizes it once, at
    /// `finalizeImport`, and keeps both the growth helper and the field
    /// `private`. Elaborating any `namespace`, `section`, or `open … in`
    /// against it makes `ScopedEnvExtension.pushScope` index past the end and
    /// `panic!`. There is no repair — the only sound response is to drop the
    /// session and re-import.
    ///
    /// A bare non-persistent `registerEnvExtension` moves none of the three
    /// registries the stamp sums, so in principle it can grow the array
    /// requirement without moving the stamp. The resulting mismatch is not
    /// reachable: extension indices are shared across kinds, so if such an
    /// extension lands at index *n* then `scopedEnvExtensionsRef` did not grow
    /// and every scoped extension the blind iteration touches still has an
    /// index inside the short array. The only other blind walks are over
    /// `persistentEnvExtensionsRef` — import finalization, and olean writing,
    /// which this host never performs. Beyond those, such an extension is
    /// reached only through an explicit `ext.getState env` from the module that
    /// registered it, and that module is not in the stale environment's import
    /// closure. Re-check this argument at every toolchain bump.
    #[must_use]
    pub fn extension_registry_epoch(&self) -> u64 {
        self.extension_registry_epoch
    }

    /// The current process-global extension-registration stamp.
    ///
    /// Compare against [`Self::extension_registry_epoch`] to decide whether
    /// this session's environment is still safe to elaborate against. The
    /// stamp is process-global, so one read answers for every live session,
    /// whichever one it is read through.
    ///
    /// # Errors
    ///
    /// Returns an error if the shim raises through `IO` or the result cannot
    /// be decoded.
    pub fn live_extension_registry_epoch(&self) -> LeanResult<u64> {
        let started = Instant::now();
        let epoch = with_host_import_transaction(|transaction| self.shims.registry_epoch_in(transaction));
        self.record_call(0, started.elapsed());
        epoch
    }

    /// Internal helper: record one FFI call and add `batch` per-item
    /// entries plus `elapsed` nanoseconds. Singular methods pass
    /// `batch = 0`; bulk methods pass the input length.
    fn record_call(&self, batch: u64, elapsed: std::time::Duration) {
        let mut s = self.stats.get();
        s.ffi_calls = s.ffi_calls.saturating_add(1);
        s.batch_items = s.batch_items.saturating_add(batch);
        s.elapsed_ns = s
            .elapsed_ns
            .saturating_add(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
        self.stats.set(s);
    }

    fn all_equal_name<'a>(names: &'a [&str]) -> Option<&'a str> {
        let first = *names.first()?;
        names.iter().all(|name| *name == first).then_some(first)
    }

    /// Look up a declaration by full Lean name (e.g. `"Nat.zero"`).
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage [`HostStage::Conversion`]
    /// if the name is not present in the imported environment. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side query raises.
    pub fn query_declaration(
        &mut self,
        name: &str,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<LeanDeclaration<'lean>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.query_declaration",
            name = name,
        )
        .entered();
        check_cancellation(cancellation)?;
        let name_handle = self.make_name(name, cancellation)?;
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.query_declaration(&self.environment, name_handle);
        self.record_call(0, t.elapsed());
        match result? {
            Some(decl) => Ok(decl),
            None => Err(lean_rs::abi::traits::conversion_error(format!(
                "declaration '{name}' not found in imported environment"
            ))),
        }
    }

    /// All declaration names in the imported environment.
    ///
    /// Returns a Vec; the environment's `constants` map contains many
    /// thousands of entries even for a small project (Lean prelude is
    /// always imported), so prefer [`LeanSession::query_declaration`]
    /// when you already know the name.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn list_declarations(
        &mut self,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<Vec<LeanName<'lean>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.list_declarations",
        )
        .entered();
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let raw = self.shims.list_declarations(&self.environment);
        self.record_call(0, t.elapsed());
        raw
    }

    /// Declaration names matching `filter`.
    ///
    /// Filtering runs inside Lean while traversing the environment
    /// constants table, so Rust only allocates handles for names the
    /// caller asked to keep.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn list_declarations_filtered(
        &mut self,
        filter: &LeanDeclarationFilter,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<LeanName<'lean>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.list_declarations_filtered",
            include_private = filter.include_private,
            include_generated = filter.include_generated,
            include_internal = filter.include_internal,
        )
        .entered();
        check_cancellation(cancellation)?;
        if let Some(sink) = progress {
            let bridge = ProgressBridge::new(sink, "list_declarations_filtered", None)?;
            let t = Instant::now();
            let result = self
                .shims
                .list_declarations_filtered_progress(&self.environment, *filter, bridge.callback());
            self.record_call(0, t.elapsed());
            result
        } else {
            let t = Instant::now();
            let result = self.shims.list_declarations_filtered(&self.environment, *filter);
            self.record_call(0, t.elapsed());
            result
        }
    }

    /// Source range Lean recorded for `name`.
    ///
    /// Returns `Ok(None)` when the name is absent or Lean has no
    /// declaration range for it. That is normal for synthetic,
    /// runtime-created, and some compiler-generated declarations.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn declaration_source_range(
        &mut self,
        name: &str,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<Option<LeanSourceRange>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_source_range",
            name = name,
        )
        .entered();
        check_cancellation(cancellation)?;
        let name_handle = self.make_name(name, cancellation)?;
        check_cancellation(cancellation)?;
        let source_roots = self
            .capabilities
            .host()
            .project()
            .source_roots()?
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self
            .shims
            .declaration_source_range(&self.environment, name_handle, source_roots);
        self.record_call(0, t.elapsed());
        result?.map(LeanSourceRange::try_from_lean).transpose()
    }

    /// The declared type of `name`, as an opaque [`LeanExpr`] handle.
    ///
    /// Returns `Ok(None)` if the name is not present in the environment.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn declaration_type(
        &mut self,
        name: &str,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<Option<LeanExpr<'lean>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_type",
            name = name,
        )
        .entered();
        check_cancellation(cancellation)?;
        let name_handle = self.make_name(name, cancellation)?;
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.declaration_type(&self.environment, name_handle);
        self.record_call(0, t.elapsed());
        result
    }

    /// The declared types of `names`, preserving input order.
    ///
    /// Returns `None` in each slot whose name is not present in the
    /// environment. With `cancellation = None`, the whole batch crosses
    /// the FFI boundary once and Lean converts the input strings to
    /// names internally. With `Some(token)`, this loops through
    /// [`Self::declaration_type`] so cancellation can be observed
    /// between items; partial results are discarded when cancellation
    /// fires.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side
    /// bulk shim raises through `IO`.
    pub fn declaration_type_bulk(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<Option<LeanExpr<'lean>>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_type_bulk",
            batch_size = names.len(),
        )
        .entered();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        check_cancellation(cancellation)?;
        if cancellation.is_some() {
            let started = Instant::now();
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(names.len());
            for (idx, name) in names.iter().enumerate() {
                check_cancellation(cancellation)?;
                out.push(self.declaration_type(name, cancellation)?);
                report_progress(
                    progress,
                    "declaration_type_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                    started,
                )?;
            }
            return Ok(out);
        }
        if progress.is_none()
            && let Some(name) = Self::all_equal_name(names)
        {
            let names_owned = vec![name.to_owned()];
            let t = Instant::now();
            let mut result = self.shims.declaration_type_bulk(&self.environment, names_owned)?;
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let value = result.pop().unwrap_or(None);
            return Ok(vec![value; names.len()]);
        }
        let names_owned: Vec<String> = names.iter().map(|&name| name.to_owned()).collect();
        if let Some(sink) = progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let bridge = ProgressBridge::new(sink, "declaration_type_bulk", total)?;

            let t = Instant::now();
            let result = self
                .shims
                .declaration_type_bulk_progress(&self.environment, names_owned, bridge.callback());
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result
        } else {
            let t = Instant::now();
            let result = self.shims.declaration_type_bulk(&self.environment, names_owned);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result
        }
    }

    /// The kind of `name` as a Lean-rendered string
    /// (`"axiom"`, `"definition"`, `"theorem"`, `"opaque"`, `"quot"`,
    /// `"inductive"`, `"constructor"`, `"recursor"`), or `"missing"`
    /// if `name` is not in the environment.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn declaration_kind(&mut self, name: &str, cancellation: Option<&LeanCancellationToken>) -> LeanResult<String> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_kind",
            name = name,
        )
        .entered();
        check_cancellation(cancellation)?;
        let name_handle = self.make_name(name, cancellation)?;
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.declaration_kind(&self.environment, name_handle);
        self.record_call(0, t.elapsed());
        result
    }

    /// The declaration kinds of `names`, preserving input order.
    ///
    /// Each output slot is the same string that [`Self::declaration_kind`]
    /// would return for the corresponding input, including `"missing"`
    /// for absent declarations. With `cancellation = None`, this is one
    /// Lean-side bulk dispatch over an `Array String`; with
    /// `Some(token)`, this loops through the singular path so the token
    /// can be checked between items.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side
    /// bulk shim raises through `IO`.
    pub fn declaration_kind_bulk(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<String>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_kind_bulk",
            batch_size = names.len(),
        )
        .entered();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        check_cancellation(cancellation)?;
        if cancellation.is_some() {
            let started = Instant::now();
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(names.len());
            for (idx, name) in names.iter().enumerate() {
                check_cancellation(cancellation)?;
                out.push(self.declaration_kind(name, cancellation)?);
                report_progress(
                    progress,
                    "declaration_kind_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                    started,
                )?;
            }
            return Ok(out);
        }
        if progress.is_none()
            && let Some(name) = Self::all_equal_name(names)
        {
            let names_owned = vec![name.to_owned()];
            let t = Instant::now();
            let mut result = self.shims.declaration_kind_bulk(&self.environment, names_owned)?;
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let value = result.pop().unwrap_or_default();
            return Ok(vec![value; names.len()]);
        }
        let names_owned: Vec<String> = names.iter().map(|&name| name.to_owned()).collect();
        if let Some(sink) = progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let bridge = ProgressBridge::new(sink, "declaration_kind_bulk", total)?;

            let t = Instant::now();
            let result = self
                .shims
                .declaration_kind_bulk_progress(&self.environment, names_owned, bridge.callback());
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result
        } else {
            let t = Instant::now();
            let result = self.shims.declaration_kind_bulk(&self.environment, names_owned);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result
        }
    }

    /// The Lean-rendered display string of `name`. Round-trips a name
    /// through the capability's `Name.toString` shim so callers see the
    /// same canonical form Lean would log.
    ///
    /// Diagnostic only—not a semantic key. Use
    /// [`LeanSession::query_declaration`] + a typed handle when
    /// equality matters.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn declaration_name(&mut self, name: &str, cancellation: Option<&LeanCancellationToken>) -> LeanResult<String> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_name",
            name = name,
        )
        .entered();
        check_cancellation(cancellation)?;
        let name_handle = self.make_name(name, cancellation)?;
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.declaration_name(&self.environment, name_handle);
        self.record_call(0, t.elapsed());
        result
    }

    /// Lean-rendered display strings for `names`, preserving input
    /// order.
    ///
    /// This is diagnostic text, not a semantic key. Missing
    /// declarations are not an error because the singular
    /// [`Self::declaration_name`] path also only round-trips the input
    /// name through Lean's `Name.toString` renderer.
    ///
    /// With `cancellation = None`, this is one Lean-side bulk dispatch
    /// over an `Array String`; with `Some(token)`, this loops through
    /// the singular path so the token can be checked between items.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side
    /// bulk shim raises through `IO`.
    pub fn declaration_name_bulk(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<String>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_name_bulk",
            batch_size = names.len(),
        )
        .entered();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        check_cancellation(cancellation)?;
        if cancellation.is_some() {
            let started = Instant::now();
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(names.len());
            for (idx, name) in names.iter().enumerate() {
                check_cancellation(cancellation)?;
                out.push(self.declaration_name(name, cancellation)?);
                report_progress(
                    progress,
                    "declaration_name_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                    started,
                )?;
            }
            return Ok(out);
        }
        if progress.is_none()
            && let Some(name) = Self::all_equal_name(names)
        {
            let names_owned = vec![name.to_owned()];
            let t = Instant::now();
            let mut result = self.shims.declaration_name_bulk(&self.environment, names_owned)?;
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let value = result.pop().unwrap_or_default();
            return Ok(vec![value; names.len()]);
        }
        let names_owned: Vec<String> = names.iter().map(|&name| name.to_owned()).collect();
        if let Some(sink) = progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let bridge = ProgressBridge::new(sink, "declaration_name_bulk", total)?;

            let t = Instant::now();
            let result = self
                .shims
                .declaration_name_bulk_progress(&self.environment, names_owned, bridge.callback());
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result
        } else {
            let t = Instant::now();
            let result = self.shims.declaration_name_bulk(&self.environment, names_owned);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result
        }
    }

    /// Render an opaque [`LeanName`] handle as its dotted-string form,
    /// routed through the capability's `Name.toString` shim.
    ///
    /// This is the supported way to turn a `LeanName` (e.g. an element
    /// of [`Self::list_declarations_filtered`]'s result) into Rust text.
    /// The output is diagnostic—not a semantic key—and equality on
    /// the underlying `Lean.Name` still lives in Lean.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch.
    pub fn name_to_string(
        &mut self,
        name: &LeanName<'lean>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<String> {
        let _span = tracing::debug_span!(target: "lean_rs", "lean_rs.host.session.name_to_string").entered();
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.name_to_string(name);
        self.record_call(0, t.elapsed());
        result
    }

    /// Render `names` as dotted-string forms, preserving input order.
    ///
    /// Implemented as a per-item loop over [`Self::name_to_string`] in
    /// v1: cancellation is checked between items, progress is reported
    /// after each. The Lean shim is pure and short, so the per-item FFI
    /// overhead is acceptable; a bulk shim is a future optimisation if
    /// profiling shows it matters.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] between items if the
    /// token is tripped during the walk.
    pub fn name_to_string_bulk(
        &mut self,
        names: &[LeanName<'lean>],
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<String>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.name_to_string_bulk",
            batch_size = names.len(),
        )
        .entered();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        check_cancellation(cancellation)?;
        let started = Instant::now();
        let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
        let mut out = Vec::with_capacity(names.len());
        for (idx, name) in names.iter().enumerate() {
            check_cancellation(cancellation)?;
            out.push(self.name_to_string(name, cancellation)?);
            report_progress(
                progress,
                "name_to_string_bulk",
                u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                total,
                started,
            )?;
        }
        Ok(out)
    }

    /// Enumerate the imported environment's declaration names and render
    /// each as a dotted string. Convenience over
    /// [`Self::list_declarations_filtered`] + [`Self::name_to_string_bulk`]
    /// for the common case where the consumer only needs strings.
    ///
    /// Two FFI hops (list + per-name render) and one heap allocation
    /// per name. For batches under a few thousand this is fine; for
    /// six-figure walks consider the lower-level pair so the listing
    /// pass and the rendering pass can be cancelled or chunked
    /// independently.
    ///
    /// # Errors
    ///
    /// Forwards errors from [`Self::list_declarations_filtered`] and
    /// [`Self::name_to_string_bulk`].
    pub fn list_declarations_strings(
        &mut self,
        filter: &LeanDeclarationFilter,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<String>> {
        let _span = tracing::debug_span!(target: "lean_rs", "lean_rs.host.session.list_declarations_strings").entered();
        let names = self.list_declarations_filtered(filter, cancellation, None)?;
        self.name_to_string_bulk(&names, cancellation, progress)
    }

    /// Search declarations with structural filters and bounded metadata rows.
    ///
    /// The search runs inside Lean while traversing the imported environment.
    /// It may inspect declaration types structurally for required constants and
    /// conclusion heads, but it never renders type text. Use
    /// [`Self::declaration_type`] for explicit one-name type rendering.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is already
    /// cancelled before dispatch. Returns [`lean_rs::LeanError::LeanException`]
    /// if the Lean-side search raises.
    pub fn search_declarations(
        &mut self,
        search: &DeclarationSearchRequest,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<DeclarationSearchResult> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.search_declarations",
            limit = search.limit,
            include_source = search.include_source,
        )
        .entered();
        check_cancellation(cancellation)?;
        let source_roots = if search.include_source {
            self.capabilities
                .host()
                .project()
                .source_roots()?
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self
            .shims
            .search_declarations(&self.environment, search.clone(), source_roots);
        self.record_call(0, t.elapsed());
        DeclarationSearchResult::try_from_lean(result?)
    }

    /// Inspect one selected declaration under explicit output budgets.
    ///
    /// Search remains metadata-only; callers use this method after selecting
    /// one declaration name whose rendered statement/docstring are worth
    /// paying for. Missing names and missing optional shim support are normal
    /// result statuses.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is already
    /// cancelled before dispatch. Returns [`lean_rs::LeanError::LeanException`]
    /// if the Lean-side inspection raises.
    pub fn inspect_declaration(
        &mut self,
        request: &DeclarationInspectionRequest,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<DeclarationInspectionResult> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.inspect_declaration",
            source = request.fields.source,
            statement = request.fields.statement,
            docstring = request.fields.docstring,
            attributes = request.fields.attributes,
            flags = request.fields.flags,
        )
        .entered();
        check_cancellation(cancellation)?;
        if !self.shims.supports_declaration_inspection() {
            return Ok(DeclarationInspectionResult::Unsupported);
        }
        let source_roots = if request.fields.source {
            self.capabilities
                .host()
                .project()
                .source_roots()?
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        check_cancellation(cancellation)?;
        let t = Instant::now();
        // Bound the optional notation-aware statement rendering by the default
        // heartbeat budget; a deeply nested term that exceeds it falls back to
        // the raw `Expr.toString` form inside the shim.
        let result = self.shims.inspect_declaration(
            &self.environment,
            request.clone(),
            source_roots,
            lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT,
        );
        self.record_call(0, t.elapsed());
        match result? {
            Some(record) => DeclarationInspectionResult::try_from_lean(record),
            None => Ok(DeclarationInspectionResult::Unsupported),
        }
    }

    /// Render `expr` via `Expr.toString`—the cheap, deterministic
    /// projection.
    ///
    /// Walks the syntax tree directly: no `MetaM`, no notation lookup,
    /// no binder pretty-printing. The result is a legible-but-ugly
    /// dump suitable for indexing, logging, and search keys. For the
    /// form a Lean user reads, use the optional
    /// [`crate::host::meta::pp_expr`] service through
    /// [`Self::run_meta`] instead—it pays for elaboration context to
    /// get notation and unfolding right but can time out under a tight
    /// heartbeat budget.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch.
    pub fn expr_to_string_raw(
        &mut self,
        expr: &LeanExpr<'lean>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<String> {
        let _span = tracing::debug_span!(target: "lean_rs", "lean_rs.host.session.expr_to_string_raw").entered();
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.expr_to_string(expr);
        self.record_call(0, t.elapsed());
        result
    }

    /// Parse and elaborate a Lean module, returning only the requested
    /// bounded projection.
    ///
    /// The Lean shim owns header parsing, module-system header handling,
    /// info-tree traversal, cursor selection, reference collection, and
    /// bounded expression/goal rendering. The Rust side chooses a
    /// [`ModuleQuery`](crate::host::process::ModuleQuery) and receives the matching
    /// [`ModuleQueryOutcome`]; whole-file raw expression/type dumps never
    /// cross this boundary.
    ///
    /// The shim is optional. When the loaded capability dylib does not
    /// export `lean_rs_host_process_module_query`, the method returns
    /// [`ModuleQueryOutcome::Unsupported`] without an FFI call.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side shim
    /// raises through `IO`. Returns [`lean_rs::LeanError::Host`] with
    /// stage [`HostStage::Conversion`] if the Lean return value does
    /// not decode into [`ModuleQueryOutcome`].
    pub fn process_module_query(
        &mut self,
        request: &lean_rs::host_bridge::TrustedModuleSource<'lean, '_, lean_rs::host_bridge::HostModuleQuery>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<ModuleQueryOutcome> {
        check_cancellation(cancellation)?;
        let started = Instant::now();
        let result = self
            .shims
            .process_module_query(&self.environment, &self.imports, request);
        self.record_call(
            u64::try_from(request.item_count()).unwrap_or(u64::MAX),
            started.elapsed(),
        );
        match result? {
            Some(record) => ModuleQueryOutcome::try_from_lean(record),
            None => Ok(ModuleQueryOutcome::Unsupported),
        }
    }

    /// Parse and elaborate a Lean module once, returning several bounded
    /// projections keyed by selector id.
    ///
    /// This is the proof-agent path: Lean owns header handling, one body
    /// elaboration, info-tree traversal, and selector projection. Rust sends
    /// a small selector array and receives per-selector outcomes; whole-file
    /// info-tree arrays never cross the boundary.
    ///
    /// The shim is optional. When the loaded capability dylib does not
    /// export `lean_rs_host_process_module_query_batch`, the method returns
    /// [`ModuleQueryBatchOutcome::Unsupported`] without an FFI call.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side shim raises
    /// through `IO`. Returns [`lean_rs::LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the Lean return value does not decode
    /// into [`ModuleQueryBatchOutcome`].
    pub fn process_module_query_batch(
        &mut self,
        request: &lean_rs::host_bridge::TrustedModuleSource<'lean, '_, lean_rs::host_bridge::HostModuleBatch>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<ModuleQueryBatchOutcome> {
        check_cancellation(cancellation)?;
        let started = Instant::now();
        let result = self
            .shims
            .process_module_query_batch(&self.environment, &self.imports, request);
        self.record_call(
            u64::try_from(request.item_count()).unwrap_or(u64::MAX),
            started.elapsed(),
        );
        match result? {
            Some(record) => ModuleQueryBatchOutcome::try_from_lean(record),
            None => Ok(ModuleQueryBatchOutcome::Unsupported),
        }
    }

    /// Parse/elaborate a Lean module through the shim-owned module snapshot
    /// cache, then return bounded selector projections plus cache facts.
    ///
    /// The snapshot cache remains private to the loaded shim. Rust provides
    /// the stable cache key and conservative policy, but never receives raw
    /// info trees.
    ///
    /// # Errors
    ///
    /// Returns an error if cancellation is already requested, if the shim
    /// raises an `IO` exception, or if the Lean result cannot be decoded into
    /// the expected cached batch outcome.
    pub fn process_module_query_batch_cached(
        &mut self,
        request: &lean_rs::host_bridge::TrustedModuleSource<'lean, '_, lean_rs::host_bridge::HostCachedModuleBatch>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<ModuleQueryBatchCachedOutcome> {
        check_cancellation(cancellation)?;
        let started = Instant::now();
        let result = self
            .shims
            .process_module_query_batch_cached(&self.environment, &self.imports, request);
        self.record_call(
            u64::try_from(request.item_count()).unwrap_or(u64::MAX),
            started.elapsed(),
        );
        match result? {
            Some(record) => ModuleQueryBatchCachedOutcome::try_from_lean(record),
            None => Ok(ModuleQueryBatchCachedOutcome::Unsupported),
        }
    }

    /// Try proof snippets against an in-memory source overlay.
    ///
    /// The shim is optional. When the loaded capability dylib does not export
    /// `lean_rs_host_attempt_proof`, the method returns
    /// [`ProofAttemptOutcome::Unsupported`] without an FFI call.
    ///
    /// # Errors
    ///
    /// Returns an error if cancellation is already requested, if the shim
    /// raises an `IO` exception, or if the Lean result cannot be decoded.
    pub fn attempt_proof(
        &mut self,
        request: &lean_rs::host_bridge::TrustedModuleSource<
            'lean,
            '_,
            lean_rs::host_bridge::module_requests::ProofAttemptRequest,
        >,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<ProofAttemptOutcome> {
        check_cancellation(cancellation)?;
        let started = Instant::now();
        let result = self.shims.attempt_proof(&self.environment, &self.imports, request);
        self.record_call(
            u64::try_from(request.item_count()).unwrap_or(u64::MAX),
            started.elapsed(),
        );
        match result? {
            Some(record) => ProofAttemptOutcome::try_from_lean(record),
            None => Ok(ProofAttemptOutcome::Unsupported),
        }
    }

    /// Verify one declaration in an in-memory source snapshot.
    ///
    /// The shim is optional. When the loaded capability dylib does not export
    /// `lean_rs_host_verify_declaration`, the method returns
    /// [`DeclarationVerificationOutcome::Unsupported`] without an FFI call.
    ///
    /// # Errors
    ///
    /// Returns an error if cancellation is already requested, if the shim
    /// raises an `IO` exception, or if the Lean result cannot be decoded.
    pub fn verify_declaration(
        &mut self,
        request: &lean_rs::host_bridge::TrustedModuleSource<
            'lean,
            '_,
            lean_rs::host_bridge::module_requests::DeclarationVerificationRequest,
        >,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<DeclarationVerificationOutcome> {
        check_cancellation(cancellation)?;
        let started = Instant::now();
        let result = self.shims.verify_declaration(&self.environment, &self.imports, request);
        self.record_call(
            u64::try_from(request.item_count()).unwrap_or(u64::MAX),
            started.elapsed(),
        );
        match result? {
            Some(record) => DeclarationVerificationOutcome::try_from_lean(record),
            None => Ok(DeclarationVerificationOutcome::Unsupported),
        }
    }

    /// Verify several declarations in one in-memory source snapshot.
    ///
    /// The shim is optional. When the loaded capability dylib does not export
    /// `lean_rs_host_verify_declaration_batch`, the method returns
    /// [`DeclarationVerificationBatchOutcome::Unsupported`] without an FFI
    /// call. The returned rows preserve request order.
    ///
    /// # Errors
    ///
    /// Returns an error if cancellation is already requested, if the shim
    /// raises an `IO` exception, or if the Lean result cannot be decoded.
    pub fn verify_declaration_batch(
        &mut self,
        request: &lean_rs::host_bridge::TrustedModuleSource<
            'lean,
            '_,
            lean_rs::host_bridge::module_requests::DeclarationVerificationBatchRequest,
        >,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<DeclarationVerificationBatchOutcome> {
        check_cancellation(cancellation)?;
        let started = Instant::now();
        let result = self
            .shims
            .verify_declaration_batch(&self.environment, &self.imports, request);
        self.record_call(
            u64::try_from(request.item_count()).unwrap_or(u64::MAX),
            started.elapsed(),
        );
        match result? {
            Some(record) => DeclarationVerificationBatchOutcome::try_from_lean(record),
            None => Ok(DeclarationVerificationBatchOutcome::Unsupported),
        }
    }

    /// Clear the shim-owned module snapshot cache when the loaded capability
    /// supports it.
    ///
    /// This releases only cached module snapshots built by the bundled
    /// info-tree shim. It does not reset Lean's runtime, unload imported
    /// modules, or make full-session compacted regions safe to free.
    ///
    /// # Errors
    ///
    /// Returns an error if the shim raises an `IO` exception or if the Lean
    /// clear result cannot be decoded.
    pub fn clear_module_snapshot_cache(&mut self) -> LeanResult<ModuleSnapshotCacheClearResult> {
        let t = Instant::now();
        let result = self.shims.clear_module_snapshot_cache(());
        self.record_call(0, t.elapsed());
        match result? {
            Some(obj) => ModuleSnapshotCacheClearResult::try_from_lean(obj),
            None => Ok(ModuleSnapshotCacheClearResult {
                entries_cleared: 0,
                approx_bytes_cleared: 0,
            }),
        }
    }

    /// Inspect checked bindings retained by this live session. No source is
    /// elaborated and no user capability export is called.
    ///
    /// # Errors
    /// Returns the loaded runtime-version shim's IO or ABI conversion failure.
    pub fn checked_host_runtime_metadata(&self) -> LeanResult<LeanHostRuntimeMetadata> {
        let version = self.shims.runtime_version()?;
        Ok(LeanHostRuntimeMetadata {
            runtime_lean_version: version,
            checked_binding_signatures: self.shims.checked_signatures(),
        })
    }

    /// Set the cumulative session-import charge and source-header budget.
    /// A zero limit disables the byte limit, not failed-import retirement.
    /// This setting is process-wide. Callers using multiple host threads must
    /// coordinate budget configuration; worker children use one serial thread.
    /// # Errors
    /// Returns a shim call error.
    pub fn configure_header_import_budget(&mut self, session_bytes: u64, limit_bytes: u64) -> LeanResult<()> {
        self.shims
            .configure_header_import_budget(session_bytes, limit_bytes)
            .map(|_| ())
    }

    /// Read process-wide cumulative source-header accounting.
    /// # Errors
    /// Returns a shim call or malformed JSON error.
    pub fn header_import_state(&self) -> LeanResult<HeaderImportState> {
        let Some(json) = self.shims.header_import_state(())? else {
            return Ok(HeaderImportState::default());
        };
        serde_json::from_str(&json).map_err(|err| conversion_error(format!("invalid header import state: {err}")))
    }

    /// Drop the reusable header for this session, without changing its charge.
    /// # Errors
    /// Returns a shim call error.
    pub fn clear_module_header_cache(&mut self) -> LeanResult<()> {
        self.shims
            .clear_module_header_cache(self.environment.clone())
            .map(|_| ())
    }

    /// Parse and elaborate a single Lean term against the imported
    /// environment, optionally against an expected type.
    ///
    /// The boundary is explicit: Rust supplies the source text, module
    /// context, and bounded options; Lean parses, elaborates, and
    /// returns either an opaque [`LeanExpr`] handle or a structured
    /// [`LeanElabFailure`] carrying typed diagnostics. Rust does not
    /// inspect elaborator internals or proof terms to decide
    /// correctness.
    ///
    /// The outer [`LeanResult`] surfaces host-stack failures (a Lean
    /// `IO`-level exception from the shim itself, a malformed Lean
    /// return value); the inner `Result` distinguishes successful
    /// elaboration from parse / type / kernel-stage failures the
    /// elaborator reports through its `MessageLog`. Both error paths
    /// propagate the [`LeanElabOptions::diagnostic_byte_limit`] bound
    /// structurally.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side shim raises
    /// through `IO`. Returns [`lean_rs::LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the Lean return value does not
    /// decode into [`LeanElabFailure`] / [`LeanExpr`].
    pub fn elaborate(
        &mut self,
        term: &lean_rs::host_bridge::TrustedHostTerm<'lean, '_>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<Result<LeanExpr<'lean>, LeanElabFailure>> {
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.elaborate(&self.environment, &self.imports, term);
        self.record_call(0, t.elapsed());
        match result? {
            Ok(expression) => Ok(Ok(expression)),
            Err(record) => LeanElabFailure::try_from_lean(record).map(Err),
        }
    }

    /// Borrow the environment and import authority an application needs to attest source for this session.
    #[must_use]
    pub fn source_context(&self) -> (&HostEnvironment<'lean>, &TrustedHostImports<'lean, 'c>) {
        (&self.environment, &self.imports)
    }

    /// Parse, elaborate, and kernel-check a Lean declaration source
    /// (typically a `theorem` or `def`), returning a typed outcome
    /// that classifies the result and carries either the produced
    /// [`crate::LeanEvidence`] handle or the diagnostics the elaborator and
    /// kernel emitted.
    ///
    /// The boundary is explicit (mirrors [`Self::elaborate`]): Rust
    /// supplies source + options; Lean parses, elaborates, runs
    /// `addDecl` (which kernel-checks), and classifies the outcome.
    /// Rust never inspects the produced proof term or declaration
    /// internals.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side shim
    /// raises through `IO` (an unexpected internal failure that is not
    /// itself a rejection / unavailable diagnostic). Returns
    /// [`lean_rs::LeanError::Host`] with stage [`HostStage::Conversion`] if the
    /// Lean return value does not decode into [`LeanKernelOutcome`].
    pub fn kernel_check(
        &mut self,
        request: &lean_rs::host_bridge::TrustedHostKernelRequest<'lean, '_>,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<LeanKernelOutcome<'lean>> {
        check_cancellation(cancellation)?;
        let bridge = progress
            .map(|sink| ProgressBridge::new(sink, "kernel_check", Some(1)))
            .transpose()?;
        let started = Instant::now();
        let result = self.shims.kernel_check(
            &self.environment,
            &self.imports,
            request,
            bridge.as_ref().map(ProgressBridge::callback),
        );
        self.record_call(0, started.elapsed());
        LeanKernelOutcome::from_native(result?)
    }

    /// Re-validate a previously captured [`LeanEvidence`] against the
    /// session's imported environment, returning the kernel's current
    /// verdict.
    ///
    /// The handle was produced by an earlier
    /// [`Self::kernel_check`] call against this same environment and
    /// carries the kernel-accepted `Lean.Declaration` opaquely. The
    /// session never installs that declaration into its stored
    /// environment, so re-checking against the unchanged environment
    /// is the supported way to ask "is this evidence still valid?"—
    /// the kernel runs fresh.
    ///
    /// The returned [`EvidenceStatus`] mirrors
    /// [`LeanKernelOutcome::status`]: `Checked` on success, `Rejected`
    /// if the kernel now refuses the declaration, `Unavailable` if
    /// the Lean shim caught an `IO` exception. The Lean fixture does
    /// not currently emit `Unsupported` from this path—`Unsupported`
    /// only fires during the initial classification in
    /// `kernel_check`.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean shim raises
    /// through `IO` outside of its own `try` (an unexpected internal
    /// failure that the shim did not classify). Returns
    /// [`lean_rs::LeanError::Host`] with stage [`HostStage::Conversion`] if the
    /// return value does not decode as a four-tag
    /// [`EvidenceStatus`] inductive.
    pub fn check_evidence(
        &mut self,
        handle: &LeanEvidence<'lean>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<EvidenceStatus> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.check_evidence",
        )
        .entered();
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.check_evidence(&self.environment, handle);
        self.record_call(0, t.elapsed());
        EvidenceStatus::try_from_lean(result?)
    }

    /// Project a previously captured [`LeanEvidence`] into a bounded
    /// [`ProofSummary`] for diagnostics or storage.
    ///
    /// The Lean shim renders the captured declaration's name, kind,
    /// and type expression as three byte-bounded `String`s—no
    /// `Lean.Expr` or proof term crosses the FFI boundary. The
    /// summary is computed on demand (not at
    /// [`Self::kernel_check`] time) because most callers only ever
    /// inspect the [`EvidenceStatus`] tag and would pay the
    /// pretty-print cost for nothing.
    ///
    /// Strings on the returned summary are display text. They are not
    /// semantic keys; route equality comparisons through a
    /// Lean-authored equality export.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean shim raises
    /// through `IO`. Returns [`lean_rs::LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the return value does not decode
    /// as a three-field [`ProofSummary`] structure.
    pub fn summarize_evidence(
        &mut self,
        handle: &LeanEvidence<'lean>,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<ProofSummary> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.summarize_evidence",
        )
        .entered();
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.evidence_summary(&self.environment, handle);
        self.record_call(0, t.elapsed());
        ProofSummary::try_from_lean(result?)
    }

    /// Invoke a registered bounded [`MetaM`](https://leanprover.github.io/theorem_proving_in_lean4/)
    /// service against the imported environment.
    ///
    /// The session dispatches through the checked binding for the closed
    /// service shape; if the loaded capability does not export the optional
    /// symbol, the call short-circuits to [`LeanMetaResponse::Unsupported`]
    /// with a synthetic host-side diagnostic naming the missing symbol.
    ///
    /// The outer [`LeanResult`] surfaces host-stack failures (a Lean
    /// `IO`-level exception from the shim itself, or an undecodable
    /// return value). The four-way classification—`Ok` / `Failed` /
    /// `TimeoutOrHeartbeat` / `Unsupported`—lives in the inner
    /// [`LeanMetaResponse`].
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean shim raises
    /// through `IO`. Returns [`lean_rs::LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the return value does not decode
    /// into [`LeanMetaResponse<Resp>`].
    pub fn run_meta<Req, Resp>(
        &mut self,
        service: &LeanMetaService<Req, Resp>,
        request: Req,
        options: &LeanMetaOptions,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<LeanMetaResponse<Resp>>
    where
        LeanMetaService<Req, Resp>: HostMetaDispatch<'lean, Req, Resp>,
    {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.run_meta",
            service = service.name(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
        check_cancellation(cancellation)?;
        service.dispatch(self, request, options)
    }

    /// Look up many declarations in one Lean traversal.
    ///
    /// Equivalent to calling [`Self::query_declaration`] in a loop over
    /// `names`, except that the entire batch crosses the FFI boundary
    /// exactly once: one `Array Name` allocation in, one
    /// `Array (Option Declaration)` allocation out. The Lean shim folds
    /// the singular `envQueryDeclaration` across the input array, so the
    /// iteration semantics are identical to a Rust-side fold over the
    /// singular path—a missing name still errors the batch.
    ///
    /// Names are still resolved through the capability's
    /// `name_from_string` shim, one [`lean_rs::LeanName`] handle per
    /// input. The metric impact is `names.len() + 1` recorded FFI calls
    /// for a batch of `names.len()` items, versus `2 * names.len()` for
    /// the same workload through [`Self::query_declaration`].
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage [`HostStage::Conversion`]
    /// on the first name that is not present in the imported
    /// environment, with the missing name in the diagnostic. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side bulk shim raises
    /// through `IO`.
    pub fn query_declarations_bulk(
        &mut self,
        names: &[&str],
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<LeanDeclaration<'lean>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.query_declarations_bulk",
            batch_size = names.len(),
        )
        .entered();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        check_cancellation(cancellation)?;
        if cancellation.is_some() {
            let started = Instant::now();
            let mut out = Vec::with_capacity(names.len());
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            for (idx, name) in names.iter().enumerate() {
                check_cancellation(cancellation)?;
                out.push(self.query_declaration(name, cancellation)?);
                report_progress(
                    progress,
                    "query_declarations_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                    started,
                )?;
            }
            return Ok(out);
        }
        let prepare_started = Instant::now();
        let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
        let mut name_handles: Vec<LeanName<'lean>> = Vec::with_capacity(names.len());
        for (idx, name) in names.iter().enumerate() {
            name_handles.push(self.make_name(name, cancellation)?);
            report_progress(
                progress,
                "prepare_names",
                u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                total,
                prepare_started,
            )?;
        }
        check_cancellation(cancellation)?;
        let raw = if let Some(sink) = progress {
            let bridge = ProgressBridge::new(sink, "query_declarations_bulk", total)?;

            let t = Instant::now();
            let result =
                self.shims
                    .query_declarations_bulk_progress(&self.environment, name_handles, bridge.callback());
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result?
        } else {
            let t = Instant::now();
            let result = self.shims.query_declarations_bulk(&self.environment, name_handles);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result?
        };
        let mut out: Vec<LeanDeclaration<'lean>> = Vec::with_capacity(raw.len());
        for (slot, name) in raw.into_iter().zip(names.iter()) {
            match slot {
                Some(decl) => out.push(decl),
                None => {
                    return Err(lean_rs::abi::traits::conversion_error(format!(
                        "declaration '{name}' not found in imported environment"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// Parse and elaborate many independent Lean terms in one Lean
    /// traversal.
    ///
    /// Per-source `Result<LeanExpr, LeanElabFailure>` shape matches
    /// [`Self::elaborate`] exactly: outer [`LeanResult`] surfaces
    /// host-stack failures, inner per-source `Result` distinguishes
    /// successful elaboration from elaborator-reported diagnostics. A
    /// caller treating the bulk path as a fold over the singular path
    /// sees no semantic surprise.
    ///
    /// The `expected_type` parameter is **not** carried by the bulk
    /// shape: per-source expectations would force a parallel
    /// `&[Option<&LeanExpr>]` array, and no in-tree caller has earned
    /// the surface. Use [`Self::elaborate`] for individual terms with
    /// expected types.
    ///
    /// The heartbeat and diagnostic-byte budgets in `options` apply
    /// once each per source (the Lean shim builds fresh
    /// [`Lean.Options`](https://leanprover.github.io/) per item via the
    /// same `hostElaborate` path), so the per-batch upper bound on
    /// elapsed CPU work is `sources.len() * options.heartbeats()`.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side bulk shim
    /// raises through `IO`. Returns [`lean_rs::LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the Lean return value does not
    /// decode into a `Vec<Result<LeanExpr, LeanElabFailure>>`.
    pub fn elaborate_bulk(
        &mut self,
        batch: &lean_rs::host_bridge::TrustedHostTermBatch<'lean, '_>,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        check_cancellation(cancellation)?;
        let total = u64::try_from(batch.len()).unwrap_or(u64::MAX);
        if cancellation.is_some() {
            let started = Instant::now();
            let mut out = Vec::with_capacity(batch.len());
            for (index, term) in batch.terms().enumerate() {
                out.push(self.elaborate(&term, cancellation)?);
                report_progress(
                    progress,
                    "elaborate_bulk",
                    u64::try_from(index.saturating_add(1)).unwrap_or(u64::MAX),
                    Some(total),
                    started,
                )?;
            }
            return Ok(out);
        }
        let progress = progress
            .map(|sink| ProgressBridge::new(sink, "elaborate_bulk", Some(total)))
            .transpose()?;
        let started = Instant::now();
        let result = self.shims.elaborate_bulk(
            &self.environment,
            &self.imports,
            batch,
            progress.as_ref().map(ProgressBridge::callback),
        );
        self.record_call(total, started.elapsed());
        result?
            .into_iter()
            .map(|item| match item {
                Ok(expression) => Ok(Ok(expression)),
                Err(record) => LeanElabFailure::try_from_lean(record).map(Err),
            })
            .collect()
    }

    /// Build a `LeanName` from a dotted Rust string via the capability's
    /// `Name.toName` shim.
    fn make_name(&self, name: &str, cancellation: Option<&LeanCancellationToken>) -> LeanResult<LeanName<'lean>> {
        check_cancellation(cancellation)?;
        let t = Instant::now();
        let result = self.shims.name_from_string(name.to_owned());
        self.record_call(0, t.elapsed());
        result
    }
}

trait HostMetaDispatch<'lean, Req, Resp> {
    fn dispatch(
        &self,
        session: &mut LeanSession<'lean, '_>,
        request: Req,
        options: &LeanMetaOptions,
    ) -> LeanResult<LeanMetaResponse<Resp>>;
}

impl<'lean> HostMetaDispatch<'lean, LeanExpr<'lean>, LeanExpr<'lean>>
    for LeanMetaService<LeanExpr<'lean>, LeanExpr<'lean>>
{
    fn dispatch(
        &self,
        session: &mut LeanSession<'lean, '_>,
        request: LeanExpr<'lean>,
        options: &LeanMetaOptions,
    ) -> LeanResult<LeanMetaResponse<LeanExpr<'lean>>> {
        let t = Instant::now();
        let shims = &session.shims;
        let env = &session.environment;
        let heartbeats = options.heartbeats();
        let bytes = options.diagnostic_byte_limit_usize();
        let mode = options.transparency_byte();
        let result = match self.name() {
            "lean_rs_host_meta_infer_type" => shims.meta_infer_type(env, request, heartbeats, bytes, mode),
            "lean_rs_host_meta_whnf" => shims.meta_whnf(env, request, heartbeats, bytes, mode),
            "lean_rs_host_meta_heartbeat_burn" => shims.meta_heartbeat_burn(env, request, heartbeats, bytes, mode),
            _ => return Ok(unsupported_meta_response(self.name())),
        };
        session.record_call(0, t.elapsed());
        decode_meta_result(result?, self.name())
    }
}

impl<'lean>
    HostMetaDispatch<
        'lean,
        (
            LeanExpr<'lean>,
            LeanExpr<'lean>,
            crate::host::meta::LeanMetaTransparency,
        ),
        bool,
    >
    for LeanMetaService<
        (
            LeanExpr<'lean>,
            LeanExpr<'lean>,
            crate::host::meta::LeanMetaTransparency,
        ),
        bool,
    >
{
    fn dispatch(
        &self,
        session: &mut LeanSession<'lean, '_>,
        request: (
            LeanExpr<'lean>,
            LeanExpr<'lean>,
            crate::host::meta::LeanMetaTransparency,
        ),
        options: &LeanMetaOptions,
    ) -> LeanResult<LeanMetaResponse<bool>> {
        let t = Instant::now();
        let result = session.shims.meta_is_def_eq(
            &session.environment,
            (request.0, request.1, request.2.as_byte()),
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
            options.transparency_byte(),
        );
        session.record_call(0, t.elapsed());
        decode_meta_result(result?, self.name())
    }
}

impl<'lean> HostMetaDispatch<'lean, LeanExpr<'lean>, String> for LeanMetaService<LeanExpr<'lean>, String> {
    fn dispatch(
        &self,
        session: &mut LeanSession<'lean, '_>,
        request: LeanExpr<'lean>,
        options: &LeanMetaOptions,
    ) -> LeanResult<LeanMetaResponse<String>> {
        let t = Instant::now();
        let result = session.shims.meta_pp_expr(
            &session.environment,
            request,
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
            options.transparency_byte(),
        );
        session.record_call(0, t.elapsed());
        decode_meta_result(result?, self.name())
    }
}

fn decode_meta_result<T>(
    result: Option<lean_rs::host_bridge::HostMetaOutcome<'_, T>>,
    symbol: &str,
) -> LeanResult<LeanMetaResponse<T>> {
    use lean_rs::host_bridge::HostMetaOutcome;
    match result {
        None => Ok(unsupported_meta_response(symbol)),
        Some(HostMetaOutcome::Ok(value)) => Ok(LeanMetaResponse::Ok(value)),
        Some(HostMetaOutcome::Failed(record)) => LeanElabFailure::try_from_lean(record).map(LeanMetaResponse::Failed),
        Some(HostMetaOutcome::Timeout(record)) => {
            LeanElabFailure::try_from_lean(record).map(LeanMetaResponse::TimeoutOrHeartbeat)
        }
        Some(HostMetaOutcome::Unsupported(record)) => {
            LeanElabFailure::try_from_lean(record).map(LeanMetaResponse::Unsupported)
        }
    }
}

fn unsupported_meta_response<Resp>(symbol: &str) -> LeanMetaResponse<Resp> {
    LeanMetaResponse::Unsupported(LeanElabFailure::synthetic(
        format!("bundled host shim does not export meta service '{symbol}'"),
        "<lean-rs-host meta>".to_owned(),
    ))
}
