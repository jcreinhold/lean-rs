//! `LeanSession` — a long-lived Lean session over an imported
//! environment.
//!
//! A [`LeanSession`] holds an imported `Lean.Environment` value (as an
//! opaque `Obj<'lean>`) plus a borrow of its parent
//! [`crate::host::LeanCapabilities`]. Each typed query method
//! ([`LeanSession::query_declaration`], …) dispatches through a
//! pre-resolved C-ABI function address cached on the capability — one
//! struct-field read, one FFI call, no per-query `dlsym`.
//!
//! ## Capability contract
//!
//! The bundled host shim dylib that [`crate::host::LeanCapabilities`] loads
//! exports twenty-eight **mandatory** `@[export]` symbols and may export seven
//! **optional** symbols (matched at `LeanCapabilities::load_capabilities` time) —
//! five bounded `MetaM` services plus two info-tree projection entry points
//! (one for body-only snippets, one for files that include an `import` header):
//!
//! | C symbol                                       | Mandatory? | Lean signature                                                                                                |
//! | ---------------------------------------------- | ---------- | ------------------------------------------------------------------------------------------------------------- |
//! | `lean_rs_host_session_import`                  | yes        | `String -> Array String -> IO Environment`                                                                    |
//! | `lean_rs_host_session_import_progress`         | yes        | `Array String -> Array String -> USize -> USize -> IO (Except UInt8 Environment)`                             |
//! | `lean_rs_host_name_from_string`                | yes        | `String -> Name`                                                                                              |
//! | `lean_rs_host_name_to_string`                  | yes        | `Name -> String`                                                                                              |
//! | `lean_rs_host_env_query_declaration`           | yes        | `Environment -> Name -> IO (Option Declaration)`                                                              |
//! | `lean_rs_host_env_query_declarations_bulk`     | yes        | `Environment -> Array Name -> IO (Array (Option Declaration))`                                                |
//! | `lean_rs_host_env_query_declarations_bulk_progress` | yes   | `Environment -> Array Name -> USize -> USize -> IO (Except UInt8 (Array (Option Declaration)))`               |
//! | `lean_rs_host_env_list_declarations`           | yes        | `Environment -> IO (Array Name)`                                                                              |
//! | `lean_rs_host_env_list_declarations_filtered`  | yes        | `Environment -> DeclarationFilter -> IO (Array Name)`                                                         |
//! | `lean_rs_host_env_list_declarations_filtered_progress` | yes | `Environment -> DeclarationFilter -> USize -> USize -> IO (Except UInt8 (Array Name))`                        |
//! | `lean_rs_host_env_declaration_source_range`    | yes        | `Environment -> Name -> Array String -> IO (Option SourceRange)`                                              |
//! | `lean_rs_host_env_declaration_type`            | yes        | `Environment -> Name -> IO (Option Expr)`                                                                     |
//! | `lean_rs_host_env_declaration_type_bulk`       | yes        | `Environment -> Array String -> IO (Array (Option Expr))`                                                     |
//! | `lean_rs_host_env_declaration_type_bulk_progress` | yes    | `Environment -> Array String -> USize -> USize -> IO (Except UInt8 (Array (Option Expr)))`                    |
//! | `lean_rs_host_env_declaration_kind`            | yes        | `Environment -> Name -> IO String`                                                                            |
//! | `lean_rs_host_env_declaration_kind_bulk`       | yes        | `Environment -> Array String -> IO (Array String)`                                                            |
//! | `lean_rs_host_env_declaration_kind_bulk_progress` | yes    | `Environment -> Array String -> USize -> USize -> IO (Except UInt8 (Array String))`                           |
//! | `lean_rs_host_env_declaration_name`            | yes        | `Environment -> Name -> IO String`                                                                            |
//! | `lean_rs_host_env_declaration_name_bulk`       | yes        | `Environment -> Array String -> IO (Array String)`                                                            |
//! | `lean_rs_host_env_declaration_name_bulk_progress` | yes    | `Environment -> Array String -> USize -> USize -> IO (Except UInt8 (Array String))`                           |
//! | `lean_rs_host_env_expr_to_string_raw`          | yes        | `Expr -> String`                                                                                              |
//! | `lean_rs_host_elaborate`                       | yes        | `Environment -> String -> Option Expr -> String -> String -> UInt64 -> USize -> IO (Except ElabFailure Expr)` |
//! | `lean_rs_host_elaborate_bulk`                  | yes        | `Environment -> Array String -> String -> String -> UInt64 -> USize -> IO (Array (Except ElabFailure Expr))`  |
//! | `lean_rs_host_elaborate_bulk_progress`         | yes        | `Environment -> Array String -> String -> String -> UInt64 -> USize -> USize -> USize -> IO (Except UInt8 (Array (Except ElabFailure Expr)))` |
//! | `lean_rs_host_kernel_check`                    | yes        | `Environment -> String -> String -> String -> UInt64 -> USize -> IO KernelOutcome`                            |
//! | `lean_rs_host_kernel_check_progress`           | yes        | `Environment -> String -> String -> String -> UInt64 -> USize -> USize -> USize -> IO (Except UInt8 KernelOutcome)` |
//! | `lean_rs_host_check_evidence`                  | yes        | `Environment -> Evidence -> IO EvidenceStatus`                                                                |
//! | `lean_rs_host_evidence_summary`                | yes        | `Environment -> Evidence -> IO ProofSummary`                                                                  |
//! | `lean_rs_host_meta_infer_type`                 | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                   |
//! | `lean_rs_host_meta_whnf`                       | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                   |
//! | `lean_rs_host_meta_heartbeat_burn`             | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                   |
//! | `lean_rs_host_meta_is_def_eq`                  | optional   | `Environment -> (Expr × Expr × UInt8) -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Bool)`                  |
//! | `lean_rs_host_meta_pp_expr`                    | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse String)`                                 |
//! | `lean_rs_host_process_with_info_tree`          | optional   | `Environment -> String -> String -> String -> UInt64 -> USize -> IO ProcessedFile`                            |
//! | `lean_rs_host_process_module_with_info_tree`        | optional   | `Environment -> String -> String -> String -> UInt64 -> USize -> IO ProcessModuleOutcome`             |
//!
//! Missing **mandatory** symbols surface at `load_capabilities` as
//! [`lean_rs::HostStage::Link`] — failures bind to the capability's load,
//! not to the first query. Missing **optional** symbols degrade
//! gracefully: [`LeanSession::run_meta`] returns
//! [`crate::host::meta::LeanMetaResponse::Unsupported`] against a service whose
//! address did not resolve, [`LeanSession::process_with_info_tree`]
//! returns [`crate::host::process::ProcessFileOutcome::Unsupported`],
//! [`LeanSession::process_module_with_info_tree`] returns
//! [`crate::host::process::ProcessModuleOutcome::Unsupported`],
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
//! the session. `import` itself is **not** counted as a query FFI call
//! — pool reuse vs. fresh import is tracked at the
//! [`crate::host::pool::SessionPool`] level instead.
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
//! Long-running session operations additionally accept
//! `Option<&dyn LeanProgressSink>`. `None` allocates no callback handle
//! and preserves the existing fast path. `Some(sink)` delivers
//! phase-local [`crate::host::progress::LeanProgressEvent`] values on
//! the Lean-bound worker thread. A progress sink must not call back into
//! the same session.
//!
//! The Rust side passes the `.olean` search path (resolved by
//! [`crate::host::lake::LakeProject`]) as the first argument to
//! `lean_rs_host_session_import`; the Lean shim only has to call
//! `Lean.initSearchPath` and `Lean.importModules` on it. Path-layout
//! knowledge stays in Rust.
//!
//! ## Lifetime story
//!
//! - `LeanSession<'lean, 'c>` borrows `&'c LeanCapabilities<'lean, '_>`.
//! - The session's owned `Obj<'lean>` is independent of `'c`; it carries
//!   one Lean refcount on the imported environment, anchored to the
//!   runtime.
//! - Local `LeanExported<'lean, '_, ...>` values used per query borrow
//!   from the capability's `LeanLibrary` through the lifetime inferred
//!   at the `LeanExported::from_function_address` call site; they die
//!   at end-of-method; their `'lean`-anchored outputs escape cleanly.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment naming the precondition. The blanket allow is
// scoped to this single dispatch site, per
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]
// `run_meta` is `pub` but bounded on `lean_rs::abi::traits::{LeanAbi, TryFromLean}`.
// `LeanAbi` is sealed-public; `TryFromLean` is `pub(crate)`. The bound is a
// crate-internal compatibility requirement, not a downstream extension point
// (the meta-service registry is closed by `host::meta::service`). Same
// precedent as `module::exported.rs`.
#![allow(private_bounds, private_interfaces)]

