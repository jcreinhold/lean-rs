//! Explicit native artifact and semantic-contract assertions for safe facades.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::LeanCapability;
use crate::{LeanError, LeanResult};

/// A host shim loaded from one immutable manifest snapshot.
/// The manifest, search paths, loaded token, and an optional application guard
/// travel together.
pub struct LoadedHostArtifacts<'lean> {
    host: TrustedHostCapability<'lean>,
    manifest_path: PathBuf,
    manifest: Vec<u8>,
    search_paths: Vec<PathBuf>,
    // Dropping the closure releases the captured application guard.
    _custody: Box<dyn FnOnce() + 'lean>,
}

impl<'lean> LoadedHostArtifacts<'lean> {
    /// Load the exact snapshot and keep `custody` until this descriptor drops.
    ///
    /// `custody` is any value the application wants dropped with the descriptor,
    /// such as a lock or lease of its own; pass `()` when there is none.
    ///
    /// # Safety
    ///
    /// Before calling, verify the actual primary and dependency images and their complete
    /// host contracts as required by [`LeanCapability::from_manifest_snapshot`] and
    /// [`TrustedHostCapability::from_loaded`]. Nothing may replace these files, change how
    /// the manifest resolves, or change the ordered search paths during and after loading.
    /// Keep any code or data needed by escaping Lean values and registrations available
    /// through their full lifetime, including after dropping this descriptor.
    ///
    /// # Errors
    ///
    /// Propagates manifest, preflight, load, or initializer failures.
    #[allow(
        unsafe_code,
        reason = "application verifies the native artifacts and the actual host ABI"
    )]
    pub unsafe fn load(
        runtime: &'lean crate::LeanRuntime,
        manifest_path: PathBuf,
        manifest: Vec<u8>,
        search_paths: Vec<PathBuf>,
        custody: impl Sized + 'lean,
    ) -> LeanResult<Self> {
        // SAFETY: the caller keeps this exact snapshot and every artifact its loader
        // reaches unchanged. The same bytes and paths are retained below.
        let image = unsafe { LeanCapability::from_manifest_snapshot(runtime, &manifest_path, &manifest) }?;
        // SAFETY: the caller attests the actual images just loaded from that snapshot.
        let host = unsafe { TrustedHostCapability::from_loaded(image) };
        Ok(Self {
            host,
            manifest_path,
            manifest,
            search_paths,
            _custody: Box::new(move || drop(custody)),
        })
    }

    /// Borrow the attested loaded shim for closed native dispatch.
    #[must_use]
    pub fn host(&self) -> &TrustedHostCapability<'lean> {
        &self.host
    }

    /// Compare a prepared descriptor against every input retained at loading.
    #[must_use]
    pub fn matches_snapshot(&self, path: &Path, bytes: &[u8], search_paths: &[PathBuf]) -> bool {
        self.manifest_path == path && self.manifest == bytes && self.search_paths == search_paths
    }
}

/// A loaded capability attested to implement the bundled host shim contracts.
///
/// Construction requires an unsafe assertion about the actual retained native
/// images. Manifest fields, symbol names, and matching C slots cannot construct
/// this token safely. Its capability and dependencies cannot be substituted.
/// Native images remain loaded until process exit because Lean registrations
/// and returned opaque values can outlive this Rust token.
pub struct TrustedHostCapability<'lean> {
    capability: LeanCapability<'lean>,
}

impl<'lean> TrustedHostCapability<'lean> {
    /// Attest the fixed bundled host contracts for this loaded capability.
    ///
    /// # Safety
    ///
    /// The actual native images must implement every present bundled
    /// `lean_rs_host_*` export with the complete contract of this SDK version:
    /// Lean value layouts, scalar refinements, ownership, runtime identity,
    /// callback protocols, synchronous callback lifetimes, and returned opaque
    /// types. Optional symbols may be absent. A same-named symbol or manifest
    /// assertion is insufficient. Native loading/initialization requirements
    /// must already have been established for this capability and dependencies.
    #[allow(unsafe_code, reason = "the application attests actual loaded host semantics")]
    pub unsafe fn from_loaded(capability: LeanCapability<'lean>) -> Self {
        capability.pin_for_process();
        Self { capability }
    }

