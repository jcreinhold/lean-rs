//! Application-owned native trust ingress for a dedicated worker process.
use lean_rs::host_bridge::{
    HostCachedModuleBatch, HostEnvironment, HostModuleBatch, HostModuleQuery, HostSourceOptions,
    TrustedHostKernelRequest, TrustedHostTerm, TrustedModuleSource,
    module_requests::{DeclarationVerificationBatchRequest, DeclarationVerificationRequest, ProofAttemptRequest},
};
use lean_rs::module::{
    AuthorizedWorkerSnapshot, HostImportOptions, LeanLibrary, LoadedHostArtifacts, TrustedHostCapability,
    TrustedHostImports, TrustedWorkerCapability,
};
use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::host::LeanPreparedHostArtifacts;
use lean_rs_worker_protocol::worker_exports::WorkerExportOperation;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Inputs to the application source authorizer, from the actual opened worker session.
pub struct WorkerSourceContext<'a, 'cap> {
    /// Actual loaded and attested shim.
    pub capability: &'cap TrustedHostCapability<'static>,
    /// Environment receiving the operation.
    pub environment: &'a HostEnvironment<'static>,
    /// Original immutable import authority.
    pub imports: &'a TrustedHostImports<'static, 'cap>,
}

/// The actual bounded input to a checked worker String → IO String export.
pub struct WorkerStringCall<'a> {
    /// The admitted manifest-backed worker capability for this session.
    pub capability: &'a TrustedWorkerCapability<'static>,
    /// Project root associated with the opened worker session.
    pub project_root: &'a str,
    /// Protocol operation used to address the export.
    pub operation: WorkerExportOperation,
    /// Exact named export the request will invoke.
    pub export: &'a str,
    /// Unmodified payload from the bounded worker frame.
    pub request_json: &'a str,
}

/// Application-owned read admission for one canonical manifest in a protected artifact root.
pub struct WorkerManifestRead {
    root: PathBuf,
    manifest: PathBuf,
    _custody: Box<dyn FnOnce()>,
}

impl WorkerManifestRead {
    /// Supply the canonical root and manifest selected by deployment policy, under a held guard.
    /// Custody covers all directory components, symlink resolution and the file through this read.
    /// This metadata grant confers no permission to initialize or call native code.
    #[must_use]
    pub fn new(root: PathBuf, manifest: PathBuf, custody: impl Sized + 'static) -> Self {
        Self {
            root,
            manifest,
            _custody: Box::new(move || drop(custody)),
        }
    }

    pub(crate) fn read_for<T>(
        &self,
        requested: &Path,
        use_bytes: impl FnOnce(&Path, &[u8]) -> LeanResult<T>,
    ) -> LeanResult<T> {
        let denied = || lean_rs::error::host_internal("manifest outside the approved path or custody root");
        // Reject path substitution before performing any filesystem operation on the request.
        if self.manifest != requested || !self.root.is_absolute() || !self.manifest.starts_with(&self.root) {
            return Err(denied());
        }
        if self.root.canonicalize().ok().as_deref() != Some(self.root.as_path())
            || self.manifest.canonicalize().ok().as_deref() != Some(self.manifest.as_path())
        {
            return Err(denied());
        }
        let bytes = crate::host_artifacts::read_bounded(&self.manifest)
            .map_err(|e| lean_rs::error::host_internal(e.to_string()))?;
        use_bytes(&self.manifest, &bytes)
    }
}

