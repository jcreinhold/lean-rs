//! RAII handle over a Lake-built Lean shared object.
//!
//! [`LeanLibrary`] owns a [`libloading::Library`] for the duration of its
//! scope and exposes a single safe operation that callers actually want:
//! initialize a named Lean module out of it. The dlopen step, the Lake
//! symbol-mangling convention, the `IO Unit` decoding, and the
//! `builtin` flag policy are hidden inside the implementation; the
//! interface mentions only the human-readable package and module names.
//!
//! Construction requires a `&'lean LeanRuntime` borrow. Use-before-init
//! is therefore structurally impossible: a caller cannot build a
//! [`LeanLibrary`] without holding the proof that
//! [`crate::LeanRuntime::init`] has succeeded.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment. The blanket allow exists because this is the
// single `pub` doorway into the dlopen/dlsym path; per
// `docs/architecture/01-safety-model.md` the opt-out lives at the
// smallest scope that compiles.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use super::initializer::{InitializerName, RawInitializer, call_initializer};
use super::loaded::LeanModule;
use crate::error::{HostStage, LeanError, LeanResult};
use crate::runtime::LeanRuntime;

/// A loaded native Lean shared object.
///
/// Wraps a [`libloading::Library`] and hands out initialized module
/// handles via [`LeanLibrary::initialize_module`]. The `'lean` lifetime
/// parameter ties the library to the witnessing
/// [`crate::LeanRuntime`] borrow; the resulting [`LeanModule`] handles
/// borrow from `self`, so they cannot outlive the library that hosts
/// them.
///
/// Neither [`Send`] nor [`Sync`]: the `&'lean LeanRuntime` field
/// inherits the runtime's per-thread restriction (`LeanRuntime: !Sync`
/// implies `&LeanRuntime: !Send`, and `&LeanRuntime: Sync` iff
/// `LeanRuntime: Sync`). A negative-bound compile-time assertion in
/// the test module fails if either auto-trait is ever implemented.
pub struct LeanLibrary<'lean> {
    library: libloading::Library,
    path: PathBuf,
    runtime: &'lean LeanRuntime,
}

impl<'lean> LeanLibrary<'lean> {
    /// Load a Lake-built Lean shared object from `path`.
    ///
    /// The `runtime` borrow is the type-level proof that the Lean runtime
    /// is up; it is retained for the symbol-initialization step but
    /// otherwise unused. Module initialization happens later through
    /// [`LeanLibrary::initialize_module`].
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Load`] if the
    /// dynamic linker fails to open the file (missing file, unreadable
    /// permissions, missing transitive dependency, architecture mismatch,
    /// …). The diagnostic embeds the path and the underlying
    /// `libloading` message.
    pub fn open(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self> {
        let path = path.as_ref();
        // SAFETY: `Library::new` runs the platform dynamic loader. Lake
        // does not emit constructor-style initializers for Lean
        // libraries (the per-module `initialize_*` functions are
        // explicit, not C constructors), so the load is side-effect-free
        // from Rust's perspective; the resulting handle releases the
        // library on drop.
        let library = unsafe { libloading::Library::new(path) }.map_err(|err| {
            LeanError::host(
                HostStage::Load,
                format!("failed to open Lean library '{}': {err}", path.display()),
            )
        })?;
        Ok(Self {
            library,
            path: path.to_path_buf(),
            runtime,
        })
    }

    /// Initialize the Lean module identified by `(package, module)`.
    ///
    /// Resolves the Lake-mangled initializer symbol against this
    /// library, invokes it under a panic boundary with the runtime
    /// `builtin` flag, and decodes the resulting `IO Unit`. Idempotent:
    /// the Lean-emitted initializer body short-circuits to `IO.ok(())`
    /// on its second and later calls, so repeated invocations are safe
    /// and cheap.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with:
    ///
    /// - [`HostStage::Link`] if `package` or `module` is not a valid
    ///   Lake identifier, or the mangled initializer symbol is not
    ///   exported by this library.
    /// - [`HostStage::Load`] if the initializer panics or returns
    ///   `IO.error`.
    pub fn initialize_module<'lib>(&'lib self, package: &str, module: &str) -> LeanResult<LeanModule<'lean, 'lib>> {
        let name = InitializerName::from_lake_names(package, module)?;
        let init = self.lookup_initializer(&name)?;
        call_initializer(self.runtime, init, &name)?;
        Ok(LeanModule::new(self, name))
    }

    /// `libloading` symbol lookup, mapped to a typed host error.
    fn lookup_initializer(&self, name: &InitializerName) -> LeanResult<RawInitializer> {
        // SAFETY: the type parameter spells the canonical Lake
        // initializer signature (`(u8) -> *mut lean_object`) verified
        // against the Lake-emitted C in
        // `fixtures/lean/.lake/build/ir/`. `libloading::Library::get`
        // returns a borrowed `Symbol<'_, T>`; copying the raw function
        // pointer out of it via deref is the standard pattern when the
        // caller does not need to retain the borrow.
        let symbol: libloading::Symbol<'_, RawInitializer> =
            unsafe { self.library.get(name.symbol_bytes()) }.map_err(|err| {
                LeanError::host(
                    HostStage::Link,
                    format!(
                        "missing initializer symbol '{}' in '{}': {err}",
                        name.symbol_str(),
                        self.path.display(),
                    ),
                )
            })?;
        Ok(*symbol)
    }
}

impl std::fmt::Debug for LeanLibrary<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeanLibrary").field("path", &self.path).finish()
    }
}
