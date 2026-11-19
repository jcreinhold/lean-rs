use lean_rs::host_bridge::{
    HostCachedModuleBatch, HostModuleBatch, HostModuleQuery, HostSourceOptions,
    TrustedHostKernelRequest, TrustedHostTerm, TrustedModuleSource,
    module_requests::{
        DeclarationVerificationBatchRequest, DeclarationVerificationRequest, ProofAttemptRequest,
    },
};
use lean_rs::module::{
    AuthorizedWorkerSnapshot, HostImportOptions, LeanLibrary, LeanModuleInitializer,
    LoadedHostArtifacts, TrustedHostCapability, TrustedHostImports, WorkerExportContract,
};
use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::host::LeanPreparedHostArtifacts;
use lean_rs_worker_child::{WorkerAuthority, WorkerManifestRead, WorkerSourceContext};
use std::path::Path;

use metadata_qualification::{EXPORT, Held, Record, audit};
use std::sync::{Arc, OnceLock};
struct MetadataApp(OnceLock<Result<Arc<Held>, String>>);
impl MetadataApp {
    fn deployment(&self) -> LeanResult<&Arc<Held>> {
        self.0
            .get_or_init(|| {
                Held::acquire(Record::selected())
                    .and_then(|held| {
                        held.verify()?;
                        Ok(Arc::new(held))
                    })
                    .map_err(|error| {
                        audit(&format!("refused {error}"));
                        error.to_string()
                    })
            })
            .as_ref()
            .map_err(|message| lean_rs::error::host_internal(message.clone()))
    }
}
fn denied() -> lean_rs::LeanError {
    lean_rs::error::host_internal("operation outside this metadata application")
}
fn ioerr(e: std::io::Error) -> lean_rs::LeanError {
    audit(&format!("refused {e}"));
    lean_rs::error::host_internal(e.to_string())
}
impl WorkerAuthority for MetadataApp {
    fn manifest_read(&self, path: &Path) -> LeanResult<WorkerManifestRead> {
        let held = self.deployment()?;
        let d = &held.record;
        if path != d.consumer_manifest {
            return Err(denied());
        }
        Ok(WorkerManifestRead::new(
            d.project.clone(),
            path.into(),
            held.clone(),
        ))
    }
    fn worker(
        &self,
        runtime: &'static LeanRuntime,
        path: &Path,
        bytes: &[u8],
    ) -> LeanResult<AuthorizedWorkerSnapshot<'static>> {
        let held = self.deployment()?;
        let d = &held.record;
        if path != d.consumer_manifest || bytes != d.consumer_json.as_bytes() {
            return Err(denied());
        }
        audit("authorize-consumer-load");
        // SAFETY: the private producer checked the two exact images, initializer names, visibility,
        // runtime and owned String -> IO String export. The compiled function ignores its input.
        // The same guard pins those protected files and their import graph for this worker generation.
        Ok(unsafe {
            AuthorizedWorkerSnapshot::attest(
                runtime,
                path.into(),
                bytes.to_vec(),
                vec![(EXPORT.into(), WorkerExportContract::StringIoString)],
                held.clone(),
            )
        })
    }
    fn host(
        &self,
        runtime: &'static LeanRuntime,
        p: &LeanPreparedHostArtifacts,
    ) -> LeanResult<LoadedHostArtifacts<'static>> {
        let held = self.deployment()?;
        let d = &held.record;
        if p.manifest_path() != d.shim_manifest
            || p.manifest_json() != d.shim_json
            || p.artifact_roots() != d.shim_roots
            || p.shim_search_paths() != d.shim_paths
        {
            audit("refused prepared host descriptor");
            return Err(denied());
        }
        held.check_interop_visibility(true).map_err(ioerr)?;
        audit("verified-global-interop-initializer");
        audit("authorize-host-load");
        // SAFETY: the build-selected SDK supplied its own compiled shim and full generated ABI.
        // Admission checked both manifests, the common interop image and all image bytes
        // under the retained deployment guard. This worker uses the pinned private release.
        unsafe {
            LoadedHostArtifacts::load(
                runtime,
                p.manifest_path().into(),
                p.manifest_json().as_bytes().to_vec(),
                d.shim_roots.clone(),
                d.shim_paths.clone(),
                held.clone(),
            )
        }
    }
    fn imports<'cap>(
        &self,
        cap: &'cap TrustedHostCapability<'static>,
        paths: Vec<String>,
        names: Vec<String>,
        options: HostImportOptions,
    ) -> LeanResult<TrustedHostImports<'static, 'cap>> {
        let held = self.deployment()?;
        if cap.library_path() != held.record.host_image {
            return Err(denied());
        }
        held.record
            .check_order(&paths, &names, &options)
            .map_err(ioerr)?;
        audit("authorize-Callback-import");
        // SAFETY: the checked scope is exactly Callback with the audited profile and ordered
        // deployment roots. The retained guard covers each actual imported file and shadowing
        // prefix. No external inherited search path or plugin participates.
        Ok(unsafe { cap.import_scope_with_options(paths, names, options) })
    }
    fn project(
        &self,
        _: &'static LeanRuntime,
        _: &Path,
        _: &LeanModuleInitializer,
    ) -> LeanResult<(LeanLibrary<'static>, LeanModuleInitializer)> {
        Err(denied())
    }
    fn term<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: String,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedHostTerm<'static, 'cap>> {
        Err(denied())
    }
    fn kernel<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: String,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedHostKernelRequest<'static, 'cap>> {
        Err(denied())
    }
    fn query<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: HostModuleQuery,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostModuleQuery>> {
        Err(denied())
    }
    fn batch<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: HostModuleBatch,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostModuleBatch>> {
        Err(denied())
    }
    fn cached<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: HostCachedModuleBatch,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostCachedModuleBatch>> {
        Err(denied())
    }
    fn proof<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: ProofAttemptRequest,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, ProofAttemptRequest>> {
        Err(denied())
    }
    fn verify<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: DeclarationVerificationRequest,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, DeclarationVerificationRequest>> {
        Err(denied())
    }
    fn verify_batch<'cap>(
        &self,
        _session: WorkerSourceContext<'_, 'cap>,
        _request: DeclarationVerificationBatchRequest,
        _options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, DeclarationVerificationBatchRequest>> {
        Err(denied())
    }
}
fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    let app = MetadataApp(OnceLock::new());
    Ok(lean_rs_worker_child::run_worker_child_with_authority(app)?)
}