    /// Diagnostic path of the attested loaded primary image.
    #[must_use]
    pub fn library_path(&self) -> &Path {
        self.capability.library().path()
    }

    pub(crate) fn capability(&self) -> &LeanCapability<'lean> {
        &self.capability
    }

    /// Attest the exact future import graph and search-path resolution.
    ///
    /// # Safety
    ///
    /// Every module artifact reachable through these exact inputs must be
    /// trusted and compatible with the retained capability and Lean runtime.
    /// Its native code, extensions, and initializers must uphold runtime value,
    /// ownership, callback, and initialization contracts. The assertion must
    /// remain valid through every use of this scope: retain artifact custody
    /// against replacement and do not substitute a different native/olean pair.
    /// Imported native code must remain loaded while any returned value or
    /// process-global registration can refer to it, including after this scope
    /// or its host token is dropped.
    /// A path label or module name alone is not evidence of these properties.
    #[allow(unsafe_code, reason = "the application attests actual future module artifacts")]
    pub unsafe fn import_scope<'cap>(
        &'cap self,
        search_paths: Vec<String>,
        imports: Vec<String>,
    ) -> TrustedHostImports<'lean, 'cap> {
        TrustedHostImports::new(self, search_paths, imports, HostImportOptions::default())
    }

    /// Attest the module graph for the bracketed, no-extension import query.
    ///
    /// # Safety
    ///
    /// The ordered paths and modules must resolve to compatible, trusted artifacts throughout
    /// use, as in [`Self::import_scope`]. This operation uses Lean's withImportModules bracket
    /// and frees its compacted regions after copying the query result.
    #[allow(unsafe_code, reason = "application attests the actual bracketed import graph")]
    pub unsafe fn bracketed_import_scope<'cap>(
        &'cap self,
        paths: Vec<String>,
        imports: Vec<String>,
    ) -> TrustedBracketedImports<'lean, 'cap> {
        TrustedBracketedImports(TrustedHostImports::new(
            self,
            paths,
            imports,
            HostImportOptions::default(),
        ))
    }

    /// Attest an import with its exact profile and profiler output settings.
    ///
    /// # Safety
    ///
    /// All artifact, retention and runtime obligations of [`Self::import_scope`] apply
    /// to the graph selected by these options, including extension initializers.
    /// The profiler output destination must be authorized for native writes.
    #[allow(unsafe_code, reason = "application attests the graph and native profiler options")]
    pub unsafe fn import_scope_with_options<'cap>(
        &'cap self,
        search_paths: Vec<String>,
        imports: Vec<String>,
        options: HostImportOptions,
    ) -> TrustedHostImports<'lean, 'cap> {
        TrustedHostImports::new(self, search_paths, imports, options)
    }

    /// Attest one native plugin and its matching compiled modules for this import.
    ///
    /// # Safety
    ///
    /// All obligations of [`Self::import_scope_with_options`] apply. Under retained custody,
    /// verify that the canonical image, its transitive native dependencies, the exact Lean
    /// initializer ABI and the selected compiled modules belong to one trusted deployment.
    /// Fix the native loader environment before process startup. The initializer may run again
    /// on a fresh import and may publish process-wide state even if a later import step fails.
    /// Keep this image and module generation fixed for the native process, including after a
    /// failed import, and retain any data its registrations may use. Loading a plugin can run
    /// its initializer even when environment-extension loading is disabled.
    ///
    /// # Errors
    ///
    /// Rejects a missing or noncanonical image, a non-UTF-8 path, or an invalid symbol name.
    #[allow(
        unsafe_code,
        reason = "the application attests the plugin and its compiled import graph"
    )]
    pub unsafe fn import_scope_with_plugin<'cap>(
        &'cap self,
        search_paths: Vec<String>,
        imports: Vec<String>,
        options: HostImportOptions,
        image: PathBuf,
        initializer: String,
    ) -> LeanResult<TrustedHostImports<'lean, 'cap>> {
        if !image.is_absolute() || !image.is_file() || image.canonicalize().ok().as_ref() != Some(&image) {
            return Err(LeanError::module_init(
                "plugin image must be an existing canonical absolute file",
            ));
        }
        let image = image
            .into_os_string()
            .into_string()
            .map_err(|_| LeanError::module_init("plugin image path must be UTF-8"))?;
        if initializer.is_empty() || initializer.as_bytes().contains(&0) {
            return Err(LeanError::module_init(
                "plugin initializer must be a nonempty symbol without NUL",
            ));
        }
        let mut scope = TrustedHostImports::new(self, search_paths, imports, options);
        scope.plugin = Some((image, initializer));
        Ok(scope)
    }
}

