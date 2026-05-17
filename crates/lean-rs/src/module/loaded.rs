//! Handle to a Lean module whose initializer has succeeded.
//!
//! [`LeanModule`] is the typed proof that
//! [`crate::module::LeanLibrary::initialize_module`] ran the module's
//! initializer to `IO.ok(())`. The only constructor is
//! `pub(super) fn new`, invoked at the end of `initialize_module`;
//! therefore a value of this type cannot exist unless the corresponding
//! module is live in the Lean runtime.
//!
//! This prompt (`11-module-loading-and-initializers`) lands the handle
//! itself plus a diagnostic accessor. Typed exported-function call
//! handles (`LeanExported{N}`) are added by prompt 12, which attaches
//! `exported_*` methods to this type.

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
    /// `pub(crate)` so the typed `LeanExported{N}` machinery landing in
    /// prompt 12 can resolve exported-function symbols against the
    /// same library this module was initialized from.
    #[allow(dead_code, reason = "first non-test caller lands in prompt 12 (LeanExported{N})")]
    pub(crate) fn library(&self) -> &'lib LeanLibrary<'lean> {
        self.library
    }

    /// Initializer name in its mangled C form.
    ///
    /// `pub(crate)` so prompt 12 can derive related symbol names from
    /// the same typed handle.
    #[allow(dead_code, reason = "first non-test caller lands in prompt 12 (LeanExported{N})")]
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
