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
//!
//! ## Symbol-table walk at `open`
//!
//! Lean compiles `def x : T := constant` — a nullary export whose body
//! reduces to a constant — to a persistent `lean_object*` global variable
//! (`lean_mark_persistent` at module init), not a callable function.
//! Calling such a symbol as a function pointer SIGBUSes. To make the
//! distinction invisible to callers, [`LeanLibrary::open`] reads the
//! dylib's bytes once, walks the export table with the [`object`] crate,
//! and records the names of data-section exports as `globals`.
//! [`LeanModule::exported`](super::loaded::LeanModule::exported) consults
//! the set to dispatch function-vs-global at call time.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment. The blanket allow exists because this is the
// single `pub` doorway into the dlopen/dlsym path; per
// `docs/architecture/01-safety-model.md` the opt-out lives at the
// smallest scope that compiles.
#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use lean_rs_sys::lean_object;
use object::{Object, ObjectSection, ObjectSymbol, SectionKind, SymbolSection};

use super::initializer::{InitializerName, RawInitializer, call_initializer};
use super::loaded::LeanModule;
#[cfg(doc)]
use crate::error::HostStage;
use crate::error::{LeanError, LeanResult};
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
    /// Names of data-section exports (Lean nullary-constant globals),
    /// normalised to what [`libloading::Library::get`] resolves with
    /// (Mach-O leading underscore stripped). Computed once at [`open`]
    /// and consulted by
    /// [`LeanModule::exported`](super::loaded::LeanModule::exported) to
    /// dispatch function-vs-global at call time.
    ///
    /// [`open`]: Self::open
    globals: HashSet<String>,
}

impl<'lean> LeanLibrary<'lean> {
    /// Load a Lake-built Lean shared object from `path`.
    ///
    /// Reads the file's symbol table once to classify each exported
    /// symbol as a function (text/code section) or a Lean
    /// nullary-constant global (data/rodata/bss section). The
    /// classification is consulted by
    /// [`LeanModule::exported`](super::loaded::LeanModule::exported) so
    /// callers never write the function-vs-global distinction at the
    /// call site. The walk is cheap (a single `std::fs::read` plus an
    /// in-memory parse) and amortised across every later lookup.
    ///
    /// The `runtime` borrow is the type-level proof that the Lean runtime
    /// is up; it is retained for the symbol-initialization step but
    /// otherwise unused. Module initialization happens later through
    /// [`LeanLibrary::initialize_module`].
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Load`] if:
    ///
    /// - the file cannot be read (missing, permission denied),
    /// - the bytes do not parse as a recognised object format (Mach-O,
    ///   ELF, PE),
    /// - the dynamic linker fails to open the file (missing transitive
    ///   dependency, architecture mismatch, …).
    ///
    /// The diagnostic embeds the path and the underlying error message.
    pub fn open(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self> {
        let path = path.as_ref();
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.module.library.open",
            path = %crate::error::redact::short_path(path),
        )
        .entered();
        let globals = classify_globals(path)?;
        // SAFETY: `Library::new` runs the platform dynamic loader. Lake
        // does not emit constructor-style initializers for Lean
        // libraries (the per-module `initialize_*` functions are
        // explicit, not C constructors), so the load is side-effect-free
        // from Rust's perspective; the resulting handle releases the
        // library on drop.
        let library = unsafe { libloading::Library::new(path) }.map_err(|err| {
            LeanError::module_init(format!("failed to open Lean library '{}': {err}", path.display()))
        })?;
        Ok(Self {
            library,
            path: path.to_path_buf(),
            runtime,
            globals,
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
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.module.library.initialize",
            package = package,
            module = module,
        )
        .entered();
        let name = InitializerName::from_lake_names(package, module)?;
        let init = self.lookup_initializer(&name)?;
        call_initializer(self.runtime, init, &name)?;
        Ok(LeanModule::new(self, name))
    }

    /// Names of data-section exports for this library, normalised to the
    /// form [`libloading::Library::get`] resolves with.
    pub(crate) fn globals(&self) -> &HashSet<String> {
        &self.globals
    }

    /// The runtime borrow this library was opened with.
    pub(crate) fn runtime(&self) -> &'lean LeanRuntime {
        self.runtime
    }

    /// On-disk path the library was opened from. Used only for
    /// diagnostics.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve `name` as a function-pointer symbol (text section).
    ///
    /// Returns the raw symbol address. The caller is responsible for
    /// casting to the right `unsafe extern "C" fn(...) -> ...`
    /// signature.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Link`] if
    /// the symbol is not exported by this library. The diagnostic
    /// embeds the symbol name and the library path.
    pub(crate) fn resolve_function_symbol(&self, name: &str) -> LeanResult<*mut c_void> {
        // SAFETY: `libloading::Library::get::<*mut c_void>` is the raw
        // address lookup; the returned `Symbol<'_, *mut c_void>` borrows
        // from `self.library`, so dereferencing it inside this scope is
        // valid. We copy the address out via `*symbol` — the same idiom
        // `lookup_initializer` uses.
        let symbol: libloading::Symbol<'_, *mut c_void> =
            unsafe { self.library.get(name.as_bytes()) }.map_err(|err| {
                LeanError::symbol_lookup(format!(
                    "unknown exported symbol '{}' in '{}': {err}",
                    name,
                    self.path.display()
                ))
            })?;
        Ok(*symbol)
    }

    /// Resolve `name` as a function-pointer symbol, tolerating a missing
    /// symbol.
    ///
    /// Returns `Ok(Some(address))` when the symbol resolves, `Ok(None)`
    /// when it is absent from this library. Used by the host stack to
    /// pre-resolve *optional* capability symbols at
    /// [`crate::host::LeanCapabilities::new`] time without failing
    /// capability load when an older fixture omits a service.
    ///
    /// Symbol resolution at the dlsym level only fails with "symbol not
    /// present" — the dynamic linker has already accepted the library at
    /// [`Self::open`], so a per-symbol lookup error is either "missing"
    /// or a `\0` in the name (which the host stack never passes). Both
    /// are collapsed into the `Ok(None)` branch by design.
    pub(crate) fn resolve_optional_function_symbol(&self, name: &str) -> Option<*mut c_void> {
        // SAFETY: identical to `resolve_function_symbol`; the symbol
        // borrow does not escape this scope.
        match unsafe { self.library.get::<*mut c_void>(name.as_bytes()) } {
            Ok(symbol) => Some(*symbol),
            Err(_) => None,
        }
    }

    /// Resolve `name` as a Lean nullary-constant global symbol
    /// (data section). The returned pointer addresses the storage
    /// holding the persistent `lean_object*` value; the caller reads
    /// `*returned` to get the Lean object pointer itself.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Link`] if
    /// the symbol is not exported by this library.
    pub(crate) fn resolve_global_symbol(&self, name: &str) -> LeanResult<*mut *mut lean_object> {
        // SAFETY: `libloading::Library::get::<T>` returns the symbol's
        // address typed as `T`. For data symbols the address is the
        // location of the variable, so the parameterised type spells a
        // pointer to the variable's value. The borrow lifetime ends
        // when this function returns; we copy the address out.
        let symbol: libloading::Symbol<'_, *mut *mut lean_object> = unsafe { self.library.get(name.as_bytes()) }
            .map_err(|err| {
                LeanError::symbol_lookup(format!(
                    "unknown global symbol '{}' in '{}': {err}",
                    name,
                    self.path.display()
                ))
            })?;
        Ok(*symbol)
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
                LeanError::linking(format!(
                    "missing initializer symbol '{}' in '{}': {err}",
                    name.symbol_str(),
                    self.path.display(),
                ))
            })?;
        Ok(*symbol)
    }
}

