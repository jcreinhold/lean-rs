#[path = "../support/deployment.rs"]
mod deployment;
use deployment::Deployment;
use lean_rs::host_bridge::{
    HostCachedModuleBatch, HostModuleBatch, HostModuleQuery, HostSourceOptions, TrustedHostKernelRequest,
    TrustedHostTerm, TrustedModuleSource,
    module_requests::{DeclarationVerificationBatchRequest, DeclarationVerificationRequest, ProofAttemptRequest},
};
use lean_rs::module::{
    AuthorizedWorkerSnapshot, HostImportOptions, LeanLibrary, LeanModuleInitializer, LoadedHostArtifacts,
    TrustedHostCapability, TrustedHostImports, WorkerExportContract,
};
use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::host::LeanPreparedHostArtifacts;
use lean_rs_worker_child::{WorkerAuthority, WorkerManifestRead, WorkerSourceContext};
use std::path::{Path, PathBuf};

struct Demo(Deployment);
fn denied() -> lean_rs::LeanError {
    lean_rs::error::host_internal("outside the build-selected ShipLeanDemo deployment")
}
fn ioerr(e: std::io::Error) -> lean_rs::LeanError {
    lean_rs::error::host_internal(e.to_string())
}
const MANIFEST: &str = env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_MANIFEST");
const BYTES: &[u8] = include_bytes!(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_MANIFEST"));
impl WorkerAuthority for Demo {
    fn manifest_read(&self, path: &Path) -> LeanResult<WorkerManifestRead> {
        if path != Path::new(MANIFEST) {
            return Err(denied());
        }
        let readers = deployment::hold(std::slice::from_ref(&self.0.project)).map_err(ioerr)?;
        Ok(WorkerManifestRead::new(self.0.project.clone(), path.into(), readers))
    }
    fn worker(
        &self,
        runtime: &'static LeanRuntime,
        path: &Path,
        bytes: &[u8],
    ) -> LeanResult<AuthorizedWorkerSnapshot<'static>> {
        let readers = deployment::hold(std::slice::from_ref(&self.0.project)).map_err(ioerr)?;
        let image = Path::new(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_DYLIB"));
        if path != Path::new(MANIFEST)
            || bytes != BYTES
            || !deployment::matches_demo(&serde_json::from_slice(bytes).map_err(|_| denied())?, image)
            || image.canonicalize().map_err(ioerr)? != image
            || !image.starts_with(&self.0.project)
            || std::fs::read(image).map_err(ioerr)?
                != include_bytes!(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_DYLIB")).as_slice()
        {
            return Err(denied());
        }
        // SAFETY: the captured manifest and image equal this package's build-selected bytes.
        // A retained reader excludes cooperating builds. The deployed matching Lean runtime
        // and its loader dependencies are trusted and stable for the process lifetime.
        Ok(unsafe {
            AuthorizedWorkerSnapshot::attest(
                runtime,
                path.into(),
                bytes.to_vec(),
                vec![("ship_lean_demo_add".into(), WorkerExportContract::U64Binary)],
                readers,
            )
        })
    }
    fn host(
        &self,
        runtime: &'static LeanRuntime,
        p: &LeanPreparedHostArtifacts,
    ) -> LeanResult<LoadedHostArtifacts<'static>> {
        let mut roots = self.0.shim_roots.clone();
        roots.push(self.0.project.clone());
        let readers = deployment::hold(&roots).map_err(ioerr)?;
        if p.manifest_path() != self.0.shim_manifest
            || p.artifact_roots() != self.0.shim_roots
            || p.shim_search_paths() != self.0.shim_paths
            || std::fs::read(p.manifest_path()).map_err(ioerr)? != p.manifest_json().as_bytes()
            || self.0.inventory().map_err(ioerr)? != self.0.files
        {
            return Err(denied());
        }
        // SAFETY: the bundled host and interop images and manifest match the SDK build
        // recorded by this application. The readers retain their checked resolution.
        Ok(unsafe {
            LoadedHostArtifacts::load(
                runtime,
                p.manifest_path().into(),
                p.manifest_json().as_bytes().to_vec(),
                self.0.shim_roots.clone(),
                self.0.shim_paths.clone(),
                readers,
            )
        }?)
    }
    fn imports<'cap>(
        &self,
        cap: &'cap TrustedHostCapability<'static>,
        paths: Vec<String>,
        names: Vec<String>,
        options: HostImportOptions,
    ) -> LeanResult<TrustedHostImports<'static, 'cap>> {
        let expected: Vec<_> = std::iter::once(self.0.project.join(".lake/build/lib/lean"))
            .chain(self.0.shim_paths.clone())
            .collect();
        if paths.iter().map(PathBuf::from).ne(expected)
            || names != ["ShipLeanDemo"]
            || options != HostImportOptions::default()
            || std::env::var_os("LEAN_PATH").is_some()
            || std::env::var_os("LEAN_SYSROOT")
                .and_then(|p| PathBuf::from(p).canonicalize().ok())
                .as_ref()
                != Some(&self.0.sysroot)
        {
            return Err(denied());
        }
        // SAFETY: host() retains the read union and checks the packaged module bytes,
        // canonical search order and absence of Init shadows. ShipLeanDemo imports only
        // the builtin prelude supplied by the trusted, stable build-selected toolchain.
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
    let record = serde_json::from_slice(include_bytes!(env!("SHIP_DEMO_DEPLOYMENT")))?;
    Ok(lean_rs_worker_child::run_worker_child_with_authority(Demo(record))?)
}