use core::cell::Cell;
use core::ffi::c_void;
use std::sync::Mutex;
use std::time::Instant;

use crate::host::cancellation::{LeanCancellationToken, check_cancellation};
use crate::host::capabilities::LeanCapabilities;
use crate::host::elaboration::{LeanElabFailure, LeanElabOptions};
use crate::host::evidence::{EvidenceStatus, LeanEvidence, LeanKernelOutcome, ProofSummary};
use crate::host::meta::{LeanMetaOptions, LeanMetaResponse, LeanMetaService};
use crate::host::process::{ProcessFileOutcome, ProcessModuleOutcome, ProcessedFile};
use crate::host::progress::{LeanProgressSink, ProgressBridge, report_progress};
use lean_rs::Obj;
use lean_rs::abi::structure::{alloc_ctor_with_objects, take_ctor_objects};
use lean_rs::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error, sealed};
#[cfg(doc)]
use lean_rs::error::HostStage;
use lean_rs::error::LeanResult;
use lean_rs::module::{DecodeCallResult, LeanArgs, LeanExported, LeanIo, LeanLibrary};
use lean_rs::{LeanDeclaration, LeanExpr, LeanName};
use lean_rs_sys::lean_object;

// -- SessionStats: per-session dispatch metrics --------------------------

/// Cumulative dispatch metrics for one [`LeanSession`].
///
/// Snapshot via [`LeanSession::stats`]. Each typed query method records
/// one FFI call; the bulk methods additionally record the per-item batch
/// size. `elapsed_ns` accumulates the wall time spent inside the inner
/// `.call(...)` dispatch (measured with [`Instant::now`]) — it excludes
/// Rust-side argument marshaling, name lookup, and result decoding so
/// the number is comparable across singular and bulk paths.
///
/// `import` is **not** counted: import vs. reuse is tracked at the
/// [`crate::host::pool::SessionPool`] level. Construction of a session
/// always pays for one import, and that cost is reported by
/// [`crate::host::pool::PoolStats::imports_performed`] when the session
/// flows through a pool.
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

/// Lean-side declaration-listing filter.
///
/// The default is tuned for user-facing declaration browsers: include
/// private names because callers may be indexing the current project,
/// but drop compiler-generated and internal-detail names that usually
/// swamp the list with implementation artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeanDeclarationFilter {
    /// Keep names Lean marks as private.
    pub include_private: bool,
    /// Keep generated names with numeric components.
    pub include_generated: bool,
    /// Keep Lean internal-detail names such as `_`, `match_`, `proof_`,
    /// and similar implementation artifacts.
    pub include_internal: bool,
}

impl Default for LeanDeclarationFilter {
    fn default() -> Self {
        Self {
            include_private: true,
            include_generated: false,
            include_internal: false,
        }
    }
}

impl<'lean> IntoLean<'lean> for LeanDeclarationFilter {
    fn into_lean(self, runtime: &'lean lean_rs::LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.include_private.into_lean(runtime),
                self.include_generated.into_lean(runtime),
                self.include_internal.into_lean(runtime),
            ],
        )
    }
}

impl<'lean> TryFromLean<'lean> for LeanDeclarationFilter {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [include_private_o, include_generated_o, include_internal_o] =
            take_ctor_objects::<3>(obj, 0, "DeclarationFilter")?;
        Ok(Self {
            include_private: bool::try_from_lean(include_private_o)?,
            include_generated: bool::try_from_lean(include_generated_o)?,
            include_internal: bool::try_from_lean(include_internal_o)?,
        })
    }
}

impl sealed::SealedAbi for LeanDeclarationFilter {}

impl<'lean> LeanAbi<'lean> for LeanDeclarationFilter {
    type CRepr = *mut lean_object;

    fn into_c(self, runtime: &'lean lean_rs::LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — called only by LeanExported"
    )]
    fn from_c(c: Self::CRepr, runtime: &'lean lean_rs::LeanRuntime) -> LeanResult<Self> {
        if c.is_null() {
            return Err(conversion_error("Lean DeclarationFilter returned a null pointer"));
        }
        // SAFETY: boxed C-ABI return values carry one owned refcount.
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Self::try_from_lean(obj)
    }
}

// -- SessionSymbols: pre-resolved C-ABI function addresses ---------------

