//! `LeanCapabilities` — a loaded capability dylib with its session
//! symbol addresses pre-resolved.
//!
//! [`LeanCapabilities`] owns the [`crate::module::LeanLibrary`] and
//! caches the nine function-symbol addresses that
//! [`crate::host::LeanSession`] dispatches through (seven baseline
//! environment-query symbols plus the prompt-15 `elaborate` /
//! `kernel_check` pair). Pre-resolution at construction means each
//! later query is one struct-field read and one FFI call — no per-query
//! `dlsym`.
//!
//! Construction goes through [`crate::host::LeanHost::load_capabilities`];
//! [`LeanCapabilities::session`] then imports a module list and returns
//! the long-lived [`crate::host::LeanSession`] handle.

use core::fmt;

use crate::error::LeanResult;
use crate::host::host::LeanHost;
use crate::host::session::{LeanSession, SessionSymbols};
use crate::module::LeanLibrary;

/// A loaded capability dylib with its session symbol addresses
/// pre-resolved.
///
/// Owns the [`LeanLibrary`] so callers do not have to track the dylib's
/// lifetime separately. Borrows from the parent [`LeanHost`] for the
/// runtime + project context. Neither [`Send`] nor [`Sync`]: inherited
/// from the contained `LeanLibrary`.
pub struct LeanCapabilities<'lean, 'h> {
    host: &'h LeanHost<'lean>,
    /// RAII anchor — the dylib stays mapped as long as this struct lives.
    /// Read indirectly through the cached function addresses in `symbols`,
    /// so it appears unused to the compiler.
    #[allow(dead_code, reason = "Drop releases the dylib; field is structurally required")]
    library: LeanLibrary<'lean>,
    symbols: SessionSymbols,
}

impl fmt::Debug for LeanCapabilities<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeanCapabilities").finish_non_exhaustive()
    }
}

impl<'lean, 'h> LeanCapabilities<'lean, 'h> {
    /// Build a [`LeanCapabilities`] from an opened library.
    ///
    /// Initializes the root module of the dylib (idempotent through
    /// Lean's `_G_initialized` short-circuit) and resolves the nine
    /// session-dispatch symbol addresses from the library. The
    /// initialized [`crate::module::LeanModule`] is dropped at the end
    /// of this call — the cached symbol addresses provide everything
    /// the session needs without re-`dlsym`-ing.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LeanError::Host`] with stage
    /// [`crate::HostStage::Link`] if the initializer or any of the
    /// nine required symbols is missing from `library`.
    pub(crate) fn new(
        host: &'h LeanHost<'lean>,
        library: LeanLibrary<'lean>,
        package: &str,
        lib_name: &str,
    ) -> LeanResult<Self> {
        // Drive Lean's per-module initializer once so the module's
        // constants (and the seven `@[export]` functions we're about to
        // resolve) are live. We don't keep the LeanModule: the symbol
        // addresses we cache below outlive any single LeanModule borrow.
        let _module = library.initialize_module(package, lib_name)?;
        let symbols = SessionSymbols::resolve(&library)?;
        Ok(Self { host, library, symbols })
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
    /// Returns [`crate::LeanError::LeanException`] if the Lean-side
    /// import raises (missing `.olean`, malformed module name, …),
    /// with the bounded message Lean surfaced.
    pub fn session<'c>(&'c self, imports: &[&str]) -> LeanResult<LeanSession<'lean, 'c>> {
        LeanSession::import(self, imports)
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
}
