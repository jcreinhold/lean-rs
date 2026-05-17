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
//! loads must export eleven **mandatory** `@[export]` symbols and may
//! export three **optional** meta-service symbols (matched at
//! `LeanCapabilities::load_capabilities` time):
//!
//! | C symbol                                  | Mandatory? | Lean signature                                             |
//! | ----------------------------------------- | ---------- | ---------------------------------------------------------- |
//! | `lean_rs_host_session_import`             | yes        | `String -> Array String -> IO Environment`                 |
//! | `lean_rs_host_name_from_string`           | yes        | `String -> Name`                                           |
//! | `lean_rs_host_env_query_declaration`      | yes        | `Environment -> Name -> IO (Option Declaration)`           |
//! | `lean_rs_host_env_list_declarations`      | yes        | `Environment -> IO (Array Name)`                           |
//! | `lean_rs_host_env_declaration_type`       | yes        | `Environment -> Name -> IO (Option Expr)`                  |
//! | `lean_rs_host_env_declaration_kind`       | yes        | `Environment -> Name -> IO String`                         |
//! | `lean_rs_host_env_declaration_name`       | yes        | `Environment -> Name -> IO String`                         |
//! | `lean_rs_host_elaborate`                  | yes        | `Environment -> String -> Option Expr -> String -> String -> UInt64 -> USize -> IO (Except ElabFailure Expr)` |
//! | `lean_rs_host_kernel_check`               | yes        | `Environment -> String -> String -> String -> UInt64 -> USize -> IO KernelOutcome` |
//! | `lean_rs_host_check_evidence`             | yes        | `Environment -> Evidence -> IO EvidenceStatus`             |
//! | `lean_rs_host_evidence_summary`           | yes        | `Environment -> Evidence -> IO ProofSummary`               |
//! | `lean_rs_host_meta_infer_type`            | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)` |
//! | `lean_rs_host_meta_whnf`                  | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)` |
//! | `lean_rs_host_meta_heartbeat_burn`        | optional   | `Environment -> Expr -> UInt64 -> USize -> UInt8 -> IO (MetaResponse Expr)` |
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
//! `evidence_summary` pair. Future prompts extend additively.
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

use core::ffi::c_void;

use crate::abi::traits::TryFromLean;
use crate::error::{HostStage, LeanError, LeanResult};
use crate::host::capabilities::LeanCapabilities;
use crate::host::elaboration::{LeanElabFailure, LeanElabOptions};
use crate::host::evidence::{EvidenceStatus, LeanEvidence, LeanKernelOutcome, ProofSummary};
use crate::host::meta::{LeanMetaOptions, LeanMetaResponse, LeanMetaService};
use crate::module::{LeanExported, LeanIo, LeanLibrary};
use crate::runtime::obj::Obj;
use crate::{LeanDeclaration, LeanExpr, LeanName};

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
    pub(crate) env_list_declarations: *mut c_void,
    pub(crate) env_declaration_type: *mut c_void,
    pub(crate) env_declaration_kind: *mut c_void,
    pub(crate) env_declaration_name: *mut c_void,
    pub(crate) elaborate: *mut c_void,
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
            env_list_declarations: library.resolve_function_symbol("lean_rs_host_env_list_declarations")?,
            env_declaration_type: library.resolve_function_symbol("lean_rs_host_env_declaration_type")?,
            env_declaration_kind: library.resolve_function_symbol("lean_rs_host_env_declaration_kind")?,
            env_declaration_name: library.resolve_function_symbol("lean_rs_host_env_declaration_name")?,
            elaborate: library.resolve_function_symbol("lean_rs_host_elaborate")?,
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
        })
    }

    /// Look up a declaration by full Lean name (e.g. `"Nat.zero"`).
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Conversion`]
    /// if the name is not present in the imported environment. Returns
    /// [`LeanError::LeanException`] if the Lean-side query raises.
    pub fn query_declaration(&mut self, name: &str) -> LeanResult<LeanDeclaration<'lean>> {
        let name_handle = self.make_name(name)?;
        let address = self.capabilities.symbols().env_query_declaration;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO (Option Declaration)`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<Option<LeanDeclaration<'lean>>>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        match query.call(self.environment.clone(), name_handle)? {
            Some(decl) => Ok(decl),
            None => Err(LeanError::host(
                HostStage::Conversion,
                format!("declaration '{name}' not found in imported environment"),
            )),
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
        let address = self.capabilities.symbols().env_list_declarations;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `Environment -> IO (Array Name)`.
        let list: LeanExported<'lean, '_, (Obj<'lean>,), LeanIo<Vec<Obj<'lean>>>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        let raw = list.call(self.environment.clone())?;
        raw.into_iter().map(LeanName::try_from_lean).collect()
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
        let name_handle = self.make_name(name)?;
        let address = self.capabilities.symbols().env_declaration_type;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO (Option Expr)`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<Option<LeanExpr<'lean>>>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        query.call(self.environment.clone(), name_handle)
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
        let name_handle = self.make_name(name)?;
        let address = self.capabilities.symbols().env_declaration_kind;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO String`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<String>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        query.call(self.environment.clone(), name_handle)
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
        let name_handle = self.make_name(name)?;
        let address = self.capabilities.symbols().env_declaration_name;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Name) -> IO String`.
        let query: LeanExported<'lean, '_, (Obj<'lean>, LeanName<'lean>), LeanIo<String>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        query.call(self.environment.clone(), name_handle)
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
        let address = self.capabilities.symbols().elaborate;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, String, Option Expr, String, String,
        // UInt64, USize) -> IO (Except ElabFailure Expr)`.
        let call: LeanExported<
            'lean,
            '_,
            (Obj<'lean>, String, Option<LeanExpr<'lean>>, String, String, u64, usize),
            LeanIo<Result<LeanExpr<'lean>, LeanElabFailure>>,
        > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
        call.call(
            self.environment.clone(),
            source.to_owned(),
            expected_type.cloned(),
            options.namespace_context_str().to_owned(),
            options.file_label_str().to_owned(),
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
        )
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
        let address = self.capabilities.symbols().kernel_check;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, String, String, String, UInt64, USize) ->
        // IO KernelOutcome`.
        let call: LeanExported<
            'lean,
            '_,
            (Obj<'lean>, String, String, String, u64, usize),
            LeanIo<LeanKernelOutcome<'lean>>,
        > = unsafe { LeanExported::from_function_address(self.runtime(), address) };
        call.call(
            self.environment.clone(),
            source.to_owned(),
            options.namespace_context_str().to_owned(),
            options.file_label_str().to_owned(),
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
        )
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
        let address = self.capabilities.symbols().check_evidence;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Evidence) -> IO EvidenceStatus`.
        let call: LeanExported<'lean, '_, (Obj<'lean>, LeanEvidence<'lean>), LeanIo<EvidenceStatus>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        call.call(self.environment.clone(), handle.clone())
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
        let address = self.capabilities.symbols().evidence_summary;
        // SAFETY: per the SessionSymbols::resolve invariant; signature
        // is `(Environment, Evidence) -> IO ProofSummary`.
        let call: LeanExported<'lean, '_, (Obj<'lean>, LeanEvidence<'lean>), LeanIo<ProofSummary>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        call.call(self.environment.clone(), handle.clone())
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
        call.call(
            self.environment.clone(),
            request,
            options.heartbeats(),
            options.diagnostic_byte_limit_usize(),
            options.transparency_byte(),
        )
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
        let to_name: LeanExported<'lean, '_, (String,), LeanName<'lean>> =
            unsafe { LeanExported::from_function_address(self.runtime(), address) };
        to_name.call(name.to_owned())
    }
}