/// The session function-symbol addresses [`LeanSession`] dispatches
/// through.
///
/// Populated once at [`LeanCapabilities::new`] time; read by every
/// session method without further `dlsym`. Each mandatory field is a
/// non-null `*mut c_void` (raw function entry point); the safety
/// obligation that these point at Lake-emitted functions with the
/// expected ABI is discharged by [`Self::resolve`] only resolving
/// symbols whose Lean signatures are pinned in the module docstring
/// above. Meta-service fields are `Option<*mut c_void>`: missing
/// symbols degrade to [`crate::host::meta::LeanMetaResponse::Unsupported`] at the
/// `run_meta` dispatch site instead of failing capability load.
pub(crate) struct SessionSymbols {
    pub(crate) session_import: *mut c_void,
    pub(crate) session_import_progress: *mut c_void,
    pub(crate) name_from_string: *mut c_void,
    pub(crate) name_to_string: *mut c_void,
    pub(crate) env_query_declaration: *mut c_void,
    pub(crate) env_query_declarations_bulk: *mut c_void,
    pub(crate) env_query_declarations_bulk_progress: *mut c_void,
    pub(crate) env_list_declarations: *mut c_void,
    pub(crate) env_list_declarations_filtered: *mut c_void,
    pub(crate) env_list_declarations_filtered_progress: *mut c_void,
    pub(crate) env_declaration_source_range: *mut c_void,
    pub(crate) env_declaration_type: *mut c_void,
    pub(crate) env_declaration_type_bulk: *mut c_void,
    pub(crate) env_declaration_type_bulk_progress: *mut c_void,
    pub(crate) env_declaration_kind: *mut c_void,
    pub(crate) env_declaration_kind_bulk: *mut c_void,
    pub(crate) env_declaration_kind_bulk_progress: *mut c_void,
    pub(crate) env_declaration_name: *mut c_void,
    pub(crate) env_declaration_name_bulk: *mut c_void,
    pub(crate) env_declaration_name_bulk_progress: *mut c_void,
    pub(crate) env_expr_to_string_raw: *mut c_void,
    pub(crate) elaborate: *mut c_void,
    pub(crate) elaborate_bulk: *mut c_void,
    pub(crate) elaborate_bulk_progress: *mut c_void,
    pub(crate) kernel_check: *mut c_void,
    pub(crate) kernel_check_progress: *mut c_void,
    pub(crate) check_evidence: *mut c_void,
    pub(crate) evidence_summary: *mut c_void,
    pub(crate) meta_infer_type: Option<*mut c_void>,
    pub(crate) meta_whnf: Option<*mut c_void>,
    pub(crate) meta_heartbeat_burn: Option<*mut c_void>,
    pub(crate) meta_is_def_eq: Option<*mut c_void>,
    pub(crate) meta_pp_expr: Option<*mut c_void>,
    pub(crate) process_with_info_tree: Option<*mut c_void>,
    pub(crate) process_module_with_info_tree: Option<*mut c_void>,
}

impl SessionSymbols {
    /// Resolve session function symbols from `library`. The twenty-eight
    /// baseline symbols are mandatory; the six optional symbols (five
    /// bounded `MetaM` services and the info-tree projection) degrade
    /// to per-call `Unsupported` returns if missing.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage [`HostStage::Link`] on
    /// the first mandatory symbol that fails to resolve; the
    /// diagnostic embeds the missing symbol name and the library path
    /// (via [`LeanLibrary::resolve_function_symbol`]). Missing
    /// **optional** meta-service symbols never fail capability load —
    /// the corresponding field is `None` and the `run_meta` dispatch
    /// site synthesises an `Unsupported` response.
    pub(crate) fn resolve(library: &LeanLibrary<'_>) -> LeanResult<Self> {
        Ok(Self {
            session_import: library.resolve_function_symbol("lean_rs_host_session_import")?,
            session_import_progress: library.resolve_function_symbol("lean_rs_host_session_import_progress")?,
            name_from_string: library.resolve_function_symbol("lean_rs_host_name_from_string")?,
            name_to_string: library.resolve_function_symbol("lean_rs_host_name_to_string")?,
            env_query_declaration: library.resolve_function_symbol("lean_rs_host_env_query_declaration")?,
            env_query_declarations_bulk: library.resolve_function_symbol("lean_rs_host_env_query_declarations_bulk")?,
            env_query_declarations_bulk_progress: library
                .resolve_function_symbol("lean_rs_host_env_query_declarations_bulk_progress")?,
            env_list_declarations: library.resolve_function_symbol("lean_rs_host_env_list_declarations")?,
            env_list_declarations_filtered: library
                .resolve_function_symbol("lean_rs_host_env_list_declarations_filtered")?,
            env_list_declarations_filtered_progress: library
                .resolve_function_symbol("lean_rs_host_env_list_declarations_filtered_progress")?,
            env_declaration_source_range: library
                .resolve_function_symbol("lean_rs_host_env_declaration_source_range")?,
            env_declaration_type: library.resolve_function_symbol("lean_rs_host_env_declaration_type")?,
            env_declaration_type_bulk: library.resolve_function_symbol("lean_rs_host_env_declaration_type_bulk")?,
            env_declaration_type_bulk_progress: library
                .resolve_function_symbol("lean_rs_host_env_declaration_type_bulk_progress")?,
            env_declaration_kind: library.resolve_function_symbol("lean_rs_host_env_declaration_kind")?,
            env_declaration_kind_bulk: library.resolve_function_symbol("lean_rs_host_env_declaration_kind_bulk")?,
            env_declaration_kind_bulk_progress: library
                .resolve_function_symbol("lean_rs_host_env_declaration_kind_bulk_progress")?,
            env_declaration_name: library.resolve_function_symbol("lean_rs_host_env_declaration_name")?,
            env_declaration_name_bulk: library.resolve_function_symbol("lean_rs_host_env_declaration_name_bulk")?,
            env_declaration_name_bulk_progress: library
                .resolve_function_symbol("lean_rs_host_env_declaration_name_bulk_progress")?,
            env_expr_to_string_raw: library.resolve_function_symbol("lean_rs_host_env_expr_to_string_raw")?,
            elaborate: library.resolve_function_symbol("lean_rs_host_elaborate")?,
            elaborate_bulk: library.resolve_function_symbol("lean_rs_host_elaborate_bulk")?,
            elaborate_bulk_progress: library.resolve_function_symbol("lean_rs_host_elaborate_bulk_progress")?,
            kernel_check: library.resolve_function_symbol("lean_rs_host_kernel_check")?,
            kernel_check_progress: library.resolve_function_symbol("lean_rs_host_kernel_check_progress")?,
            check_evidence: library.resolve_function_symbol("lean_rs_host_check_evidence")?,
            evidence_summary: library.resolve_function_symbol("lean_rs_host_evidence_summary")?,
            meta_infer_type: library.resolve_optional_function_symbol("lean_rs_host_meta_infer_type"),
            meta_whnf: library.resolve_optional_function_symbol("lean_rs_host_meta_whnf"),
            meta_heartbeat_burn: library.resolve_optional_function_symbol("lean_rs_host_meta_heartbeat_burn"),
            meta_is_def_eq: library.resolve_optional_function_symbol("lean_rs_host_meta_is_def_eq"),
            meta_pp_expr: library.resolve_optional_function_symbol("lean_rs_host_meta_pp_expr"),
            process_with_info_tree: library.resolve_optional_function_symbol("lean_rs_host_process_with_info_tree"),
            process_module_with_info_tree: library
                .resolve_optional_function_symbol("lean_rs_host_process_module_with_info_tree"),
        })
    }

