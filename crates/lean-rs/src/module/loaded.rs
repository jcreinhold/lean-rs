//! Handle to a Lean module whose initializer has succeeded.
//!
//! [`LeanModule`] is the typed proof that
//! [`crate::module::LeanLibrary::initialize_module`] ran the module's
//! initializer to `IO.ok(())`. The only constructor is
//! `pub(super) fn new`, invoked at the end of `initialize_module`;
//! therefore a value of this type cannot exist unless the corresponding
//! module is live in the Lean runtime.
//!
//! Typed exported-function handles attach to this type via
//! [`LeanModule::exported_unchecked`], which returns a
//! [`crate::module::LeanExported`] parameterised by the argument tuple
//! and return-type marker.

use super::initializer::InitializerName;
use super::library::LeanLibrary;

/// A Lean module that has been linked and initialized successfully.
///
/// Holds a borrow of its source [`LeanLibrary`]; the `'lib` lifetime
/// keeps the dylib loaded while any `LeanModule` referring to it is
/// alive. The `'lean` lifetime is inherited from the library, which
/// inherits it from the [`crate::LeanRuntime`] borrow that anchored the
/// whole chain. Neither [`Send`] nor [`Sync`] (inherited from the
/// `&LeanLibrary` field).
pub struct LeanModule<'lean, 'lib> {
    library: &'lib LeanLibrary<'lean>,
    initializer: InitializerName,
}

impl<'lean, 'lib> LeanModule<'lean, 'lib> {
    /// Build the typed handle.
    ///
    /// `pub(super)` so only `LeanLibrary::initialize_module` can produce
    /// one — the construction site is the proof that initialization
    /// succeeded.
    pub(super) fn new(library: &'lib LeanLibrary<'lean>, initializer: InitializerName) -> Self {
        Self { library, initializer }
    }

    /// Human-readable `package::Module.Path` form for diagnostics.
    ///
    /// The format matches what
    /// [`crate::module::LeanLibrary::initialize_module`] accepted, so
    /// log messages and test assertions can round-trip the name a
    /// caller used.
    #[must_use]
    pub fn module_name(&self) -> &str {
        self.initializer.display()
    }

    /// Borrowed reference to the owning library.
    ///
    /// `pub(crate)` so the typed [`crate::module::exported`] machinery
    /// can resolve exported-function symbols against the same library
    /// this module was initialized from.
    pub(crate) fn library(&self) -> &'lib LeanLibrary<'lean> {
        self.library
    }

    /// Initializer name in its mangled C form.
    ///
    /// `pub(crate)` so internal symbol-derivation code can reach the
    /// canonical Lake mangling without re-running the validator.
    #[allow(dead_code, reason = "reserved for symbol-derivation helpers in later prompts")]
    pub(crate) fn initializer(&self) -> &InitializerName {
        &self.initializer
    }
}

impl std::fmt::Debug for LeanModule<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeanModule")
            .field("module", &self.initializer.display())
            .finish()
    }
}
