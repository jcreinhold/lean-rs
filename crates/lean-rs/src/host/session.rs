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
//! Every Lean capability dylib that [`crate::host::LeanCapabilities`]
//! loads must export thirteen **mandatory** `@[export]` symbols and may
//! export three **optional** meta-service symbols (matched at
//! `LeanCapabilities::load_capabilities` time):
//!
//! | C symbol                                       | Mandatory? | Lean signature                                                                                                |
//! | ---------------------------------------------- | ---------- | ------------------------------------------------------------------------------------------------------------- |
//! | `lean_rs_host_session_import`                  | yes        | `String -> Array String -> IO Environment`                                                                    |
//! | `lean_rs_host_name_from_string`                | yes        | `String -> Name`                                                                                              |
//! | `lean_rs_host_env_query_declaration`           | yes        | `Environment -> Name -> IO (Option Declaration)`                                                              |
//! | `lean_rs_host_env_query_declarations_bulk`     | yes        | `Environment -> Array Name -> IO (Array (Option Declaration))`                                                |
//! | `lean_rs_host_env_list_declarations`           | yes        | `Environment -> IO (Array Name)`                                                                              |
//! | `lean_rs_host_env_declaration_type`            | yes        | `Environment -> Name -> IO (Option Expr)`                                                                     |
//! | `lean_rs_host_env_declaration_kind`            | yes        | `Environment -> Name -> IO String`                                                                            |
//! | `lean_rs_host_env_declaration_name`            | yes        | `Environment -> Name -> IO String`                                                                            |
//! | `lean_rs_host_elaborate`                       | yes        | `Environment -> String -> Option Expr -> String -> String -> UInt64 -> USize -> IO (Except ElabFailure Expr)` |
//! | `lean_rs_host_elaborate_bulk`                  | yes        | `Environment -> Array String -> String -> String -> UInt64 -> USize -> IO (Array (Except ElabFailure Expr))`  |
//! | `lean_rs_host_kernel_check`                    | yes        | `Environment -> String -> String -> String -> UInt64 -> USize -> IO KernelOutcome`                            |
//! | `lean_rs_host_check_evidence`                  | yes        | `Environment -> Evidence -> IO EvidenceStatus`                                                                |
//! | `lean_rs_host_evidence_summary`                | yes        | `Environment -> Evidence -> IO ProofSummary`                                                                  |
//! | `lean_rs_host_meta_infer_type`                 | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                   |
//! | `lean_rs_host_meta_whnf`                       | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                   |
//! | `lean_rs_host_meta_heartbeat_burn`             | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)`                                   |
//!
//! Missing **mandatory** symbols surface at `load_capabilities` as
//! [`crate::HostStage::Link`] — failures bind to the capability's load,
//! not to the first query. Missing **optional** meta-service symbols
//! degrade gracefully: [`LeanSession::run_meta`] returns
//! [`crate::host::meta::LeanMetaResponse::Unsupported`] against a service whose
//! address did not resolve, the rest of the capability stays usable.
//! The evidence-side pair (`check_evidence`, `evidence_summary`) is
//! mandatory because any capability that produces a `LeanEvidence`
//! handle via `kernel_check` must also be able to re-validate and
//! summarize it: the missing-symbol case defines no recoverable
//! caller behaviour, so the error is folded into capability load
//! rather than into every call site.
//!
//! The baseline (prompts 13–14) is the first seven symbols; prompt 15
//! adds the `elaborate` / `kernel_check` pair; prompt 16 adds the three
//! optional meta-service symbols; prompt 17 adds the `check_evidence` /
//! `evidence_summary` pair; prompt 20 adds the
//! `env_query_declarations_bulk` and `elaborate_bulk` pair to amortise
//! per-item FFI overhead across a single Lean traversal. Future prompts
//! extend additively.
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
// `run_meta` is `pub` but bounded on `crate::abi::traits::{LeanAbi, TryFromLean}`.
// `LeanAbi` is sealed-public; `TryFromLean` is `pub(crate)`. The bound is a
// crate-internal compatibility requirement, not a downstream extension point
// (the meta-service registry is closed by `host::meta::service`). Same
// precedent as `module::exported.rs`.
#![allow(private_bounds, private_interfaces)]

use core::cell::Cell;
use core::ffi::c_void;
use std::time::Instant;