/// An attestation usable only by the bracketed import operation.
pub struct TrustedBracketedImports<'lean, 'cap>(pub(crate) TrustedHostImports<'lean, 'cap>);

/// Native options captured when establishing an import scope.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent settings mirror the native import protocol"
)]
pub struct HostImportOptions {
    /// Include declarations available through Lean's import-all mode.
    pub import_all: bool,
    /// Bundled shim level code (exported 0, server 1, private 2).
    pub level: u8,
    /// Load environment extensions.
    pub load_extensions: bool,
    /// Enable the Lean profiler.
    pub profiler: bool,
    /// Enable profiler tracing.
    pub trace_profiler: bool,
    /// Destination passed to the native profiler.
    pub trace_output: String,
}

impl Default for HostImportOptions {
    fn default() -> Self {
        Self {
            import_all: false,
            level: 2,
            load_extensions: true,
            profiler: false,
            trace_profiler: false,
            trace_output: String::new(),
        }
    }
}

/// Process-local identity of one immutable import attestation. A cache keeps its
/// allocation live, so newly issued attestations cannot reuse the same identity.
#[derive(Clone, Debug)]
pub struct HostImportIdentity(std::rc::Rc<()>);

impl PartialEq for HostImportIdentity {
    fn eq(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for HostImportIdentity {}

/// An immutable import operation attested for one retained host capability.
///
/// Construction is unsafe because import can execute native initializers from
/// the selected module graph. Safe callers cannot add paths or names afterward.
#[derive(Clone)]
pub struct TrustedHostImports<'lean, 'cap> {
    capability: &'cap TrustedHostCapability<'lean>,
    search_paths: Vec<String>,
    imports: Vec<String>,
    options: HostImportOptions,
    plugin: Option<(String, String)>,
    identity: HostImportIdentity,
    pub(crate) native_identity: crate::Obj<'lean>,
}

impl<'lean, 'cap> TrustedHostImports<'lean, 'cap> {
    fn new(
        capability: &'cap TrustedHostCapability<'lean>,
        search_paths: Vec<String>,
        imports: Vec<String>,
        options: HostImportOptions,
    ) -> Self {
        Self {
            capability,
            search_paths,
            imports,
            options,
            // A fresh Lean heap object for every attestation. Native cache entries retain
            // this object, so pointer identity cannot be recycled while an entry exists.
            native_identity: crate::abi::string::from_str(capability.capability.library().runtime(), "import scope"),
            plugin: None,
            identity: HostImportIdentity(std::rc::Rc::new(())),
        }
    }

    /// Identity suitable for same-process environment reuse under this attestation.
    #[must_use]
    pub fn identity(&self) -> HostImportIdentity {
        self.identity.clone()
    }

    /// Profile and profiler settings covered by this attestation.
    #[must_use]
    pub fn options(&self) -> &HostImportOptions {
        &self.options
    }

    /// Check that this attestation belongs to this loaded capability.
    ///
    /// # Errors
    ///
    /// Returns a module initialization error for another capability.
    pub fn check_capability(&self, capability: &TrustedHostCapability<'_>) -> LeanResult<()> {
        if core::ptr::eq(self.capability, capability) {
            Ok(())
        } else {
            Err(LeanError::module_init(
                "import scope belongs to a different retained host capability",
            ))
        }
    }
}

impl TrustedHostImports<'_, '_> {
    /// Application-selected canonical plugin image and exact initializer for this scope.
    #[must_use]
    pub fn plugin(&self) -> Option<(&str, &str)> {
        self.plugin
            .as_ref()
            .map(|(image, initializer)| (image.as_str(), initializer.as_str()))
    }

    /// Exact ordered module names admitted by the unsafe artifact assertion.
    #[must_use]
    pub fn imports(&self) -> &[String] {
        &self.imports
    }

    /// Check the exact module names and ordered resolution roots requested by a worker.
    #[must_use]
    pub fn matches_request(&self, paths: &[String], names: &[String]) -> bool {
        self.search_paths == paths && self.imports == names
    }

    pub(crate) fn native_inputs(
        &self,
        capability: &TrustedHostCapability<'_>,
    ) -> LeanResult<(Vec<String>, Vec<String>)> {
        self.check_capability(capability)?;
        Ok((self.search_paths.clone(), self.imports.clone()))
    }
}

/// Closed native contracts supported by worker command dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerExportContract {
    /// `String -> IO String`, including correct Lean String layouts.
    StringIoString,
    /// Standard synchronous String request/row callback command protocol.
    StringStream,
    /// Two unboxed `UInt64` arguments and an unboxed `UInt64` result.
    U64Binary,
    /// One unboxed `UInt8` argument and a boxed `Unit` result.
    U8ToUnit,
}

/// An application-verified worker snapshot awaiting admission by the worker loader.
/// Holds custody from verification through the native load and token lifetime.
pub struct AuthorizedWorkerSnapshot<'custody> {
    runtime: &'custody crate::LeanRuntime,
    path: PathBuf,
    bytes: Vec<u8>,
    contracts: Vec<(String, WorkerExportContract)>,
    custody: Box<dyn FnOnce() + 'custody>,
}