    /// Look up the cached address for a meta service by name. Returns
    /// `None` if the service was absent from the loaded capability at
    /// resolve time.
    pub(crate) fn meta_address_by_name(&self, name: &str) -> Option<*mut c_void> {
        match name {
            "lean_rs_host_meta_infer_type" => self.meta_infer_type,
            "lean_rs_host_meta_whnf" => self.meta_whnf,
            "lean_rs_host_meta_heartbeat_burn" => self.meta_heartbeat_burn,
            "lean_rs_host_meta_is_def_eq" => self.meta_is_def_eq,
            "lean_rs_host_meta_pp_expr" => self.meta_pp_expr,
            _ => None,
        }
    }
}

// -- LeanSession ---------------------------------------------------------

/// A long-lived Lean session over an imported environment.
///
/// Construct via [`LeanCapabilities::session`]. The session owns the
/// imported `Lean.Environment` privately (never exposed) and dispatches
/// each typed query through the capability's pre-resolved symbol
/// addresses. Neither [`Send`] nor [`Sync`]: inherited from the
/// contained `Obj<'lean>` and the borrow of `LeanCapabilities`.
pub struct LeanSession<'lean, 'c> {
    capabilities: &'c LeanCapabilities<'lean, 'c>,
    /// The imported `Lean.Environment`. Private — Rust never inspects
    /// the environment directly; every query routes through a Lean
    /// capability export.
    environment: Obj<'lean>,
    /// Per-session dispatch metrics. `Cell` because every query method
    /// takes `&mut self` but the bulk path can also be invoked through a
    /// shared reference (e.g. inside a fold helper) — keeping the
    /// counter in `Cell` makes the recording uniform without adding an
    /// extra `&mut` borrow at each call site.
    stats: Cell<SessionStats>,
}

/// Process-wide serialization for [`LeanSession::import`]. See the
/// comment at the lock-acquire site for the Lean-4.30 race it closes.
static SESSION_IMPORT_LOCK: Mutex<()> = Mutex::new(());