use crate::abi::traits::TryFromLean;
#[cfg(doc)]
use crate::error::HostStage;
use crate::error::{LeanError, LeanResult};
use crate::host::capabilities::LeanCapabilities;
use crate::host::elaboration::{LeanElabFailure, LeanElabOptions};
use crate::host::evidence::{EvidenceStatus, LeanEvidence, LeanKernelOutcome, ProofSummary};
use crate::host::meta::{LeanMetaOptions, LeanMetaResponse, LeanMetaService};
use crate::module::{DecodeCallResult, LeanArgs, LeanExported, LeanIo, LeanLibrary};
use crate::runtime::obj::Obj;
use crate::{LeanDeclaration, LeanExpr, LeanName};

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
    pub(crate) name_from_string: *mut c_void,
    pub(crate) env_query_declaration: *mut c_void,
    pub(crate) env_query_declarations_bulk: *mut c_void,
    pub(crate) env_list_declarations: *mut c_void,
    pub(crate) env_declaration_type: *mut c_void,
    pub(crate) env_declaration_kind: *mut c_void,
    pub(crate) env_declaration_name: *mut c_void,
    pub(crate) elaborate: *mut c_void,
    pub(crate) elaborate_bulk: *mut c_void,
    pub(crate) kernel_check: *mut c_void,
    pub(crate) check_evidence: *mut c_void,
    pub(crate) evidence_summary: *mut c_void,
    pub(crate) meta_infer_type: Option<*mut c_void>,
    pub(crate) meta_whnf: Option<*mut c_void>,
    pub(crate) meta_heartbeat_burn: Option<*mut c_void>,
}