impl<'custody> AuthorizedWorkerSnapshot<'custody> {
    /// Seal the exact snapshot and its externally verified native contracts before loading.
    ///
    /// # Safety
    ///
    /// Establish all artifact, dependency, initializer, lifetime and symbol obligations
    /// of [`TrustedWorkerCapability::load_snapshot`] for precisely these path/bytes.
    /// The guard must prevent substitution through verification, load and use.
    #[allow(unsafe_code, reason = "application is the native artifact and contract authority")]
    pub unsafe fn attest(
        runtime: &'custody crate::LeanRuntime,
        path: PathBuf,
        bytes: Vec<u8>,
        contracts: Vec<(String, WorkerExportContract)>,
        custody: impl Sized + 'custody,
    ) -> Self {
        Self {
            runtime,
            path,
            bytes,
            contracts,
            custody: Box::new(move || drop(custody)),
        }
    }

    /// Admit only the exact snapshot requested by the worker before opening any native image.
    ///
    /// # Errors
    ///
    /// Rejects substituted paths or bytes and propagates loader errors.
    #[allow(
        unsafe_code,
        reason = "the immutable authorization retains verified inputs and custody"
    )]
    pub fn load_for<'lean>(
        self,
        runtime: &'lean crate::LeanRuntime,
        path: &Path,
        bytes: &[u8],
    ) -> LeanResult<TrustedWorkerCapability<'lean>>
    where
        'custody: 'lean,
    {
        if !core::ptr::eq(self.runtime, runtime) || self.path != path || self.bytes != bytes {
            return Err(LeanError::module_init(
                "worker authority returned a different manifest snapshot",
            ));
        }
        // SAFETY: attest sealed the path, bytes, contracts and covering guard. None is replaceable.
        unsafe { TrustedWorkerCapability::load_snapshot(runtime, self.path, self.bytes, self.contracts, self.custody) }
    }
}