impl<'lean, 'c> LeanSession<'lean, 'c> {
    /// Import the named modules into a fresh Lean environment and wrap
    /// it as a session.
    ///
    /// The Lean-side `lean_rs_host_session_import` receives the Lake
    /// project root (so it can `Lean.initSearchPath` the `.olean`
    /// directory) and the module-name list, and returns the resulting
    /// environment. Failures surface as
    /// [`lean_rs::LeanError::LeanException`] with the message Lean produced.
    pub(crate) fn import(
        capabilities: &'c LeanCapabilities<'lean, 'c>,
        imports: &[&str],
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Self> {
        let _span = tracing::info_span!(
            target: "lean_rs",
            "lean_rs.host.session.import",
            imports_len = imports.len(),
        )
        .entered();
        check_cancellation(cancellation)?;
        let runtime = capabilities.host().runtime();
        let project = capabilities.host().project();
        let search_paths: Vec<String> = vec![
            project.olean_search_path().to_string_lossy().into_owned(),
            crate::host::lake::LakeProject::interop_olean_search_path()?
                .to_string_lossy()
                .into_owned(),
            crate::host::lake::LakeProject::shim_olean_search_path()?
                .to_string_lossy()
                .into_owned(),
        ];
        let imports_owned: Vec<String> = imports.iter().map(|&s| s.to_owned()).collect();
        // Lean 4.30 strictly enforces `enableInitializersExecution` before
        // `importModules (loadExts := true)`. The flag is process-global,
        // but `Lean.withImporting` (wrapped around every import) resets it
        // on completion — two threads importing concurrently race the
        // shim's enable→import sequence and the loser sees the flag
        // cleared by the winner's reset. Serializing the import phase
        // across the process matches Lean's "single execution thread
        // accessing the global references" requirement. Sessions operate
        // concurrently on their own `Environment` values once import
        // returns; the lock spans only the FFI call.
        let _import_guard = SESSION_IMPORT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let environment = if let Some(sink) = progress {
            let bridge = ProgressBridge::new(sink, "import", Some(u64::try_from(imports.len()).unwrap_or(u64::MAX)))?;
            let (handle, trampoline) = bridge.abi_parts();
            let address = capabilities.symbols().session_import_progress;
            // SAFETY: `address` was resolved by `SessionSymbols::resolve`
            // against the shim library, which outlives `'c`. The
            // signature `(Array String, Array String, USize, USize) ->
            // IO (Except UInt8 Environment)` matches the Lean-side
            // progress import shim.
            let import_fn: LeanExported<'lean, '_, (Vec<String>, Vec<String>, usize, usize), LeanIo<Obj<'lean>>> =
                unsafe { LeanExported::from_function_address(runtime, address) };
            let raw = import_fn.call(search_paths, imports_owned, handle, trampoline)?;
            bridge.decode(raw)?
        } else {
            let address = capabilities.symbols().session_import;
            // SAFETY: `address` was resolved by `SessionSymbols::resolve`
            // against the shim library, which outlives `'c`. The
            // signature `(Vec<String>, Vec<String>) -> IO Environment`
            // matches the Lean-side `lean_rs_host_session_import` —
            // first argument is the list of `.olean` search-path
            // entries (the user's package + the shim package), second
            // is the module-name list.
            let import_fn: LeanExported<'lean, '_, (Vec<String>, Vec<String>), LeanIo<Obj<'lean>>> =
                unsafe { LeanExported::from_function_address(runtime, address) };
            import_fn.call(search_paths, imports_owned)?
        };
        Ok(Self {
            capabilities,
            environment,
            stats: Cell::new(SessionStats::default()),
        })
    }

    /// Wrap a previously-imported `Lean.Environment` as a fresh
    /// [`LeanSession`] over `capabilities`.
    ///
    /// Crate-private; only [`crate::host::pool::SessionPool::acquire`]
    /// calls this to recycle a pooled environment under a new
    /// capability borrow. The returned session's [`SessionStats`] start
    /// at zero — accumulated counters from the previous owner do not
    /// leak across pool checkouts.
    pub(crate) fn from_environment(capabilities: &'c LeanCapabilities<'lean, 'c>, environment: Obj<'lean>) -> Self {
        Self {
            capabilities,
            environment,
            stats: Cell::new(SessionStats::default()),
        }
    }

    /// Consume the session and return its owned `Lean.Environment`.
    ///
    /// Crate-private; only [`crate::host::pool::SessionPool`] uses this
    /// to reclaim the environment when a [`crate::host::pool::PooledSession`]
    /// drops. The returned `Obj<'lean>` carries one Lean refcount, which
    /// the pool is responsible for either pushing back into the free
    /// list (in which case `Drop` runs later when the pool itself
    /// drops) or releasing immediately (when at capacity).
    pub(crate) fn into_environment(self) -> Obj<'lean> {
        self.environment
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

    fn decode_strings_cached(raw: Vec<Obj<'lean>>) -> LeanResult<Vec<String>> {
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        let Some(first_key) = raw.first().map(Obj::as_raw_borrowed) else {
            return Ok(Vec::new());
        };
        if raw.iter().all(|obj| obj.as_raw_borrowed() == first_key) {
            let len = raw.len();
            let mut raw_iter = raw.into_iter();
            let Some(first) = raw_iter.next() else {
                return Ok(Vec::new());
            };
            let value = String::try_from_lean(first)?;
            return Ok(vec![value; len]);
        }
        let mut out = Vec::with_capacity(raw.len());
        for obj in raw {
            out.push(String::try_from_lean(obj)?);
        }
        Ok(out)
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
        let address = self.capabilities.symbols().env_query_declaration;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO (Option Declaration)`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<Option<LeanDeclaration<'lean>>>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = query.call(self.environment.clone(), name_handle);
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
        let address = self.capabilities.symbols().env_list_declarations;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `Environment -> IO (Array Name)`.
        let list: LeanExported<'lean, '_, (Obj<'lean>,), LeanIo<Vec<Obj<'lean>>>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let raw = list.call(self.environment.clone());
        self.record_call(0, t.elapsed());
        raw?.into_iter().map(LeanName::try_from_lean).collect()
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
        let raw = if let Some(sink) = progress {
            let bridge = ProgressBridge::new(sink, "list_declarations_filtered", None)?;
            let (handle, trampoline) = bridge.abi_parts();
            let address = self.capabilities.symbols().env_list_declarations_filtered_progress;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, DeclarationFilter, USize, USize) ->
            // IO (Except UInt8 (Array Name))`.
            let list: LeanExported<'lean, '_, (Obj<'lean>, LeanDeclarationFilter, usize, usize), LeanIo<Obj<'lean>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = list.call(self.environment.clone(), *filter, handle, trampoline);
            self.record_call(0, t.elapsed());
            bridge.decode::<Vec<Obj<'lean>>>(result?)?
        } else {
            let address = self.capabilities.symbols().env_list_declarations_filtered;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, DeclarationFilter) -> IO (Array Name)`.
            let list: LeanExported<'lean, '_, (Obj<'lean>, LeanDeclarationFilter), LeanIo<Vec<Obj<'lean>>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = list.call(self.environment.clone(), *filter);
            self.record_call(0, t.elapsed());
            result?
        };
        raw.into_iter().map(LeanName::try_from_lean).collect()
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
        let address = self.capabilities.symbols().env_declaration_source_range;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name, Array String) -> IO (Option SourceRange)`.
        let query: LeanExported<
            'lean,
            '_,
            (Obj<'lean>, LeanName<'lean>, Vec<String>),
            LeanIo<Option<LeanSourceRange>>,
        > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = query.call(self.environment.clone(), name_handle, source_roots);
        self.record_call(0, t.elapsed());
        result
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
        let address = self.capabilities.symbols().env_declaration_type;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO (Option Expr)`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<Option<LeanExpr<'lean>>>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = query.call(self.environment.clone(), name_handle);
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
            let address = self.capabilities.symbols().env_declaration_type_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String) -> IO (Array (Option Expr))`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>), LeanIo<Vec<Option<LeanExpr<'lean>>>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let mut result = query.call(self.environment.clone(), names_owned)?;
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let value = result.pop().unwrap_or(None);
            return Ok(vec![value; names.len()]);
        }
        let names_owned: Vec<String> = names.iter().map(|&name| name.to_owned()).collect();
        if let Some(sink) = progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let bridge = ProgressBridge::new(sink, "declaration_type_bulk", total)?;
            let (handle, trampoline) = bridge.abi_parts();
            let address = self.capabilities.symbols().env_declaration_type_bulk_progress;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String, USize, USize) ->
            // IO (Except UInt8 (Array (Option Expr)))`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>, usize, usize), LeanIo<Obj<'lean>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = query.call(self.environment.clone(), names_owned, handle, trampoline);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            bridge.decode(result?)
        } else {
            let address = self.capabilities.symbols().env_declaration_type_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String) -> IO (Array (Option Expr))`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>), LeanIo<Vec<Option<LeanExpr<'lean>>>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = query.call(self.environment.clone(), names_owned);
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
        let address = self.capabilities.symbols().env_declaration_kind;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO String`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<String>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = query.call(self.environment.clone(), name_handle);
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
            let address = self.capabilities.symbols().env_declaration_kind_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String) -> IO (Array String)`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>), LeanIo<Vec<Obj<'lean>>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let mut result = Self::decode_strings_cached(query.call(self.environment.clone(), names_owned)?)?;
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let value = result.pop().unwrap_or_default();
            return Ok(vec![value; names.len()]);
        }
        let names_owned: Vec<String> = names.iter().map(|&name| name.to_owned()).collect();
        if let Some(sink) = progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let bridge = ProgressBridge::new(sink, "declaration_kind_bulk", total)?;
            let (handle, trampoline) = bridge.abi_parts();
            let address = self.capabilities.symbols().env_declaration_kind_bulk_progress;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String, USize, USize) ->
            // IO (Except UInt8 (Array String))`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>, usize, usize), LeanIo<Obj<'lean>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = query.call(self.environment.clone(), names_owned, handle, trampoline);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let raw = bridge.decode::<Vec<Obj<'lean>>>(result?)?;
            Self::decode_strings_cached(raw)
        } else {
            let address = self.capabilities.symbols().env_declaration_kind_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String) -> IO (Array String)`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>), LeanIo<Vec<Obj<'lean>>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = query.call(self.environment.clone(), names_owned);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            Self::decode_strings_cached(result?)
        }
    }

    /// The Lean-rendered display string of `name`. Round-trips a name
    /// through the capability's `Name.toString` shim so callers see the
    /// same canonical form Lean would log.
    ///
    /// Diagnostic only — not a semantic key. Use
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
        let address = self.capabilities.symbols().env_declaration_name;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO String`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<String>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = query.call(self.environment.clone(), name_handle);
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
            let address = self.capabilities.symbols().env_declaration_name_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String) -> IO (Array String)`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>), LeanIo<Vec<Obj<'lean>>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let mut result = Self::decode_strings_cached(query.call(self.environment.clone(), names_owned)?)?;
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let value = result.pop().unwrap_or_default();
            return Ok(vec![value; names.len()]);
        }
        let names_owned: Vec<String> = names.iter().map(|&name| name.to_owned()).collect();
        if let Some(sink) = progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let bridge = ProgressBridge::new(sink, "declaration_name_bulk", total)?;
            let (handle, trampoline) = bridge.abi_parts();
            let address = self.capabilities.symbols().env_declaration_name_bulk_progress;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String, USize, USize) ->
            // IO (Except UInt8 (Array String))`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>, usize, usize), LeanIo<Obj<'lean>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = query.call(self.environment.clone(), names_owned, handle, trampoline);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            let raw = bridge.decode::<Vec<Obj<'lean>>>(result?)?;
            Self::decode_strings_cached(raw)
        } else {
            let address = self.capabilities.symbols().env_declaration_name_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String) -> IO (Array String)`.
            let query: LeanExported<'lean, '_, (Obj<'lean>, Vec<String>), LeanIo<Vec<Obj<'lean>>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = query.call(self.environment.clone(), names_owned);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            Self::decode_strings_cached(result?)
        }
    }

    /// Render an opaque [`LeanName`] handle as its dotted-string form,
    /// routed through the capability's `Name.toString` shim.
    ///
    /// This is the supported way to turn a `LeanName` (e.g. an element
    /// of [`Self::list_declarations_filtered`]'s result) into Rust text.
    /// The output is diagnostic — not a semantic key — and equality on
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
        let address = self.capabilities.symbols().name_to_string;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `Name -> String` (pure, not IO).
        let render: LeanExported<'lean, '_, (LeanName<'lean>,), String> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = render.call(name.clone());
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

    /// Render `expr` via `Expr.toString` — the cheap, deterministic
    /// projection.
    ///
    /// Walks the syntax tree directly: no `MetaM`, no notation lookup,
    /// no binder pretty-printing. The result is a legible-but-ugly
    /// dump suitable for indexing, logging, and search keys. For the
    /// form a Lean user reads, use the optional
    /// [`crate::host::meta::pp_expr`] service through
    /// [`Self::run_meta`] instead — it pays for elaboration context to
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
        let address = self.capabilities.symbols().env_expr_to_string_raw;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `Expr -> String` (pure, not IO).
        let render: LeanExported<'lean, '_, (LeanExpr<'lean>,), String> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = render.call(expr.clone());
        self.record_call(0, t.elapsed());
        result
    }

    /// Parse, elaborate, and project a Lean source string into its
    /// `Elab.InfoTree`.
    ///
    /// Drives Lean's `IO.processCommands` pipeline with info collection
    /// enabled, walks every captured info tree, and projects each
    /// `TermInfo` / `TacticInfo` / `CommandInfo` / identifier reference
    /// into a [`ProcessedFile`] value. The projection is the FFI
    /// boundary — raw `InfoTree` does not cross, because it carries
    /// metavariable contexts that cannot be revived outside the
    /// session that produced them.
    ///
    /// Goal text in `TacticInfoNode::goals_before` /
    /// `TacticInfoNode::goals_after` is pre-rendered through Lean's
    /// `Meta.ppGoal` inside the elaboration context; the cumulative
    /// goal-byte sum is bounded by [`LeanElabOptions::diagnostic_byte_limit`].
    ///
    /// The shim is optional. When the loaded capability dylib does not
    /// export `lean_rs_host_process_with_info_tree`, the method returns
    /// [`ProcessFileOutcome::Unsupported`] without an FFI call —
    /// matching the meta-service degradation pattern. There is no
    /// separate timeout arm because `IO.processCommands` catches
    /// per-command exceptions and attaches them to the message log;
    /// inspect [`ProcessedFile::diagnostics`] to detect heartbeat
    /// exhaustion.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side shim
    /// raises through `IO`. Returns [`lean_rs::LeanError::Host`] with
    /// stage [`HostStage::Conversion`] if the Lean return value does
    /// not decode into [`ProcessedFile`].
    pub fn process_with_info_tree(
        &mut self,
        source: &str,
        options: &LeanElabOptions,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<ProcessFileOutcome> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.process_with_info_tree",
            source_len = source.len(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
        check_cancellation(cancellation)?;
        let Some(address) = self.capabilities.symbols().process_with_info_tree else {
            return Ok(ProcessFileOutcome::Unsupported);
        };
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, String, String, String, UInt64, USize) ->
        // IO ProcessedFile`.
        let call: LeanExported<'lean, '_, (Obj<'lean>, &str, &str, &str, u64, usize), LeanIo<ProcessedFile>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = call.call(
            self.environment.clone(),
            source,
            options.namespace_context_str(),
            options.file_label_str(),
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
        );
        self.record_call(0, t.elapsed());
        Ok(ProcessFileOutcome::Processed(result?))
    }

    /// Parse a Lean source file's header, then resume elaboration of
    /// the body against the session's open env, returning the
    /// info-tree projection in the original file's coordinate system.
    ///
    /// Unlike [`Self::process_with_info_tree`] (which is right for
    /// header-less snippets), this method calls
    /// `Lean.Parser.parseHeader` first and resumes `IO.processCommands`
    /// from the parser state the header parser produced. The shared
    /// `InputContext.fileMap` covers the whole source, so positions in
    /// the returned [`ProcessedFile`] use the original file's line and
    /// column numbers with no Rust-side offset arithmetic.
    ///
    /// Three real outcomes plus an `Unsupported` degradation arm:
    ///
    /// - [`ProcessModuleOutcome::Ok`] — header parsed; every
    ///   user-written import is present in the session's open env; the
    ///   body was processed. `imports` lists the user-written modules
    ///   (Lean's auto-inserted `Init` is filtered out by the shim).
    /// - [`ProcessModuleOutcome::MissingImports`] — header
    ///   parsed but some imports name modules the open env lacks. The
    ///   body still elaborated against whatever the env carries; the
    ///   projection is populated and `missing` lists the absent
    ///   modules. Soft failure — callers typically surface it as a
    ///   warning rather than as an error.
    /// - [`ProcessModuleOutcome::HeaderParseFailed`] — the
    ///   header itself did not parse (e.g. a malformed `import` line).
    ///   `IO.processCommands` was not run; only the header diagnostics
    ///   are returned.
    /// - [`ProcessModuleOutcome::Unsupported`] — the loaded
    ///   capability dylib does not export
    ///   `lean_rs_host_process_module_with_info_tree`. No FFI call was
    ///   made.
    ///
    /// As with [`Self::process_with_info_tree`], heartbeat exhaustion
    /// during a single command's elaboration surfaces as an
    /// error-severity entry in
    /// [`ProcessedFile::diagnostics`](crate::host::process::ProcessedFile)
    /// rather than as a separate wire arm.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before dispatch. Returns
    /// [`lean_rs::LeanError::LeanException`] if the Lean-side shim
    /// raises through `IO`. Returns [`lean_rs::LeanError::Host`] with
    /// stage [`HostStage::Conversion`] if the Lean return value does
    /// not decode into [`ProcessModuleOutcome`].
    pub fn process_module_with_info_tree(
        &mut self,
        source: &str,
        options: &LeanElabOptions,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<ProcessModuleOutcome> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.process_module_with_info_tree",
            source_len = source.len(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
        check_cancellation(cancellation)?;
        let Some(address) = self.capabilities.symbols().process_module_with_info_tree else {
            return Ok(ProcessModuleOutcome::Unsupported);
        };
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, String, String, String, UInt64, USize) ->
        // IO ProcessModuleOutcome`.
        let call: LeanExported<'lean, '_, (Obj<'lean>, &str, &str, &str, u64, usize), LeanIo<ProcessModuleOutcome>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = call.call(
            self.environment.clone(),
            source,
            options.namespace_context_str(),
            options.file_label_str(),
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
        );
        self.record_call(0, t.elapsed());
        result
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
        source: &str,
        expected_type: Option<&LeanExpr<'lean>>,
        options: &LeanElabOptions,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<Result<LeanExpr<'lean>, LeanElabFailure>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.elaborate",
            source_len = source.len(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
        check_cancellation(cancellation)?;
        let address = self.capabilities.symbols().elaborate;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, String, Option Expr, String, String,
        // UInt64, USize) -> IO (Except ElabFailure Expr)`.
        let call: LeanExported<
            'lean,
            '_,
            (Obj<'lean>, &str, Option<LeanExpr<'lean>>, &str, &str, u64, usize),
            LeanIo<Result<LeanExpr<'lean>, LeanElabFailure>>,
        > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = call.call(
            self.environment.clone(),
            source,
            expected_type.cloned(),
            options.namespace_context_str(),
            options.file_label_str(),
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
        );
        self.record_call(0, t.elapsed());
        result
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
        source: &str,
        options: &LeanElabOptions,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<LeanKernelOutcome<'lean>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.kernel_check",
            source_len = source.len(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
        check_cancellation(cancellation)?;
        if let Some(sink) = progress {
            let bridge = ProgressBridge::new(sink, "kernel_check", Some(1))?;
            let (handle, trampoline) = bridge.abi_parts();
            let address = self.capabilities.symbols().kernel_check_progress;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, String, String, String, UInt64, USize,
            // USize, USize) -> IO (Except UInt8 KernelOutcome)`.
            let call: LeanExported<
                'lean,
                '_,
                (Obj<'lean>, &str, &str, &str, u64, usize, usize, usize),
                LeanIo<Obj<'lean>>,
            > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = call.call(
                self.environment.clone(),
                source,
                options.namespace_context_str(),
                options.file_label_str(),
                options.heartbeats(),
                options.diagnostic_byte_limit_usize(),
                handle,
                trampoline,
            );
            self.record_call(0, t.elapsed());
            bridge.decode(result?)
        } else {
            let address = self.capabilities.symbols().kernel_check;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, String, String, String, UInt64, USize) ->
            // IO KernelOutcome`.
            let call: LeanExported<
                'lean,
                '_,
                (Obj<'lean>, &str, &str, &str, u64, usize),
                LeanIo<LeanKernelOutcome<'lean>>,
            > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = call.call(
                self.environment.clone(),
                source,
                options.namespace_context_str(),
                options.file_label_str(),
                options.heartbeats(),
                options.diagnostic_byte_limit_usize(),
            );
            self.record_call(0, t.elapsed());
            result
        }
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
    /// is the supported way to ask "is this evidence still valid?" —
    /// the kernel runs fresh.
    ///
    /// The returned [`EvidenceStatus`] mirrors
    /// [`LeanKernelOutcome::status`]: `Checked` on success, `Rejected`
    /// if the kernel now refuses the declaration, `Unavailable` if
    /// the Lean shim caught an `IO` exception. The Lean fixture does
    /// not currently emit `Unsupported` from this path — `Unsupported`
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
        let address = self.capabilities.symbols().check_evidence;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Evidence) -> IO EvidenceStatus`.
        let call: LeanExported<'lean, '_, (Obj<'lean>, LeanEvidence<'lean>), LeanIo<EvidenceStatus>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = call.call(self.environment.clone(), handle.clone());
        self.record_call(0, t.elapsed());
        result
    }

    /// Project a previously captured [`LeanEvidence`] into a bounded
    /// [`ProofSummary`] for diagnostics or storage.
    ///
    /// The Lean shim renders the captured declaration's name, kind,
    /// and type expression as three byte-bounded `String`s — no
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
        let address = self.capabilities.symbols().evidence_summary;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Evidence) -> IO ProofSummary`.
        let call: LeanExported<'lean, '_, (Obj<'lean>, LeanEvidence<'lean>), LeanIo<ProofSummary>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = call.call(self.environment.clone(), handle.clone());
        self.record_call(0, t.elapsed());
        result
    }

    /// Invoke a registered bounded [`MetaM`](https://leanprover.github.io/theorem_proving_in_lean4/)
    /// service against the imported environment.
    ///
    /// The session looks up the service's cached address; if the
    /// loaded capability does not export the symbol, the call short-
    /// circuits to [`LeanMetaResponse::Unsupported`] with a synthetic
    /// host-side diagnostic naming the missing symbol. Otherwise the
    /// session constructs a per-call typed [`LeanExported`] handle
    /// over the meta service's `(Environment, Req, UInt64, USize,
    /// UInt8) -> IO (MetaResponse Resp)` signature and dispatches.
    ///
    /// The outer [`LeanResult`] surfaces host-stack failures (a Lean
    /// `IO`-level exception from the shim itself, or an undecodable
    /// return value). The four-way classification — `Ok` / `Failed` /
    /// `TimeoutOrHeartbeat` / `Unsupported` — lives in the inner
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
        Req: lean_rs::abi::traits::LeanAbi<'lean>,
        Resp: TryFromLean<'lean>,
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
        let Some(address) = self.capabilities.symbols().meta_address_by_name(service.name()) else {
            let message = format!(
                "meta service '{}' is not exported by the loaded capability",
                service.name()
            );
            return Ok(LeanMetaResponse::Unsupported(LeanElabFailure::synthetic(
                message,
                "<host>".to_owned(),
            )));
        };
        // SAFETY: per the SessionSymbols::resolve invariant — the
        // address (when present) resolves a Lake-emitted function
        // whose signature is pinned in the capability contract table
        // above: `(Environment, Req, UInt64, USize, UInt8) -> IO
        // (MetaResponse Resp)`. `Req: LeanAbi<'lean>` and `Resp:
        // TryFromLean<'lean>` line up with the per-arg `CRepr` and the
        // `LeanIo` decoder.
        let call: LeanExported<'lean, '_, (Obj<'lean>, Req, u64, usize, u8), LeanIo<LeanMetaResponse<Resp>>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = call.call(
            self.environment.clone(),
            request,
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
            options.transparency_byte(),
        );
        self.record_call(0, t.elapsed());
        result
    }

    /// Look up many declarations in one Lean traversal.
    ///
    /// Equivalent to calling [`Self::query_declaration`] in a loop over
    /// `names`, except that the entire batch crosses the FFI boundary
    /// exactly once: one `Array Name` allocation in, one
    /// `Array (Option Declaration)` allocation out. The Lean shim folds
    /// the singular `envQueryDeclaration` across the input array, so the
    /// iteration semantics are identical to a Rust-side fold over the
    /// singular path — a missing name still errors the batch.
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
            let (handle, trampoline) = bridge.abi_parts();
            let address = self.capabilities.symbols().env_query_declarations_bulk_progress;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array Name, USize, USize) ->
            // IO (Except UInt8 (Array (Option Declaration)))`.
            let call: LeanExported<'lean, '_, (Obj<'lean>, Vec<LeanName<'lean>>, usize, usize), LeanIo<Obj<'lean>>> =
                unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = call.call(self.environment.clone(), name_handles, handle, trampoline);
            let batch_len = u64::try_from(names.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            bridge.decode::<Vec<Option<LeanDeclaration<'lean>>>>(result?)?
        } else {
            let address = self.capabilities.symbols().env_query_declarations_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array Name) -> IO (Array (Option Declaration))`.
            let call: LeanExported<
                'lean,
                '_,
                (Obj<'lean>, Vec<LeanName<'lean>>),
                LeanIo<Vec<Option<LeanDeclaration<'lean>>>>,
            > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = call.call(self.environment.clone(), name_handles);
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
        sources: &[&str],
        options: &LeanElabOptions,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.elaborate_bulk",
            batch_size = sources.len(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        check_cancellation(cancellation)?;
        if cancellation.is_some() {
            let started = Instant::now();
            let total = Some(u64::try_from(sources.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(sources.len());
            for (idx, source) in sources.iter().enumerate() {
                check_cancellation(cancellation)?;
                out.push(self.elaborate(source, None, options, cancellation)?);
                report_progress(
                    progress,
                    "elaborate_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                    started,
                )?;
            }
            return Ok(out);
        }
        let sources_owned: Vec<String> = sources.iter().map(|&s| s.to_owned()).collect();
        if let Some(sink) = progress {
            let total = Some(u64::try_from(sources.len()).unwrap_or(u64::MAX));
            let bridge = ProgressBridge::new(sink, "elaborate_bulk", total)?;
            let (handle, trampoline) = bridge.abi_parts();
            let address = self.capabilities.symbols().elaborate_bulk_progress;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String, String, String, UInt64,
            // USize, USize, USize) -> IO (Except UInt8 (Array (Except
            // ElabFailure Expr)))`.
            let call: LeanExported<
                'lean,
                '_,
                (Obj<'lean>, Vec<String>, &str, &str, u64, usize, usize, usize),
                LeanIo<Obj<'lean>>,
            > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = call.call(
                self.environment.clone(),
                sources_owned,
                options.namespace_context_str(),
                options.file_label_str(),
                options.heartbeats(),
                options.diagnostic_byte_limit_usize(),
                handle,
                trampoline,
            );
            let batch_len = u64::try_from(sources.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            bridge.decode(result?)
        } else {
            let address = self.capabilities.symbols().elaborate_bulk;
            // SAFETY: per the SessionSymbols::resolve invariant; signature
            // is `(Environment, Array String, String, String, UInt64, USize)
            // -> IO (Array (Except ElabFailure Expr))`.
            let call: LeanExported<
                'lean,
                '_,
                (Obj<'lean>, Vec<String>, &str, &str, u64, usize),
                LeanIo<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>>,
            > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
            let t = Instant::now();
            let result = call.call(
                self.environment.clone(),
                sources_owned,
                options.namespace_context_str(),
                options.file_label_str(),
                options.heartbeats(),
                options.diagnostic_byte_limit_usize(),
            );
            let batch_len = u64::try_from(sources.len()).unwrap_or(u64::MAX);
            self.record_call(batch_len, t.elapsed());
            result
        }
    }

    /// Look up and invoke a capability-exported function by name with a
    /// typed argument tuple and a typed result decoder.
    ///
    /// This is the transport-neutral escape hatch for capability dylibs
    /// that export Lean functions beyond the twenty-eight session-fixed
    /// symbols. The conversion bounds — [`LeanArgs`] on the argument
    /// tuple and [`DecodeCallResult`] on the result — are the same
    /// bounds [`lean_rs::module::LeanModule::exported`] uses, so an
    /// IO-returning Lean capability is invoked with `R = LeanIo<T>`
    /// (fused `decode_io` + `T::try_from_lean`) and a pure capability
    /// with `R = T` for `T: LeanAbi`. The sealed traits stay invisible
    /// at the call site; the bound is satisfied automatically.
    ///
    /// Function-only: nullary-constant globals are not capabilities.
    /// Reach a Lean nullary-constant global directly through
    /// [`lean_rs::module::LeanModule::exported`] if you need one. The
    /// symbol address is resolved on every call (one `dlsym` per
    /// invocation); for hot capabilities, prefer pre-resolving via
    /// `LeanModule::exported` and caching the [`lean_rs::module::LeanExported`]
    /// handle.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage [`HostStage::Link`] if
    /// `name` does not resolve as a function symbol in the capability
    /// dylib. Returns [`lean_rs::LeanError::LeanException`] when the underlying
    /// Lean export raises through `IO` (only possible when
    /// `R = LeanIo<_>`). Returns [`lean_rs::LeanError::Host`] with stage
    /// [`HostStage::Conversion`] when the return value does not decode
    /// into the declared `R::Output`.
    pub fn call_capability<Args, R>(
        &mut self,
        name: &str,
        args: Args,
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<R::Output>
    where
        Args: LeanArgs<'lean>,
        R: DecodeCallResult<'lean>,
    {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.call_capability",
            symbol = name,
            arity = Args::ARITY,
        )
        .entered();
        check_cancellation(cancellation)?;
        let Some(library) = self.capabilities.user_library() else {
            return Err(lean_rs::__host_internals::host_unsupported(format!(
                "call_capability('{name}') requires a user capability dylib; this LeanCapabilities was loaded with load_shims_only",
            )));
        };
        let address = library.resolve_function_symbol(name)?;
        check_cancellation(cancellation)?;
        // SAFETY: `resolve_function_symbol` resolved an address inside
        // the capability's `LeanLibrary<'lean>` (the dylib outlives the
        // session via the `'c` borrow). `Args: LeanArgs<'lean>` and
        // `R: DecodeCallResult<'lean>` line up with Lake's emitted C
        // ABI for the named symbol. The caller is responsible for
        // matching the Lean export's actual signature.
        let call: LeanExported<'lean, '_, Args, R> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = Args::invoke(&call, args);
        self.record_call(0, t.elapsed());
        result
    }

    fn runtime(&self) -> &'lean lean_rs::LeanRuntime {
        self.capabilities.host().runtime()
    }

    /// Build a `LeanName` from a dotted Rust string via the capability's
    /// `Name.toName` shim.
    fn make_name(&self, name: &str, cancellation: Option<&LeanCancellationToken>) -> LeanResult<LeanName<'lean>> {
        check_cancellation(cancellation)?;
        let address = self.capabilities.symbols().name_from_string;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `String -> Name` (pure, not IO).
        let to_name: LeanExported<'lean, '_, (&str,), LeanName<'lean>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let t = Instant::now();
        let result = to_name.call(name);
        self.record_call(0, t.elapsed());
        result
    }
}