impl std::fmt::Debug for LeanLibrary<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeanLibrary")
            .field("path", &self.path)
            .field("globals_count", &self.globals.len())
            .finish()
    }
}

/// Read `path` and collect the names of its data-section exports
/// (Lean nullary-constant globals).
///
/// Mach-O export names carry a leading `_` that `libloading` strips at
/// lookup; we strip here so the set keys match what
/// [`LeanLibrary::resolve_global_symbol`] queries with. ELF and PE
/// symbol names are stored without leading `_`, so no normalisation is
/// needed on those platforms.
///
/// Symbols whose section can't be classified (undefined, absolute,
/// common) are skipped: they cannot be the Lean-compiled persistent
/// globals we care about.
fn classify_globals(path: &Path) -> LeanResult<HashSet<String>> {
    let bytes = std::fs::read(path)
        .map_err(|err| LeanError::module_init(format!("failed to read Lean library '{}': {err}", path.display())))?;
    let file = object::File::parse(&*bytes)
        .map_err(|err| LeanError::module_init(format!("failed to parse object file '{}': {err}", path.display())))?;

    let strip_underscore = matches!(file.format(), object::BinaryFormat::MachO | object::BinaryFormat::Wasm);

    let mut globals = HashSet::new();
    // `symbols()` covers both ELF `.symtab` (when present) and Mach-O
    // `LC_SYMTAB` external defs; `dynamic_symbols()` on Mach-O omits
    // data-section externals such as Lean nullary-constant globals,
    // which is exactly what we need to classify.
    for symbol in file.symbols() {
        if !symbol.is_global() || symbol.is_undefined() {
            continue;
        }
        let SymbolSection::Section(section_index) = symbol.section() else {
            continue;
        };
        let Ok(section) = file.section_by_index(section_index) else {
            continue;
        };
        if !is_data_section(section.kind()) {
            continue;
        }
        let Ok(name) = symbol.name() else {
            continue;
        };
        let normalised = if strip_underscore {
            name.strip_prefix('_').unwrap_or(name)
        } else {
            name
        };
        if normalised.is_empty() {
            continue;
        }
        globals.insert(normalised.to_owned());
    }
    Ok(globals)
}

/// Section kinds that hold runtime data (Lean nullary-constant
/// `lean_object*` pointers live in `__data` on Mach-O, `.data` /
/// `.rodata` / `.bss` on ELF).
fn is_data_section(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Data
            | SectionKind::ReadOnlyData
            | SectionKind::ReadOnlyDataWithRel
            | SectionKind::UninitializedData
            | SectionKind::Common
            | SectionKind::Tls
            | SectionKind::UninitializedTls
    )
}
