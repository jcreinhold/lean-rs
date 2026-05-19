//! `LeanCapabilities` — a loaded capability dylib (plus the shim dylib
//! it depends on) with its session symbol addresses pre-resolved.
//!
//! [`LeanCapabilities`] owns two [`lean_rs::module::LeanLibrary`]
//! handles and caches the session symbol addresses that
//! [`crate::host::LeanSession`] dispatches through:
//!
//! - The **user's capability dylib** is the artefact the consumer
//!   built with `lake build` and named in
//!   [`crate::host::LeanHost::load_capabilities`]. It contains the
//!   user's own `@[export]` symbols ([`crate::LeanSession::call_capability`]
//!   dispatches here).
//! - The **shim dylib** is `liblean__rs__host__shims_LeanRsHostShims.dylib`,
//!   built by the consumer's `lake build` from the required
//!   `lean-rs-host-shims` Lake package. It contains the 18 mandatory +
//!   4 optional `lean_rs_host_*` `@[export]` symbols that every typed
//!   `LeanSession` method dispatches through. Lake does *not*
//!   transitively bundle the shim's `@[export]` symbols into the
//!   user's dylib (verified at carve-out time, 2026-05-18 —
//!   `LeanLib.sharedFacet` is a per-package shared library, not a
//!   transitive merge), so the host stack loads both dylibs and routes
//!   each call to the right one. Both dylibs share one Lean runtime;
//!   each per-module `initialize_<Module>` short-circuits idempotently
//!   on its own flag.
//!
//! A missing mandatory shim symbol fails capability load; a missing
//! meta-service symbol degrades to a synthesised
//! [`crate::host::meta::LeanMetaResponse::Unsupported`] at the
//! [`crate::LeanSession::run_meta`] call site. Pre-resolution at
//! construction means each later query is one struct-field read and
//! one FFI call — no per-query `dlsym`.
//!
//! Construction goes through [`crate::host::LeanHost::load_capabilities`];
//! [`LeanCapabilities::session`] then imports a module list and returns
//! the long-lived [`crate::host::LeanSession`] handle.

use core::fmt;

use crate::host::cancellation::LeanCancellationToken;
use crate::host::host::LeanHost;
use crate::host::session::{LeanSession, SessionSymbols};
use lean_rs::error::LeanResult;
use lean_rs::module::LeanLibrary;

/// A loaded capability dylib (and its shim dependency) with session
/// symbol addresses pre-resolved.
///
/// Owns both [`LeanLibrary`] handles so callers do not have to track
/// the dylibs' lifetimes separately. Borrows from the parent
/// [`LeanHost`] for the runtime + project context. Neither [`Send`] nor
/// [`Sync`]: inherited from the contained `LeanLibrary` handles.
pub struct LeanCapabilities<'lean, 'h> {
    host: &'h LeanHost<'lean>,
    /// User's capability dylib — the one named in `load_capabilities`.
    /// `pub(crate)` accessor below exposes it to
    /// [`crate::LeanSession::call_capability`] for ad-hoc dispatch on
    /// user-authored `@[export]` symbols.
    user_library: LeanLibrary<'lean>,
    /// Shim dylib carrying the 18+4 `lean_rs_host_*` `@[export]`
    /// symbols. RAII anchor only — the addresses inside `symbols`
    /// outlive any direct read of this field.
    #[allow(dead_code, reason = "Drop releases the dylib; field is structurally required")]
    shim_library: LeanLibrary<'lean>,
    symbols: SessionSymbols,
}

impl fmt::Debug for LeanCapabilities<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeanCapabilities").finish_non_exhaustive()
    }
}