impl SessionSymbols {
    /// Resolve session function symbols from `library`. The eleven
    /// baseline symbols are mandatory; the three meta-service symbols
    /// are optional.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Link`] on
    /// the first mandatory symbol that fails to resolve; the
    /// diagnostic embeds the missing symbol name and the library path
    /// (via [`LeanLibrary::resolve_function_symbol`]). Missing
    /// **optional** meta-service symbols never fail capability load —
    /// the corresponding field is `None` and the `run_meta` dispatch
    /// site synthesises an `Unsupported` response.
    pub(crate) fn resolve(library: &LeanLibrary<'_>) -> LeanResult<Self> {
        Ok(Self {
            session_import: library.resolve_function_symbol("lean_rs_host_session_import")?,
            name_from_string: library.resolve_function_symbol("lean_rs_host_name_from_string")?,
            env_query_declaration: library.resolve_function_symbol("lean_rs_host_env_query_declaration")?,
            env_query_declarations_bulk: library.resolve_function_symbol("lean_rs_host_env_query_declarations_bulk")?,
            env_list_declarations: library.resolve_function_symbol("lean_rs_host_env_list_declarations")?,
            env_declaration_type: library.resolve_function_symbol("lean_rs_host_env_declaration_type")?,
            env_declaration_kind: library.resolve_function_symbol("lean_rs_host_env_declaration_kind")?,
            env_declaration_name: library.resolve_function_symbol("lean_rs_host_env_declaration_name")?,
            elaborate: library.resolve_function_symbol("lean_rs_host_elaborate")?,
            elaborate_bulk: library.resolve_function_symbol("lean_rs_host_elaborate_bulk")?,
            kernel_check: library.resolve_function_symbol("lean_rs_host_kernel_check")?,
            check_evidence: library.resolve_function_symbol("lean_rs_host_check_evidence")?,
            evidence_summary: library.resolve_function_symbol("lean_rs_host_evidence_summary")?,
            meta_infer_type: library.resolve_optional_function_symbol("lean_rs_host_meta_infer_type"),
            meta_whnf: library.resolve_optional_function_symbol("lean_rs_host_meta_whnf"),
            meta_heartbeat_burn: library.resolve_optional_function_symbol("lean_rs_host_meta_heartbeat_burn"),
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

impl<'lean, 'c> LeanSession<'lean, 'c> {
    /// Import the named modules into a fresh Lean environment and wrap
    /// it as a session.
    ///
    /// The Lean-side `lean_rs_host_session_import` receives the Lake
    /// project root (so it can `Lean.initSearchPath` the `.olean`
    /// directory) and the module-name list, and returns the resulting
    /// environment. Failures surface as
    /// [`LeanError::LeanException`] with the message Lean produced.
    pub(crate) fn import(capabilities: &'c LeanCapabilities<'lean, 'c>, imports: &[&str]) -> LeanResult<Self> {
        let _span = tracing::info_span!(
            target: "lean_rs",
            "lean_rs.host.session.import",
            imports_len = imports.len(),
        )
        .entered();
        let runtime = capabilities.host().runtime();
        let address = capabilities.symbols().session_import;
        // SAFETY: `address` was resolved by `SessionSymbols::resolve`
        // against `capabilities.library()`, which outlives `'c`. The
        // signature `(String, Vec<String>) -> IO Environment` matches
        // the Lean-side `lean_rs_host_session_import`.
        let import_fn: LeanExported<'lean, '_, (String, Vec<String>), LeanIo<Obj<'lean>>> =
            unsafe { LeanExported::from_function_address(runtime, address) };
        let search_path = capabilities
            .host()
            .project()
            .olean_search_path()
            .to_string_lossy()
            .into_owned();
        let imports_owned: Vec<String> = imports.iter().map(|&s| s.to_owned()).collect();
        let environment = import_fn.call(search_path, imports_owned)?;
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

    /// Look up a declaration by full Lean name (e.g. `"Nat.zero"`).
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Conversion`]
    /// if the name is not present in the imported environment. Returns
    /// [`LeanError::LeanException`] if the Lean-side query raises.
    pub fn query_declaration(&mut self, name: &str) -> LeanResult<LeanDeclaration<'lean>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.query_declaration",
            name = name,
        )
        .entered();
        let name_handle = self.make_name(name)?;
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
            None => Err(LeanError::abi_conversion(format!(
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
    /// Returns [`LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn list_declarations(&mut self) -> LeanResult<Vec<LeanName<'lean>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.list_declarations",
        )
        .entered();
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

    /// The declared type of `name`, as an opaque [`LeanExpr`] handle.
    ///
    /// Returns `Ok(None)` if the name is not present in the environment.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn declaration_type(&mut self, name: &str) -> LeanResult<Option<LeanExpr<'lean>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_type",
            name = name,
        )
        .entered();
        let name_handle = self.make_name(name)?;
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

    /// The kind of `name` as a Lean-rendered string
    /// (`"axiom"`, `"definition"`, `"theorem"`, `"opaque"`, `"quot"`,
    /// `"inductive"`, `"constructor"`, `"recursor"`), or `"missing"`
    /// if `name` is not in the environment.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn declaration_kind(&mut self, name: &str) -> LeanResult<String> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_kind",
            name = name,
        )
        .entered();
        let name_handle = self.make_name(name)?;
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
    /// Returns [`LeanError::LeanException`] if the Lean-side query
    /// raises.
    pub fn declaration_name(&mut self, name: &str) -> LeanResult<String> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.declaration_name",
            name = name,
        )
        .entered();
        let name_handle = self.make_name(name)?;
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
    /// Returns [`LeanError::LeanException`] if the Lean-side shim raises
    /// through `IO`. Returns [`LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the Lean return value does not
    /// decode into [`LeanElabFailure`] / [`LeanExpr`].
    pub fn elaborate(
        &mut self,
        source: &str,
        expected_type: Option<&LeanExpr<'lean>>,
        options: &LeanElabOptions,
    ) -> LeanResult<Result<LeanExpr<'lean>, LeanElabFailure>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.elaborate",
            source_len = source.len(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
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
    /// Returns [`LeanError::LeanException`] if the Lean-side shim
    /// raises through `IO` (an unexpected internal failure that is not
    /// itself a rejection / unavailable diagnostic). Returns
    /// [`LeanError::Host`] with stage [`HostStage::Conversion`] if the
    /// Lean return value does not decode into [`LeanKernelOutcome`].
    pub fn kernel_check(&mut self, source: &str, options: &LeanElabOptions) -> LeanResult<LeanKernelOutcome<'lean>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.kernel_check",
            source_len = source.len(),
            heartbeats = options.heartbeats(),
            diagnostic_byte_limit = options.diagnostic_byte_limit_usize(),
        )
        .entered();
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
    /// Returns [`LeanError::LeanException`] if the Lean shim raises
    /// through `IO` outside of its own `try` (an unexpected internal
    /// failure that the shim did not classify). Returns
    /// [`LeanError::Host`] with stage [`HostStage::Conversion`] if the
    /// return value does not decode as a four-tag
    /// [`EvidenceStatus`] inductive.
    pub fn check_evidence(&mut self, handle: &LeanEvidence<'lean>) -> LeanResult<EvidenceStatus> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.check_evidence",
        )
        .entered();
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
    /// Returns [`LeanError::LeanException`] if the Lean shim raises
    /// through `IO`. Returns [`LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the return value does not decode
    /// as a three-field [`ProofSummary`] structure.
    pub fn summarize_evidence(&mut self, handle: &LeanEvidence<'lean>) -> LeanResult<ProofSummary> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.summarize_evidence",
        )
        .entered();
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
    /// Returns [`LeanError::LeanException`] if the Lean shim raises
    /// through `IO`. Returns [`LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the return value does not decode
    /// into [`LeanMetaResponse<Resp>`].
    pub fn run_meta<Req, Resp>(
        &mut self,
        service: &LeanMetaService<Req, Resp>,
        request: Req,
        options: &LeanMetaOptions,
    ) -> LeanResult<LeanMetaResponse<Resp>>
    where
        Req: crate::abi::traits::LeanAbi<'lean>,
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
    /// `name_from_string` shim, one [`crate::LeanName`] handle per
    /// input. The metric impact is `names.len() + 1` recorded FFI calls
    /// for a batch of `names.len()` items, versus `2 * names.len()` for
    /// the same workload through [`Self::query_declaration`].
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Conversion`]
    /// on the first name that is not present in the imported
    /// environment, with the missing name in the diagnostic. Returns
    /// [`LeanError::LeanException`] if the Lean-side bulk shim raises
    /// through `IO`.
    pub fn query_declarations_bulk(&mut self, names: &[&str]) -> LeanResult<Vec<LeanDeclaration<'lean>>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.session.query_declarations_bulk",
            batch_size = names.len(),
        )
        .entered();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let name_handles: Vec<LeanName<'lean>> = names.iter().map(|n| self.make_name(n)).collect::<LeanResult<_>>()?;
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
        let raw = result?;
        let mut out: Vec<LeanDeclaration<'lean>> = Vec::with_capacity(raw.len());
        for (slot, name) in raw.into_iter().zip(names.iter()) {
            match slot {
                Some(decl) => out.push(decl),
                None => {
                    return Err(LeanError::abi_conversion(format!(
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
    /// Returns [`LeanError::LeanException`] if the Lean-side bulk shim
    /// raises through `IO`. Returns [`LeanError::Host`] with stage
    /// [`HostStage::Conversion`] if the Lean return value does not
    /// decode into a `Vec<Result<LeanExpr, LeanElabFailure>>`.
    pub fn elaborate_bulk(
        &mut self,
        sources: &[&str],
        options: &LeanElabOptions,
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
        let sources_owned: Vec<String> = sources.iter().map(|&s| s.to_owned()).collect();
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

    /// Look up and invoke a capability-exported function by name with a
    /// typed argument tuple and a typed result decoder.
    ///
    /// This is the transport-neutral escape hatch for capability dylibs
    /// that export Lean functions beyond the thirteen session-fixed
    /// symbols. The conversion bounds — [`LeanArgs`] on the argument
    /// tuple and [`DecodeCallResult`] on the result — are the same
    /// bounds [`crate::module::LeanModule::exported`] uses, so an
    /// IO-returning Lean capability is invoked with `R = LeanIo<T>`
    /// (fused `decode_io` + `T::try_from_lean`) and a pure capability
    /// with `R = T` for `T: LeanAbi`. The sealed traits stay invisible
    /// at the call site; the bound is satisfied automatically.
    ///
    /// Function-only: nullary-constant globals are not capabilities.
    /// Reach a Lean nullary-constant global directly through
    /// [`crate::module::LeanModule::exported`] if you need one. The
    /// symbol address is resolved on every call (one `dlsym` per
    /// invocation); for hot capabilities, prefer pre-resolving via
    /// `LeanModule::exported` and caching the [`crate::module::LeanExported`]
    /// handle.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Link`] if
    /// `name` does not resolve as a function symbol in the capability
    /// dylib. Returns [`LeanError::LeanException`] when the underlying
    /// Lean export raises through `IO` (only possible when
    /// `R = LeanIo<_>`). Returns [`LeanError::Host`] with stage
    /// [`HostStage::Conversion`] when the return value does not decode
    /// into the declared `R::Output`.
    pub fn call_capability<Args, R>(&mut self, name: &str, args: Args) -> LeanResult<R::Output>
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
        let address = self.capabilities.library().resolve_function_symbol(name)?;
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

    fn runtime(&self) -> &'lean crate::runtime::LeanRuntime {
        self.capabilities.host().runtime()
    }

    /// Build a `LeanName` from a dotted Rust string via the capability's
    /// `Name.toName` shim.
    fn make_name(&self, name: &str) -> LeanResult<LeanName<'lean>> {
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