/// Trust decisions supplied by the embedding application before serving any requests.
///
/// A manifest or path from a parent is a request for admission. The returned token
/// must describe the actual loaded artifacts or exact captured source operation.
pub trait WorkerAuthority: Send + Sync {
    /// Load and attest the prepared shim under verified artifact custody.
    ///
    /// # Errors
    /// Returns an error if shim verification or loading fails.
    fn host(
        &self,
        runtime: &'static LeanRuntime,
        prepared: &LeanPreparedHostArtifacts,
    ) -> LeanResult<LoadedHostArtifacts<'static>>;
    /// Admit a project library, including its module initializers.
    /// Return the independently approved initializer with the actual loaded library.
    ///
    /// # Errors
    /// Returns an error if the library or initializer cannot be approved or loaded.
    fn project(
        &self,
        runtime: &'static LeanRuntime,
        path: &Path,
        requested: &lean_rs::module::LeanModuleInitializer,
    ) -> LeanResult<(LeanLibrary<'static>, lean_rs::module::LeanModuleInitializer)>;
    /// Acquire custody for the single deployment-approved manifest before bootstrap reads any bytes.
    ///
    /// # Errors
    /// Returns an error if the manifest path cannot be approved or custody cannot be acquired.
    fn manifest_read(&self, requested: &Path) -> LeanResult<WorkerManifestRead>;
    /// Verify the actual images and hold custody for the exact requested manifest and symbol contracts.
    /// Without a narrower authorization hook, permission covers every
    /// protocol-valid payload dispatched to each approved export. The
    /// application must only admit exports safe for that input domain.
    ///
    /// # Errors
    /// Returns an error if the snapshot, image or requested export contract cannot be verified.
    fn worker(
        &self,
        runtime: &'static LeanRuntime,
        path: &Path,
        snapshot: &[u8],
    ) -> LeanResult<AuthorizedWorkerSnapshot<'static>>;
    /// Narrow an already admitted String → IO String export to an exact call.
    ///
    /// The default preserves snapshot-wide payload authority. This hook cannot
    /// admit a different export, manifest, image, or ABI.
    ///
    /// # Errors
    /// Return an error to refuse this payload before native export entry.
    fn authorize_worker_string_call(&self, _call: WorkerStringCall<'_>) -> LeanResult<()> {
        Ok(())
    }
    /// Supply the project's import roots in Lean's search order, or `None` to ask Lake.
    ///
    /// With `None`, the worker runs `lake env` in `project` once and uses its
    /// answer. An application that already asked Lake, for example by running
    /// `lean_toolchain::LakeWorkspace::lean_path_command` under its own limits
    /// and reading the output with `read_lean_path`, returns that answer here,
    /// so the worker runs Lake no second time. The roots grant no
    /// import authority: [`Self::imports`] still approves the ordered roots.
    ///
    /// # Errors
    /// Returns an error if application policy refuses the project.
    fn project_import_roots(&self, _project: &Path) -> LeanResult<Option<Vec<PathBuf>>> {
        Ok(None)
    }
    /// Establish the exact import graph, ordered roots and profile.
    ///
    /// # Errors
    /// Returns an error if the import graph, search order or options cannot be approved.
    fn imports<'cap>(
        &self,
        cap: &'cap TrustedHostCapability<'static>,
        paths: Vec<String>,
        names: Vec<String>,
        options: HostImportOptions,
    ) -> LeanResult<TrustedHostImports<'static, 'cap>>;
    /// Validate the opened session before granting any source operation.
    ///
    /// `observed_roots` borrows the ordered roots retained from this session's import,
    /// including duplicates. `None` means the session has no retained observation.
    /// The default permits the existing operation-specific authorizer to decide.
    ///
    /// # Errors
    /// Return an error to stop this operation before its source grant.
    fn validate_source_context(
        &self,
        _context: &WorkerSourceContext<'_, '_>,
        _observed_roots: Option<&[String]>,
    ) -> LeanResult<()> {
        Ok(())
    }

    /// Admit the exact term request for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn term<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: String,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedHostTerm<'static, 'cap>>;
    /// Admit the exact kernel request for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn kernel<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: String,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedHostKernelRequest<'static, 'cap>>;
    /// Admit the exact query request for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn query<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: HostModuleQuery,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostModuleQuery>>;
    /// Admit the exact batch request for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn batch<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: HostModuleBatch,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostModuleBatch>>;
    /// Admit the exact cached request for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn cached<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: HostCachedModuleBatch,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostCachedModuleBatch>>;
    /// Admit the exact proof request for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn proof<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: ProofAttemptRequest,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, ProofAttemptRequest>>;
    /// Admit the exact verify request for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn verify<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: DeclarationVerificationRequest,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, DeclarationVerificationRequest>>;
    /// Admit the exact declaration verification batch for this session and source context.
    ///
    /// # Errors
    /// Returns an error if the application refuses the request or its options in this source context.
    fn verify_batch<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: DeclarationVerificationBatchRequest,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, DeclarationVerificationBatchRequest>>;
}

static AUTHORITY: OnceLock<Box<dyn WorkerAuthority>> = OnceLock::new();

/// Install the application loader and source authority for this dedicated child.
/// Call this once in the application-owned entry point before serving requests.
///
/// # Errors
///
/// Rejects attempts to replace an installed policy.
pub fn install_worker_authority(authority: impl WorkerAuthority + 'static) -> LeanResult<()> {
    AUTHORITY
        .set(Box::new(authority))
        .map_err(|_| lean_rs::__host_internals::host_module_init("worker authority already installed"))
}
/// Install the embedding application's authority before bootstrap or protocol dispatch.
///
/// # Errors
///
/// Returns an error if another authority already owns this process.
pub fn run_worker_child_with_authority(
    authority: impl WorkerAuthority + 'static,
) -> LeanResult<std::process::ExitCode> {
    install_worker_authority(authority)?;
    Ok(crate::run_worker_child_stdio())
}

pub(crate) fn worker_authority() -> LeanResult<&'static dyn WorkerAuthority> {
    AUTHORITY.get().map(Box::as_ref).ok_or_else(|| {
        lean_rs::__host_internals::host_module_init("native request requires an application-installed worker authority")
    })
}
