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
//! Lean compiles `def x : T := constant`—a nullary export whose body
//! reduces to a constant—to a persistent `lean_object*` global variable
//! (`lean_mark_persistent` at module init), not a callable function.
//! Calling such a symbol as a function pointer SIGBUSes. To make the
//! distinction invisible to callers, [`LeanLibrary::open`] reads the
//! dylib's bytes once, walks the export table with the [`object`] crate,
//! and records the names of data-section exports as `globals`.
//! [`LeanModule::exported_unchecked`](super::loaded::LeanModule::exported_unchecked) consults
//! the set to dispatch function-vs-global at call time.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment. The blanket allow exists because this is the
// single `pub` doorway into the dlopen/dlsym path; per
// `docs/architecture/safety.md` the opt-out lives at the
// smallest scope that compiles.
#![allow(unsafe_code)]

use std::collections::{BTreeMap, HashSet, btree_map::Entry};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    library: Arc<libloading::Library>,
    loader_identity: usize,
    path: PathBuf,
    runtime: &'lean LeanRuntime,
    /// Names of data-section exports (Lean nullary-constant globals),
    /// normalised to what [`libloading::Library::get`] resolves with
    /// (Mach-O leading underscore stripped). Computed once at [`open`]
    /// and consulted by
    /// [`LeanModule::exported_unchecked`](super::loaded::LeanModule::exported_unchecked) to
    /// dispatch function-vs-global at call time.
    ///
    /// [`open`]: Self::open
    globals: HashSet<String>,
}

/// Obtain the native handle identity while preserving the original loader reference.
#[allow(unsafe_code, reason = "round-trip the same owned platform handle without closing it")]
fn identify_library(library: libloading::Library) -> (libloading::Library, usize) {
    #[cfg(unix)]
    use libloading::os::unix::Library;
    #[cfg(windows)]
    use libloading::os::windows::Library;
    let raw = Library::from(library).into_raw();
    // SAFETY: into_raw transfers one live loader reference. Reconstitute that same
    // reference exactly once; its numeric identity is never converted back to a pointer.
    let library = unsafe { Library::from_raw(raw) };
    (library.into(), raw as usize)
}

impl<'lean> LeanLibrary<'lean> {
    /// Load a Lake-built Lean shared object from `path`.
    ///
    /// Reads the file's symbol table once to classify each exported
    /// symbol as a function (text/code section) or a Lean
    /// nullary-constant global (data/rodata/bss section). The
    /// classification is consulted by
    /// [`LeanModule::exported_unchecked`](super::loaded::LeanModule::exported_unchecked) so
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
    /// # Safety
    ///
    /// The actual library and all dependencies must be trusted native code for
    /// this Lean runtime. Their loader constructors, module initializers, global
    /// registrations, and unload behavior must be safe. All initializer symbols
    /// must follow Lean's initializer ABI. The application must coordinate transitive
    /// and external initialization so a failed dependency cannot appear initialized
    /// inside another native initializer. Keep required code/data loaded while
    /// any native value or process-global registration can refer to it. A path,
    /// file-format check, or build manifest does not establish these facts.
    pub unsafe fn open(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self> {
        let path = path.as_ref();
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.module.library.open",
            path = %crate::error::redact::short_path(path),
        )
        .entered();
        let globals = classify_globals(path)?;
        // SAFETY: the caller established native loader, initializer, runtime,
        // dependency, and unload safety for the actual artifact being opened.
        let library = unsafe { libloading::Library::new(path) }.map_err(|err| {
            LeanError::module_init(format!("failed to open Lean library '{}': {err}", path.display()))
        })?;
        let (library, loader_identity) = identify_library(library);
        Ok(Self {
            loader_identity,
            library: Arc::new(library),
            path: path.to_path_buf(),
            runtime,
            globals,
        })
    }

    /// Open a Lake-built Lean shared object with **globally visible
    /// symbols** (POSIX `RTLD_GLOBAL` on Unix; the Windows side stays
    /// on the default loader since DLL symbols are already global).
    ///
    /// The same contract as [`LeanLibrary::open`], plus: symbols
    /// defined by this dylib become visible to the dynamic linker's
    /// global namespace, so any subsequently `dlopen`ed dylib whose
    /// initializer chain references them resolves correctly.
    ///
    /// The motivating case is the `lean-rs-host` shim load: the host shim
    /// package imports the generic interop shim package, so opening the
    /// generic dylib globally first lets the host shim initializer resolve
    /// the generated `LeanRsInterop.*` references normally.
    ///
    /// # Errors
    ///
    /// Same as [`LeanLibrary::open`].
    /// # Safety
    ///
    /// All requirements of [`Self::open`] apply. The library's globally exposed
    /// symbols must also be compatible with every native consumer that can bind
    /// them; global visibility is not a signature or artifact check.
    pub unsafe fn open_globally(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self> {
        let path = path.as_ref();
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.module.library.open_globally",
            path = %crate::error::redact::short_path(path),
        )
        .entered();
        let globals = classify_globals(path)?;
        let library = open_with_global_visibility(path)?;
        let (library, loader_identity) = identify_library(library);
        Ok(Self {
            loader_identity,
            library: Arc::new(library),
            path: path.to_path_buf(),
            runtime,
            globals,
        })
    }

