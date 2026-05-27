//! `LeanCapabilities` — loaded generic interop, host shim, and optional user
//! dylibs with session symbol addresses pre-resolved.
//!
//! [`LeanCapabilities`] owns the [`lean_rs::module::LeanLibrary`] handles and
//! caches the session symbol addresses that
//! [`crate::host::LeanSession`] dispatches through:
//!
//! - The **user's capability dylib**, when present, is the artefact the
//!   consumer built with `lake build` and named in
//!   [`crate::host::LeanHost::load_capabilities`]. It contains the user's own
//!   `@[export]` symbols ([`crate::LeanSession::call_capability`] dispatches
//!   here).
//! - The **generic interop dylib** is
//!   `liblean__rs__interop__shims_LeanRsInterop.dylib`; it carries
//!   reusable callback helpers imported by the host progress shims.
//! - The **shim dylib** is `liblean__rs__host__shims_LeanRsHostShims.dylib`,
//!   built from the `lean-rs-host` crate's bundled shim sources. It contains
//!   the 28 mandatory + 6 optional `lean_rs_host_*` `@[export]` symbols that
//!   every typed `LeanSession` method dispatches through. Lake does *not*
//!   transitively bundle the shim's `@[export]` symbols into the user's dylib
//!   (verified at carve-out time, 2026-05-18 — `LeanLib.sharedFacet` is a
//!   per-package shared library, not a transitive merge), so the host stack
//!   loads the generic interop dylib, host shim dylib, and user dylib
//!   explicitly. All dylibs share one Lean runtime; each per-module
//!   `initialize_<Module>` short-circuits idempotently on its own flag.
//!
//! A missing mandatory shim symbol fails capability load; a missing
//! meta-service symbol degrades to a synthesised
//! [`crate::host::meta::LeanMetaResponse::Unsupported`] at the
//! [`crate::LeanSession::run_meta`] call site. Pre-resolution at
//! construction means each later query is one struct-field read and
//! one FFI call — no per-query `dlsym`.
//!
//! Construction goes through either [`crate::host::LeanHost::load_capabilities`]
//! or [`crate::host::LeanHost::load_shims_only`]; [`LeanCapabilities::session`]
//! then imports a module list and returns the long-lived
//! [`crate::host::LeanSession`] handle.

use core::fmt;

use lean_rs::error::LeanResult;
use lean_rs::module::LeanLibrary;

use crate::host::cancellation::LeanCancellationToken;
use crate::host::host::LeanHost;
use crate::host::progress::LeanProgressSink;
use crate::host::session::{LeanSession, SessionSymbols};

/// Loaded generic interop, host shim, and optional user dylibs with session
/// symbol addresses pre-resolved.
///
/// Owns the [`LeanLibrary`] handles so callers do not have to track
/// the dylibs' lifetimes separately. Borrows from the parent
/// [`LeanHost`] for the runtime + project context. Neither [`Send`] nor
/// [`Sync`]: inherited from the contained `LeanLibrary` handles.
pub struct LeanCapabilities<'lean, 'h> {
    host: &'h LeanHost<'lean>,
    /// User's capability dylib — the one named in `load_capabilities`, absent
    /// for `load_shims_only`.
    /// `pub(crate)` accessor below exposes it to
    /// [`crate::LeanSession::call_capability`] for ad-hoc dispatch on
    /// user-authored `@[export]` symbols.
    user_library: Option<LeanLibrary<'lean>>,
    /// Generic interop shim dylib carrying reusable callback helpers used by
    /// host progress shims. Loaded globally before the host shim dylib so
    /// generated `LeanRsInterop.*` initializer references resolve.
    #[allow(dead_code, reason = "Drop releases the dylib; field is structurally required")]
    interop_library: LeanLibrary<'lean>,
    /// Shim dylib carrying the 28+6 `lean_rs_host_*` `@[export]`
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
    /// library, opening the generic interop and host shim dylibs alongside it.
    ///
    /// Initializes the root module of the user's dylib (idempotent
    /// through Lean's `_G_initialized` short-circuit), opens and
    /// initializes the generic interop and host shim dylibs built from the
    /// crate-owned shim sources, and resolves the
    /// session-dispatch symbol addresses from the **shim** dylib: 28
    /// mandatory baseline symbols (load failure on miss) and 6
    /// optional symbols — five bounded `MetaM` services plus the
    /// info-tree projection (missing entries stored as `None`). Both
    /// [`lean_rs::module::LeanModule`] handles are
    /// dropped at the end of this call — the cached symbol addresses
    /// outlive any single module borrow.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage
    /// [`lean_rs::HostStage::Load`] /
    /// [`lean_rs::LeanDiagnosticCode::ModuleInit`] if the bundled shim dylibs
    /// cannot be built or located. Returns [`lean_rs::HostStage::Link`] if the
    /// initializer or any of the 28 **mandatory** symbols is missing from the
    /// shim dylib. Missing optional meta-service symbols never fail capability
    /// load.
    pub(crate) fn new(
        host: &'h LeanHost<'lean>,
        user_library: LeanLibrary<'lean>,
        package: &str,
        lib_name: &str,
    ) -> LeanResult<Self> {
        let ShimLibraries {
            interop_library,
            shim_library,
            symbols,
        } = load_shim_libraries(host)?;

        // Now the user dylib. It does not need to depend on the host shims;
        // ad-hoc user exports still resolve from this library.
        let _user_module = user_library.initialize_module(package, lib_name)?;

        Ok(Self {
            host,
            user_library: Some(user_library),
            interop_library,
            shim_library,
            symbols,
        })
    }

