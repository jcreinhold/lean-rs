use lean_rs::host_bridge::{
    HostCachedModuleBatch, HostModuleBatch, HostModuleQuery, HostSourceOptions,
    TrustedHostKernelRequest, TrustedHostTerm, TrustedModuleSource,
    module_requests::{
        DeclarationVerificationBatchRequest, DeclarationVerificationRequest,
        DeclarationVerificationTarget, ModuleQueryOutputBudgets, ProofAttemptRequest, SorryPolicy,
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
                let record = Record::selected();
                if record.broken_import != broken_import_profile()
                    || (record.broken_import && (record.typed_session || record.verification))
                {
                    return Err("worker and record select different fixture profiles".to_owned());
                }
                Held::acquire(record)
                    .and_then(|held| {
                        held.verify()?;
                        Ok(Arc::new(held))
                    })
                    .map_err(|error| {
                        if !broken_import_profile() {
                            audit(&format!("refused {error}"));
                        }
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
fn broken_import_profile() -> bool {
    option_env!("LEAN_RS_METADATA_BROKEN_IMPORT") == Some("1")
}
fn ioerr(e: std::io::Error) -> lean_rs::LeanError {
    if !broken_import_profile() {
        audit(&format!("refused {e}"));
    }
    lean_rs::error::host_internal(e.to_string())
}
impl WorkerAuthority for MetadataApp {
    fn manifest_read(&self, path: &Path) -> LeanResult<WorkerManifestRead> {
        let held = self.deployment()?;
        let d = &held.record;
        if d.broken_import || d.typed_session || path != d.consumer_manifest {
            return Err(denied());
        }
        Ok(WorkerManifestRead::new(d.project.clone(), path.into(), held.clone()))
    }
    fn worker(
        &self,
        runtime: &'static LeanRuntime,
        path: &Path,
        bytes: &[u8],
    ) -> LeanResult<AuthorizedWorkerSnapshot<'static>> {
        let held = self.deployment()?;
        let d = &held.record;
        if d.broken_import || d.typed_session || path != d.consumer_manifest || bytes != d.consumer_json.as_bytes() {
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
            if !d.broken_import {
                audit("refused prepared host descriptor");
            }
            return Err(denied());
        }
        if !d.broken_import && !d.typed_session {
            held.check_interop_visibility(true).map_err(ioerr)?;
        }
        if !d.broken_import {
            audit("authorize-host-load");
        }
        // SAFETY: the build-selected SDK supplied its own compiled shim and full generated ABI.
        // Admission checked both manifests, the common interop image and all image bytes
        // under the retained deployment guard. This worker uses the pinned private release.
        let shim = unsafe {
            LoadedHostArtifacts::load(
                runtime,
                p.manifest_path().into(),
                p.manifest_json().as_bytes().to_vec(),
                d.shim_roots.clone(),
                d.shim_paths.clone(),
                held.clone(),
            )?
        };
        held.check_interop_visibility(true).map_err(ioerr)?;
        if !d.broken_import {
            audit("verified-global-interop-initializer");
        }
        Ok(shim)
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
        held.record.check_order(&paths, &names, &options).map_err(ioerr)?;
        if !held.record.broken_import {
            audit(if held.record.typed_session {
                "authorize-Handles-SourceRanges-import"
            } else {
                "authorize-Callback-import"
            });
        }
        // SAFETY: the checked scope has the sealed fixture’s exact names, audited profile and ordered
        // deployment roots. The retained guard covers each actual imported file and shadowing
        // prefix. No external inherited search path or plugin participates.
        Ok(unsafe { cap.import_scope_with_options(paths, names, options) })
    }
    fn project(
        &self,
        runtime: &'static LeanRuntime,
        path: &Path,
        requested: &LeanModuleInitializer,
    ) -> LeanResult<(LeanLibrary<'static>, LeanModuleInitializer)> {
        let held = self.deployment()?;
        let expected = LeanModuleInitializer::new("lean_rs_fixture", "LeanRsFixture");
        if held.record.broken_import
            || !held.record.typed_session
            || path != held.record.consumer_image
            || requested != &expected
        {
            return Err(denied());
        }
        audit("authorize-LeanRsFixture-load");
        // SAFETY: Held verified the reviewed release, fixture image, linked dependencies and
        // roll-up initializer under the deployment locks retained for the worker lifetime.
        let library = unsafe { LeanLibrary::open(runtime, path) }?;
        Ok((library, expected))
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
    fn validate_source_context(
        &self,
        context: &WorkerSourceContext<'_, '_>,
        observed: Option<&[String]>,
    ) -> LeanResult<()> {
        let d = &self.deployment()?.record;
        let roots = d.verification_roots();
        if d.broken_import || !d.typed_session
            || !d.verification
            || context.capability.library_path() != d.host_image
            || observed != Some(roots.as_slice())
            || context.imports.plugin().is_some()
        {
            return Err(denied());
        }
        // This app minted the import scope only for the sealed names, order and profile.
        context.imports.check_capability(context.capability)
    }
    fn verify<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: DeclarationVerificationRequest,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, DeclarationVerificationRequest>> {
        if broken_import_profile() {
            return Err(denied());
        }
        if options.file != "/verify/theorem.lean"
            || !options.namespace.is_empty()
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || options.options_json != "{}"
        {
            audit("refused-verification-options");
            return Err(lean_rs::error::host_internal(
                "verification fixture refused source options",
            ));
        }
        let DeclarationVerificationTarget::Name { name } = &request.target else {
            return Err(denied());
        };
        let defaults = ModuleQueryOutputBudgets::default();
        if request.sorry_policy != SorryPolicy::Deny
            || !request.report_axioms
            || request.budgets.total_bytes != defaults.total_bytes
        {
            return Err(denied());
        }
        let ordinary = defaults.per_field_bytes;
        let event = match (
            request.source.as_str(),
            name.as_str(),
            request.budgets.per_field_bytes,
        ) {
            ("theorem t : True := by\n  trivial\n", "t", n) if n == ordinary => {
                "authorize-verify-proved"
            }
            ("theorem t : True := by\n  sorry\n", "t", n) if n == ordinary => {
                "authorize-verify-sorry"
            }
            ("theorem absent : Nat := 0\n", "absent", n) if n == ordinary => {
                "authorize-verify-unavailable"
            }
            ("theorem t : True := by\n  sorry\n", "t", 1) => "authorize-verify-truncated",
            _ => return Err(denied()),
        };
        let roots = self.deployment()?.record.verification_roots();
        // SAFETY: grant_source validated this opened environment's exact import order immediately
        // before calling verify. Held retains the sealed libraries, modules and release throughout.
        // Only the four reviewed in-memory declarations and their exact options reach the shim.
        let grant = unsafe {
            session.capability.verification_scope(
                session.environment,
                session.imports,
                roots,
                request,
                options,
            )?
        };
        audit(event);
        Ok(grant)
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