    /// Initialize the Lean module identified by `(package, module)`.
    ///
    /// Resolves the Lake-mangled initializer symbol against this
    /// library, invokes it under a panic boundary with the runtime
    /// `builtin` flag, and decodes the resulting `IO Unit`. Successful
    /// initialization is idempotent. If this call enters native code and fails,
    /// the loader retains the image until process exit and returns the original error
    /// on subsequent calls to the same native initializer, including through another handle.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with:
    ///
    /// - [`HostStage::Link`] if `package` or `module` is not a valid
    ///   Lake identifier, or the mangled initializer symbol is not
    ///   exported by this library.
    /// - [`HostStage::Load`] if the initializer is already running, panics,
    ///   returns `IO.error`, or previously failed.
    pub fn initialize_module<'lib>(&'lib self, package: &str, module: &str) -> LeanResult<LeanModule<'lean, 'lib>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.module.library.initialize",
            package = package,
            module = module,
        )
        .entered();
        let name = InitializerName::from_lake_names(package, module)?;
        self.run_initializer(&name)?;
        Ok(LeanModule::new(self, name))
    }

    fn run_initializer(&self, name: &InitializerName) -> LeanResult<()> {
        // Generated initializers set their "already initialized" bit before fallible
        // work. Only the first native result distinguishes failure from success.
        struct Attempt {
            // Retain the mapping while running or failed, so this address cannot be
            // recycled even after every application handle to the failed image drops.
            _image: Arc<libloading::Library>,
            failure: Option<LeanError>,
        }
        static ATTEMPTS: Mutex<BTreeMap<usize, Attempt>> = Mutex::new(BTreeMap::new());
        let init = self.lookup_initializer(name)?;
        let address = init as usize;
        {
            let mut attempts = ATTEMPTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match attempts.entry(address) {
                Entry::Occupied(entry) => {
                    return Err(entry.get().failure.clone().unwrap_or_else(|| {
                        LeanError::module_init(format!("module '{}' initializer is already running", name.display()))
                    }));
                }
                Entry::Vacant(entry) => {
                    entry.insert(Attempt {
                        _image: Arc::clone(&self.library),
                        failure: None,
                    });
                }
            }
        }

        // Foreign code can call back into the loader. Never hold the registry lock
        // across initialization; the occupied entry also rejects same-address reentry.
        let result = call_initializer(self.runtime, init, name);
        let mut attempts = ATTEMPTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &result {
            Ok(()) => {
                attempts.remove(&address);
            }
            Err(error) => {
                attempts.insert(
                    address,
                    Attempt {
                        _image: Arc::clone(&self.library),
                        failure: Some(error.clone()),
                    },
                );
            }
        }
        result
    }

    /// Check the original loader arguments without reopening the image.
    /// The application remains responsible for protecting path resolution and image bytes.
    #[must_use]
    pub fn opened_for(&self, runtime: &LeanRuntime, path: &Path) -> bool {
        std::ptr::eq(self.runtime, runtime) && self.path.as_os_str() == path.as_os_str()
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

    /// Initialized Lean code can escape through process-global registrations
    /// and runtime-bound values. Trusted safe facades retain the native image
    /// until process exit, independently of the Rust token's own lifetime.
    pub(crate) fn pin_for_process(&self) {
        // A retained native loader reference keeps its handle unique until process exit.
        // Reopening the same image can create new Rust Library wrappers around that handle.
        static PINNED: std::sync::Mutex<Option<std::collections::HashMap<usize, std::sync::Arc<libloading::Library>>>> =
            std::sync::Mutex::new(None);
        let mut pinned = PINNED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pinned
            .get_or_insert_with(Default::default)
            .entry(self.loader_identity)
            .or_insert_with(|| Arc::clone(&self.library));
    }

    /// On-disk path the library was opened from. Used only for
    /// diagnostics.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve `name` as a function-pointer symbol (text section).
    ///
    /// Private to `lean-rs`: callers reach this through
    /// [`LeanModule::exported_unchecked`](super::loaded::LeanModule::exported_unchecked)
    /// or manifest-checked [`LeanCapability::exported`](super::capability::LeanCapability::exported),
    /// not by handling raw dynamic-loader addresses.
    pub(crate) fn resolve_function_symbol(&self, name: &str) -> LeanResult<*mut c_void> {
        // SAFETY: `libloading::Library::get::<*mut c_void>` is the raw
        // address lookup; the returned `Symbol<'_, *mut c_void>` borrows
        // from `self.library`, so dereferencing it inside this scope is
        // valid. We copy the address out via `*symbol`—the same idiom
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

    /// Resolve the module initializer by `dlsym`, trying both the modern
    /// (Lean ≥ 4.27) and legacy (Lean ≤ 4.26) Lake symbol shapes. The
    /// diagnostic on failure names both candidates so the operator can
    /// see what was searched.
    fn lookup_initializer(&self, name: &InitializerName) -> LeanResult<RawInitializer> {
        // SAFETY: the type parameter spells the canonical Lake
        // initializer signature (`(u8) -> *mut lean_object`), verified
        // against the Lake-emitted C in `fixtures/lean/.lake/build/ir/`
        // for every supported Lean version. `libloading::Library::get`
        // returns a borrowed `Symbol<'_, T>`; copying the raw function
        // pointer out of it via deref is the standard pattern when the
        // caller does not need to retain the borrow.
        let modern: Result<libloading::Symbol<'_, RawInitializer>, _> =
            unsafe { self.library.get(name.symbol_bytes()) };
        if let Ok(symbol) = modern {
            return Ok(*symbol);
        }
        // SAFETY: same as above; the legacy symbol shape is the only
        // exported form for Lean ≤ 4.26 dylibs.
        let legacy: Result<libloading::Symbol<'_, RawInitializer>, _> =
            unsafe { self.library.get(name.legacy_symbol_bytes()) };
        match legacy {
            Ok(symbol) => Ok(*symbol),
            Err(err) => Err(LeanError::linking(format!(
                "missing initializer symbol in '{}': tried '{}' and '{}': {err}",
                self.path.display(),
                name.symbol_str(),
                name.legacy_symbol_str(),
            ))),
        }
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

/// Open `path` with the dynamic loader's *global* symbol-visibility
/// flag set so any subsequently loaded dylib can resolve symbols
/// defined here.
///
/// On Unix this means `RTLD_LAZY | RTLD_GLOBAL`. On Windows the
/// platform loader publishes module symbols globally by default, so
/// the standard [`libloading::Library::new`] path is sufficient.
fn open_with_global_visibility(path: &Path) -> LeanResult<libloading::Library> {
    #[cfg(unix)]
    {
        // SAFETY: identical contract to `Library::new`—runs the
        // platform dynamic loader against `path`. The added
        // `RTLD_GLOBAL` flag only affects symbol-table visibility for
        // later loads; it does not change initializer-execution
        // semantics (Lake-built Lean dylibs have no constructor-style
        // init).
        let unix_library = unsafe {
            libloading::os::unix::Library::open(
                Some(path),
                libloading::os::unix::RTLD_LAZY | libloading::os::unix::RTLD_GLOBAL,
            )
        }
        .map_err(|err| {
            LeanError::module_init(format!(
                "failed to open Lean library '{}' with RTLD_GLOBAL: {err}",
                path.display()
            ))
        })?;
        Ok(unix_library.into())
    }
    #[cfg(not(unix))]
    {
        // SAFETY: same as `Library::new`. See unix branch.
        let library = unsafe { libloading::Library::new(path) }.map_err(|err| {
            LeanError::module_init(format!("failed to open Lean library '{}': {err}", path.display()))
        })?;
        Ok(library)
    }
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

#[cfg(test)]
#[test]
#[allow(
    unsafe_code,
    clippy::expect_used,
    reason = "reopen the exact pinned test image under held fixture custody"
)]
fn reopening_pinned_image_does_not_retain_additional_wrappers() {
    let first = crate::fixed_test_library();
    first.pin_for_process();
    for _ in 0..8 {
        // SAFETY: this is the same approved path under the retained fixture reader,
        // on the same initialized runtime with a permanent reference to its image.
        let reopened = unsafe { LeanLibrary::open(first.runtime, first.path()) }.expect("reopen held fixture");
        assert_eq!(reopened.loader_identity, first.loader_identity);
        reopened.pin_for_process();
        assert_eq!(Arc::strong_count(&reopened.library), 1);
    }
}