    /// Build a [`LeanCapabilities`] backed only by the bundled interop and
    /// host shim dylibs.
    ///
    /// Sessions opened from this value can use every shim-backed session
    /// operation. [`crate::LeanSession::call_capability`] returns
    /// [`lean_rs::LeanDiagnosticCode::Unsupported`] because no user dylib is
    /// attached.
    pub(crate) fn new_shims_only(host: &'h LeanHost<'lean>) -> LeanResult<Self> {
        let ShimLibraries {
            interop_library,
            shim_library,
            symbols,
        } = load_shim_libraries(host)?;
        Ok(Self {
            host,
            user_library: None,
            interop_library,
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
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<LeanSession<'lean, 'c>> {
        LeanSession::import(self, imports, cancellation, progress)
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

    /// The user's owned capability [`LeanLibrary`], if this capability was
    /// loaded with a user dylib.
    ///
    /// `pub(crate)` so [`crate::LeanSession::call_capability`] can
    /// resolve ad-hoc function symbols on the user's dylib without
    /// holding a separate library borrow. Ad-hoc calls always go to
    /// the user's dylib, not the shim dylib: the shim dylib hosts a
    /// fixed contract (the 28+6 `lean_rs_host_*` symbols pre-resolved
    /// in `symbols`); arbitrary user `@[export]` symbols live in the
    /// user's dylib.
    pub(crate) fn user_library(&self) -> Option<&LeanLibrary<'lean>> {
        self.user_library.as_ref()
    }
}

struct ShimLibraries<'lean> {
    interop_library: LeanLibrary<'lean>,
    shim_library: LeanLibrary<'lean>,
    symbols: SessionSymbols,
}

fn load_shim_libraries<'lean>(host: &LeanHost<'lean>) -> LeanResult<ShimLibraries<'lean>> {
    // Load order matters. The host shim imports LeanRsInterop.Callback, so
    // the generic interop dylib must be global before the host shim
    // initializer runs.
    let interop_dylib_path = crate::host::lake::LakeProject::interop_dylib()?;
    let interop_library = LeanLibrary::open_globally(host.runtime(), &interop_dylib_path)?;
    let _interop_module = interop_library.initialize_module(
        crate::host::lake::INTEROP_PACKAGE_NAME,
        crate::host::lake::INTEROP_LIB_NAME,
    )?;

    let shim_dylib_path = crate::host::lake::LakeProject::shim_dylib()?;
    let shim_library = LeanLibrary::open_globally(host.runtime(), &shim_dylib_path)?;
    let _shim_module =
        shim_library.initialize_module(crate::host::lake::SHIM_PACKAGE_NAME, crate::host::lake::SHIM_LIB_NAME)?;

    // The 28 mandatory + 6 optional `lean_rs_host_*` symbols live in the
    // shim dylib; resolve them there in both capability-loading modes.
    let symbols = SessionSymbols::resolve(&shim_library)?;
    Ok(ShimLibraries {
        interop_library,
        shim_library,
        symbols,
    })
}