impl<'lean, 'h> LeanCapabilities<'lean, 'h> {
    /// Build a [`LeanCapabilities`] from an opened user-capability
    /// library, opening the shim dylib alongside it.
    ///
    /// Initializes the root module of the user's dylib (idempotent
    /// through Lean's `_G_initialized` short-circuit), opens the shim
    /// dylib located via the Lake manifest, initializes the
    /// `LeanRsHostShims` root module, and resolves the
    /// session-dispatch symbol addresses from the **shim** dylib: 18
    /// mandatory baseline symbols (load failure on miss) and 4
    /// optional meta-service symbols (missing entries stored as
    /// `None`). Both [`lean_rs::module::LeanModule`] handles are
    /// dropped at the end of this call — the cached symbol addresses
    /// outlive any single module borrow.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage
    /// [`lean_rs::HostStage::Load`] /
    /// [`lean_rs::LeanDiagnosticCode::ModuleInit`] if the shim dylib
    /// cannot be located (missing `lake-manifest.json`, missing
    /// `lean_rs_host_shims` entry, missing built dylib — the consumer
    /// likely forgot `require lean_rs_host_shims` or didn't run
    /// `lake build`). Returns [`lean_rs::HostStage::Link`] if the
    /// initializer or any of the 18 **mandatory** symbols is missing
    /// from the shim dylib. Missing optional meta-service symbols
    /// never fail capability load.
    pub(crate) fn new(
        host: &'h LeanHost<'lean>,
        user_library: LeanLibrary<'lean>,
        package: &str,
        lib_name: &str,
    ) -> LeanResult<Self> {
        // Load order matters. The user's compiled dylib has
        // `_initialize_lean__rs__host__shims_LeanRsHostShims` as an
        // *undefined* symbol — the user's initializer chain calls it
        // transitively because their lakefile `require`s the shim
        // package. The dynamic linker can only resolve that reference
        // if the shim dylib was loaded first with `RTLD_GLOBAL`
        // (default `RTLD_LOCAL` keeps the shim's symbols invisible to
        // subsequently loaded dylibs and the user's init chain
        // SIGSEGVs jumping to an unresolved symbol). Verified at
        // bring-up: `nm` showed `U _initialize_..._LeanRsHostShims`
        // in the consumer dylib.
        //
        // So: open the shim dylib globally first, then the user dylib
        // normally; initializing the user module then drives the
        // shim's initializer through the resolved global symbol.
        let shim_dylib_path = host.project().shim_dylib()?;
        let shim_library = LeanLibrary::open_globally(host.runtime(), &shim_dylib_path)?;
        // Explicitly initialize the shim's root module too, so the
        // shim's @[export] functions are live regardless of whether
        // the user's chain reaches them transitively.
        let _shim_module =
            shim_library.initialize_module(crate::host::lake::SHIM_PACKAGE_NAME, crate::host::lake::SHIM_LIB_NAME)?;

        // Now the user dylib: its initializer can resolve the
        // shim-defined transitives through the global namespace.
        let _user_module = user_library.initialize_module(package, lib_name)?;

        // The 18 mandatory + 4 optional `lean_rs_host_*` symbols live
        // in the shim dylib; resolve them there.
        // `LeanSession::call_capability` (separately) routes ad-hoc
        // user-authored `@[export]` symbols through `user_library`.
        let symbols = SessionSymbols::resolve(&shim_library)?;
        Ok(Self {
            host,
            user_library,
            shim_library,
            symbols,
        })
    }

    /// Import the named modules into a fresh Lean environment and
    /// return a session over the result.
    ///
    /// Imports happen exactly once per `session()` call. The returned
    /// [`LeanSession`] owns the imported environment and reuses this
    /// capability's cached symbol addresses for every query.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before import dispatch.
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side
    /// import raises (missing `.olean`, malformed module name, …),
    /// with the bounded message Lean surfaced.
    pub fn session<'c>(
        &'c self,
        imports: &[&str],
        cancellation: Option<&LeanCancellationToken>,
    ) -> LeanResult<LeanSession<'lean, 'c>> {
        LeanSession::import(self, imports, cancellation)
    }

    /// The capability's parent host (for runtime + project access by
    /// the session dispatch).
    pub(crate) fn host(&self) -> &'h LeanHost<'lean> {
        self.host
    }

    /// The pre-resolved session symbol addresses.
    pub(crate) fn symbols(&self) -> &SessionSymbols {
        &self.symbols
    }

    /// The user's owned capability [`LeanLibrary`].
    ///
    /// `pub(crate)` so [`crate::LeanSession::call_capability`] can
    /// resolve ad-hoc function symbols on the user's dylib without
    /// holding a separate library borrow. Ad-hoc calls always go to
    /// the user's dylib, not the shim dylib: the shim dylib hosts a
    /// fixed contract (the 18+4 `lean_rs_host_*` symbols pre-resolved
    /// in `symbols`); arbitrary user `@[export]` symbols live in the
    /// user's dylib.
    pub(crate) fn library(&self) -> &LeanLibrary<'lean> {
        &self.user_library
    }
}