/// A loaded capability with an immutable, explicitly attested symbol allowlist.
///
/// A request cannot add symbols or choose a different contract after unsafe
/// establishment. This token does not infer semantic trust from manifest data.
/// Its native images remain loaded until process exit.
pub struct TrustedWorkerCapability<'lean> {
    pub(super) capability: LeanCapability<'lean>,
    contracts: BTreeMap<String, WorkerExportContract>,
    snapshot: Option<(PathBuf, Vec<u8>)>,
    _custody: Option<Box<dyn FnOnce() + 'lean>>,
}

impl<'lean> TrustedWorkerCapability<'lean> {
    /// Attest exact symbol contracts for the actual loaded worker capability.
    ///
    /// This consumes an initialized capability. Its primary image and dependencies
    /// remain mapped until process exit even if the allowlist is rejected.
    ///
    /// # Safety
    ///
    /// Every listed symbol in this retained capability must implement the
    /// complete selected contract, including Lean value layouts, runtime
    /// identity, ownership, callback payload/trampoline compatibility, and
    /// synchronous callback lifetimes. The assertion must cover the actual
    /// loaded primary/dependency images. Request-controlled strings or manifest
    /// contract labels alone cannot establish this assertion.
    ///
    /// # Errors
    ///
    /// Rejects empty symbol names and duplicate allowlist entries.
    #[allow(unsafe_code, reason = "the application attests an immutable native symbol allowlist")]
    pub unsafe fn from_loaded(
        capability: LeanCapability<'lean>,
        contracts: impl IntoIterator<Item = (String, WorkerExportContract)>,
    ) -> LeanResult<Self> {
        // Earlier native initialization or export calls may already have exposed
        // image-backed values and registrations, even when admission returns an error.
        capability.pin_for_process();
        let contracts = collect_contracts(contracts)?;
        Ok(Self {
            capability,
            contracts,
            snapshot: None,
            _custody: None,
        })
    }

    /// Load the exact worker manifest bytes and retain the verified custody guard.
    /// Before attempting each initializer, retain its image and earlier explicit
    /// dependencies through process exit, including if a later load fails.
    ///
    /// # Safety
    ///
    /// Satisfy all loader and initializer obligations of
    /// [`LeanCapability::from_manifest_snapshot`] and the actual native contract
    /// obligations of [`Self::from_loaded`]. Custody must cover artifact resolution
    /// and loaded dependencies. Escaping values require their artifacts to remain valid.
    ///
    /// # Errors
    ///
    /// Propagates loader, initializer and allowlist validation errors.
    #[allow(
        unsafe_code,
        reason = "application attests the exact snapshot and its native contracts"
    )]
    pub unsafe fn load_snapshot(
        runtime: &'lean crate::LeanRuntime,
        path: PathBuf,
        bytes: Vec<u8>,
        contracts: impl IntoIterator<Item = (String, WorkerExportContract)>,
        custody: impl Sized + 'lean,
    ) -> LeanResult<Self> {
        with_validated_contracts(contracts, |contracts| {
            // SAFETY: caller holds custody for these actual images and attests the validated symbol contracts.
            let capability = unsafe {
                LeanCapability::load_snapshot(runtime, &path, &bytes, super::bundle::BundleLifetime::Process)
            }?;
            Ok(Self {
                capability,
                contracts,
                snapshot: Some((path, bytes)),
                _custody: Some(Box::new(move || drop(custody))),
            })
        })
    }

    /// Whether the loaded token originated at the admitted path with these exact bytes.
    #[must_use]
    pub fn matches_snapshot(&self, path: &Path, bytes: &[u8]) -> bool {
        self.snapshot.as_ref().is_some_and(|(p, b)| p == path && b == bytes)
    }

    /// Reject an unlisted symbol or a request for a different native contract.
    ///
    /// This performs no symbol invocation or native value conversion.
    ///
    /// # Errors
    ///
    /// Returns a symbol-lookup error unless the exact attested pair exists.
    pub fn require_contract(&self, symbol: &str, expected: WorkerExportContract) -> LeanResult<()> {
        require_contract(&self.contracts, symbol, expected)
    }

    /// Diagnostic path of the attested loaded primary image.
    #[must_use]
    pub fn library_path(&self) -> &Path {
        self.capability.library().path()
    }
}

