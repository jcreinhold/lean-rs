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
//! loads must export seven `@[export]` symbols with the following
//! signatures (matched at `LeanCapabilities::load_capabilities` time):
//!
//! | C symbol                                  | Lean signature                                             |
//! | ----------------------------------------- | ---------------------------------------------------------- |
//! | `lean_rs_host_session_import`             | `String -> Array String -> IO Environment`                 |
//! | `lean_rs_host_name_from_string`           | `String -> Name`                                           |
//! | `lean_rs_host_env_query_declaration`      | `Environment -> Name -> IO (Option Declaration)`           |
//! | `lean_rs_host_env_list_declarations`      | `Environment -> IO (Array Name)`                           |
//! | `lean_rs_host_env_declaration_type`       | `Environment -> Name -> IO (Option Expr)`                  |
//! | `lean_rs_host_env_declaration_kind`       | `Environment -> Name -> IO String`                         |
//! | `lean_rs_host_env_declaration_name`       | `Environment -> Name -> IO String`                         |
//!
//! Missing symbols surface at `load_capabilities` as
//! [`crate::HostStage::Link`] — failures bind to the capability's load,
//! not to the first query.
//!
//! Later prompts (parse/elaborate, evidence) extend this contract
//! additively; the seven listed here are the baseline.
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

use core::ffi::c_void;

use crate::abi::traits::TryFromLean;
use crate::error::{HostStage, LeanError, LeanResult};
use crate::host::capabilities::LeanCapabilities;
use crate::module::{LeanExported, LeanIo, LeanLibrary};
use crate::runtime::obj::Obj;
use crate::{LeanDeclaration, LeanExpr, LeanName};

// -- SessionSymbols: pre-resolved C-ABI function addresses ---------------

/// The seven function-symbol addresses [`LeanSession`] dispatches
/// through.
///
/// Populated once at [`LeanCapabilities::new`] time; read by every
/// session method without further `dlsym`. Each field is a non-null
/// `*mut c_void` (raw function entry point); the safety obligation that
/// these point at Lake-emitted functions with the expected ABI is
/// discharged by [`resolve`] only resolving symbols whose Lean
/// signatures are pinned in the module docstring above.
pub(crate) struct SessionSymbols {
    pub(crate) session_import: *mut c_void,
    pub(crate) name_from_string: *mut c_void,
    pub(crate) env_query_declaration: *mut c_void,
    pub(crate) env_list_declarations: *mut c_void,
    pub(crate) env_declaration_type: *mut c_void,
    pub(crate) env_declaration_kind: *mut c_void,
    pub(crate) env_declaration_name: *mut c_void,
}

impl SessionSymbols {
    /// Resolve all seven session function symbols from `library`.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Link`] on
    /// the first symbol that fails to resolve; the diagnostic embeds
    /// the missing symbol name and the library path (via
    /// [`LeanLibrary::resolve_function_symbol`]).
    pub(crate) fn resolve(library: &LeanLibrary<'_>) -> LeanResult<Self> {
        Ok(Self {
            session_import: library.resolve_function_symbol("lean_rs_host_session_import")?,
            name_from_string: library.resolve_function_symbol("lean_rs_host_name_from_string")?,
            env_query_declaration: library.resolve_function_symbol("lean_rs_host_env_query_declaration")?,
            env_list_declarations: library.resolve_function_symbol("lean_rs_host_env_list_declarations")?,
            env_declaration_type: library.resolve_function_symbol("lean_rs_host_env_declaration_type")?,
            env_declaration_kind: library.resolve_function_symbol("lean_rs_host_env_declaration_kind")?,
            env_declaration_name: library.resolve_function_symbol("lean_rs_host_env_declaration_name")?,
        })
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
