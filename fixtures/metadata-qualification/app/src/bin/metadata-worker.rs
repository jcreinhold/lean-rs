use lean_rs::host_bridge::{
    HostCachedModuleBatch, HostModuleBatch, HostModuleQuery, HostSourceOptions,
    TrustedHostKernelRequest, TrustedHostTerm, TrustedModuleSource,
    module_requests::{
        DeclarationVerificationBatchRequest, DeclarationVerificationRequest,
        DeclarationVerificationTarget, ModuleQueryOutputBudgets, ModuleQuerySelector, ProofAttemptRequest, SorryPolicy,
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

use metadata_qualification::{EXPORT, Held, JSON_COMMAND_EXPORT, JSON_MANIFEST_NAME, Record, audit};
use std::sync::{Arc, OnceLock};
struct MetadataApp(OnceLock<Result<Arc<Held>, String>>);
impl MetadataApp {
    fn deployment(&self) -> LeanResult<&Arc<Held>> {
        self.0
            .get_or_init(|| {
                let record = Record::selected();
                if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") == Some("1")
                    && (record.broken_import || !record.typed_session || record.verification)
                {
                    return Err("ordinary typed worker requires its fixed typed fixture profile".to_owned());
                }
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
        if d.broken_import || path != d.consumer_manifest {
            return Err(denied());
        }
        let root = if d.typed_session {
            if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
                || path.file_name().and_then(|name| name.to_str()) != Some(JSON_MANIFEST_NAME)
            {
                return Err(denied());
            }
            path.parent().ok_or_else(denied)?
        } else {
            &d.project
        };
        Ok(WorkerManifestRead::new(root.into(), path.into(), held.clone()))
    }
    fn worker(
        &self,
        runtime: &'static LeanRuntime,
        path: &Path,
        bytes: &[u8],
    ) -> LeanResult<AuthorizedWorkerSnapshot<'static>> {
        let held = self.deployment()?;
        let d = &held.record;
        if d.broken_import || path != d.consumer_manifest || bytes != d.consumer_json.as_bytes() {
            return Err(denied());
        }
        let export = if d.typed_session {
            if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1") {
                return Err(denied());
            }
            JSON_COMMAND_EXPORT
        } else {
            EXPORT
        };
        audit("authorize-consumer-load");
        // SAFETY: the seal and Held verified the exact images, initializer, runtime
        // and the one named owned String -> IO String export, which ignores its input.
        // Held pins the images, dependencies and imports for the worker generation.
        Ok(unsafe {
            AuthorizedWorkerSnapshot::attest(
                runtime,
                path.into(),
                bytes.to_vec(),
                vec![(export.into(), WorkerExportContract::StringIoString)],
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
                if names == ["LeanRsFixture.Handles", "LeanRsFixture.SourceRanges"] {
                    "authorize-Handles-SourceRanges-import"
                } else {
                    "authorize-checked-typed-import"
                }
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
        session: WorkerSourceContext<'_, 'cap>,
        request: String,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedHostTerm<'static, 'cap>> {
        let d = &self.deployment()?.record;
        let paths: Vec<_> = d.import_roots.iter().map(|path| path.display().to_string()).collect();
        let recovery = request == "(Nat.succ 0 : Nat)"
            && session.imports.matches_request(
                &paths,
                &[
                    "LeanRsFixture.Handles".to_owned(),
                    "LeanRsFixture.SourceRanges".to_owned(),
                ],
            );
        let meta = matches!(request.as_str(), "(1 + 1 : Nat)" | "(2 : Nat)")
            && session
                .imports
                .matches_request(&paths, &["LeanRsHostShims.Elaboration".to_owned()]);
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
            || d.broken_import
            || !d.typed_session
            || d.verification
            || !(recovery || meta)
            || *session.imports.options() != HostImportOptions::default()
            || !options.namespace.is_empty()
            || options.file != "<elaborate>"
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || options.options_json != "{}"
        {
            return Err(denied());
        }
        // SAFETY: grant_source checked the complete search path and plugin absence. Held
        // retains the sealed images and imports. Only the named terms can enter their
        // exact import scopes, with default options and no plugin.
        let grant = unsafe {
            session.capability.term_scope(
                session.environment,
                session.imports,
                d.verification_roots(),
                request,
                None,
                options,
            )?
        };
        if meta {
            audit("authorize-fixed-meta-term");
        }
        Ok(grant)
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
        session: WorkerSourceContext<'_, 'cap>,
        request: HostCachedModuleBatch,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostCachedModuleBatch>> {
        const BASE: &str = "theorem t (h : True) : True := by\n  exact h\n";
        const CHANGED: &str = "theorem t (h : True) : True := by\n  trivial\n";
        const EXTENDED: &str = "theorem t (h : True) : True := by\n  exact h\n\ntheorem u : 1 = 1 := by\n  rfl\n";
        const SHIFTED: &str = "theorem s : 2 = 2 := by\n  rfl\n\ntheorem t (h : True) : True := by\n  exact h\n";
        let d = &self.deployment()?.record;
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
            || d.broken_import || !d.typed_session || d.verification
            || !options.namespace.is_empty() || options.options_json != "{}"
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || request.batch.budgets != ModuleQueryOutputBudgets::default()
            || *session.imports.options() != HostImportOptions::default()
        {
            return Err(denied());
        }
        let source = request.batch.source.as_str();
        let (line, entries, ttl, bytes) = match (options.file.as_str(), source) {
            ("/cache/reuse.lean" | "/cache/clear.lean" | "/cache/imports.lean", BASE)
                | ("/cache/rebuild.lean", BASE | CHANGED)
                | ("/cache/incremental.lean", BASE | EXTENDED) => (2, "4", "300000", "67108864"),
            ("/cache/incremental.lean" | "/cache/incremental-cold.lean", SHIFTED) => (5, "4", "300000", "67108864"),
            ("/cache/ttl.lean", BASE) => (2, "4", "1", "67108864"),
            ("/cache/entry-a.lean" | "/cache/entry-b.lean", BASE) => (2, "1", "300000", "67108864"),
            ("/cache/tiny.lean", BASE) => (2, "4", "300000", "1"),
            _ => return Err(denied()),
        };
        let names = session.imports.imports();
        let ordinary = names == ["LeanRsHostShims.Elaboration"];
        let changed = options.file == "/cache/imports.lean"
            && names == ["LeanRsHostShims.Elaboration", "Lean"];
        let paths = d.import_roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>();
        if (!ordinary && !changed) || !session.imports.matches_request(&paths, names)
            || request.batch.selectors != [ModuleQuerySelector::ProofState {
                id: "state".to_owned(), line, column: 4,
            }]
        {
            return Err(denied());
        }
        // The trusted SDK computes the cache key; it is not an artifact grant.
        // Bound its wire shape and each case's actual cache limits without copying
        // the production key algorithm that these tests exercise.
        if request.policy.len() > 256 {
            return Err(denied());
        }
        let fields = request.policy.split('\n').collect::<Vec<_>>();
        let [file, key, max_entries, ttl_millis, max_bytes] = fields.as_slice() else {
            return Err(denied());
        };
        if *file != options.file || key.len() != 64
            || !key.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || *max_entries != entries || *ttl_millis != ttl || *max_bytes != bytes
        {
            return Err(denied());
        }
        // SAFETY: grant_source checked the actual retained capability, environment
        // and complete observed roots. Held protects all sealed imports and images.
        // Only the fixed sources, projections and limits above enter this frontend.
        unsafe {
            session.capability.cached_module_batch_scope(
                session.environment, session.imports, d.verification_roots(), request, options,
            )
        }
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
            || (!d.verification && option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1"))
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
        if broken_import_profile() || !self.deployment()?.record.verification {
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
    // A policy restart can shut down before the replacement receives any request.
    if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") == Some("1")
        && std::env::var_os("LEAN_RS_TEST_AUDIT_WORKER_START").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        audit("worker-started");
    }
    let app = MetadataApp(OnceLock::new());
    Ok(lean_rs_worker_child::run_worker_child_with_authority(app)?)
}