fn with_validated_contracts<T>(
    contracts: impl IntoIterator<Item = (String, WorkerExportContract)>,
    load: impl FnOnce(BTreeMap<String, WorkerExportContract>) -> LeanResult<T>,
) -> LeanResult<T> {
    load(collect_contracts(contracts)?)
}

fn collect_contracts(
    contracts: impl IntoIterator<Item = (String, WorkerExportContract)>,
) -> LeanResult<BTreeMap<String, WorkerExportContract>> {
    let mut result = BTreeMap::new();
    for (symbol, contract) in contracts {
        if symbol.is_empty() || result.insert(symbol.clone(), contract).is_some() {
            return Err(LeanError::symbol_lookup(format!(
                "worker contract allowlist has an empty or duplicate symbol '{symbol}'"
            )));
        }
    }
    Ok(result)
}

fn require_contract(
    contracts: &BTreeMap<String, WorkerExportContract>,
    symbol: &str,
    expected: WorkerExportContract,
) -> LeanResult<()> {
    match contracts.get(symbol) {
        Some(actual) if *actual == expected => Ok(()),
        Some(actual) => Err(LeanError::symbol_lookup(format!(
            "worker symbol '{symbol}' has attested contract {actual:?}, requested {expected:?}"
        ))),
        None => Err(LeanError::symbol_lookup(format!(
            "worker symbol '{symbol}' is not in the attested contract allowlist"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "contract table regressions report the exact failure")]
mod tests {
    use super::{WorkerExportContract, collect_contracts, require_contract};
    use crate::LeanDiagnosticCode;

    #[test]
    fn worker_contract_symbols_cannot_change_their_attested_protocol() {
        let contracts =
            collect_contracts([("json".to_owned(), WorkerExportContract::StringIoString)]).expect("one contract");
        require_contract(&contracts, "json", WorkerExportContract::StringIoString).expect("exact pair");
        let wrong = require_contract(&contracts, "json", WorkerExportContract::StringStream)
            .expect_err("a request cannot change the callback protocol");
        assert_eq!(wrong.code(), LeanDiagnosticCode::SymbolLookup);
        let unknown = require_contract(&contracts, "unknown", WorkerExportContract::StringIoString)
            .expect_err("a request cannot add a symbol");
        assert_eq!(unknown.code(), LeanDiagnosticCode::SymbolLookup);
    }

    #[test]
    fn invalid_worker_contracts_never_enter_the_native_initializer() {
        let mut calls = 0;
        for names in [vec![""], vec!["duplicate", "duplicate"]] {
            let result = super::with_validated_contracts(
                names
                    .into_iter()
                    .map(|s| (s.to_owned(), WorkerExportContract::U64Binary)),
                |_| {
                    calls += 1;
                    Ok(())
                },
            );
            assert!(result.is_err());
        }
        assert_eq!(calls, 0);
    }

    #[test]
    fn worker_contract_table_rejects_ambiguous_or_empty_symbols() {
        for entries in [
            vec![(String::new(), WorkerExportContract::U64Binary)],
            vec![
                ("duplicate".to_owned(), WorkerExportContract::StringIoString),
                ("duplicate".to_owned(), WorkerExportContract::StringStream),
            ],
        ] {
            let err = collect_contracts(entries).expect_err("the attestation must name each symbol exactly once");
            assert_eq!(err.code(), LeanDiagnosticCode::SymbolLookup);
        }
    }
}
