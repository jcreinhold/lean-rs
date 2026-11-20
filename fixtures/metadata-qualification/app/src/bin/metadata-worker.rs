#[path = "../stream_cases.rs"]
mod stream_cases;

use lean_rs::host_bridge::{
    HostCachedModuleBatch, HostModuleBatch, HostModuleQuery, HostSourceOptions, TrustedHostKernelRequest,
    TrustedHostTerm, TrustedModuleSource,
    module_requests::{
        DeclarationVerificationBatchRequest, DeclarationVerificationRequest, DeclarationVerificationTarget,
        ModuleQuery, ModuleQueryOutputBudgets, ModuleQuerySelector, ProofAttemptRequest, ProofEditTarget,
        ProofPositionSelector, SorryPolicy,
    },
};
use lean_rs::module::{
    AuthorizedWorkerSnapshot, HostImportOptions, LeanLibrary, LeanModuleInitializer, LoadedHostArtifacts,
    TrustedHostCapability, TrustedHostImports, TrustedWorkerCapability, WorkerExportContract,
};
use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::host::LeanPreparedHostArtifacts;
use lean_rs_worker_child::{WorkerAuthority, WorkerManifestRead, WorkerSourceContext};
use std::path::Path;

use metadata_qualification::{
    EXPORT, Held, JSON_COMMAND_EXPORT, JSON_MANIFEST_NAME, MODULE_SYNTAX_IMPORTS, Record, STREAM_MANIFEST_NAME, audit,
    module_syntax_case,
};
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
                if record.host_builder_case.as_deref() != option_env!("LEAN_RS_TEST_HOST_BUILDER_CASE")
                    || record.host_builder_case.as_deref()
                        != std::env::var("LEAN_RS_TEST_HOST_BUILDER_CASE").ok().as_deref()
                    || (record.host_builder_case.is_some() && !record.broken_import)
                {
                    return Err("worker and record select different host builder tests".to_owned());
                }
                Held::acquire(record)
                    .and_then(|held| {
                        held.verify()?;
                        if held.record.host_builder_case.is_some() {
                            audit("verified-deployment");
                        }
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

    fn ordinary_verification_record(
        &self,
        session: &WorkerSourceContext<'_, '_>,
        options: &HostSourceOptions,
    ) -> LeanResult<&Record> {
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1") {
            return Err(denied());
        }
        let d = &self.deployment()?.record;
        if d.broken_import
            || !d.typed_session
            || d.stream
            || d.verification
            || !session.imports.matches_request(
                &d.import_roots
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>(),
                &["LeanRsHostShims.Elaboration".to_owned()],
            )
            || *session.imports.options() != HostImportOptions::default()
            || !options.namespace.is_empty()
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || options.options_json != "{}"
        {
            return Err(denied());
        }
        Ok(d)
    }

    fn verify_ordinary<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: DeclarationVerificationRequest,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, DeclarationVerificationRequest>> {
        let d = self.ordinary_verification_record(&session, &options)?;
        if !matches_ordinary_verification(&options.file, &request) {
            return Err(denied());
        }
        // SAFETY: grant_source verified the session and its exact sealed roots.
        // Held pins the artifacts. The request table admits only exact source,
        // target, label, verification policy and budgets in this scope.
        let grant = unsafe {
            session.capability.verification_scope(
                session.environment,
                session.imports,
                d.verification_roots(),
                request,
                options,
            )?
        };
        audit("authorize-fixed-declaration-verification");
        Ok(grant)
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
fn matches_fixed_proof(file: &str, request: &ProofAttemptRequest) -> bool {
    use ProofPositionSelector::{Default as First, Entry, Index};

    const SKIP: &str = "theorem t : True := by\n  skip\n";
    const SUCCESS: &str = "import Lean\n\ntheorem t : True := by\n  skip\n/-- following docstring must remain parseable -/\ntheorem u : True := by\n  trivial\n";
    const ENTRY_STATE: &str =
        "import Lean\n\ntheorem t : ∀ n : Nat, n + 0 = n := by\n  intro n\n  show n + 0 = n\n  exact Nat.add_zero n\n";
    const UNICODE: &str = "theorem unicodeSig (hα : True) : True := by\n  skip\n/-- following docstring must remain parseable -/\ntheorem after : True := by\n  trivial\n";
    const ENTRY_GOAL: &str = "theorem t : ∀ (p : Prop), p → p := by\n  intro p hp\n  exact hp\n";
    const BAD: &str = "exact definitely_missing_identifier";
    const IDS: [&str; 18] = [
        "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12", "c13", "c14", "c15", "c16",
        "c17",
    ];

    let ProofEditTarget::Declaration { name, position } = &request.edit;
    let ordinary = request.budgets == ModuleQueryOutputBudgets::default();
    let one =
        |id, text| matches!(request.candidates.as_slice(), [candidate] if candidate.id == id && candidate.text == text);
    let numbered = |count, alternating| {
        request.candidates.len() == count
            && request
                .candidates
                .iter()
                .zip(IDS)
                .enumerate()
                .all(|(index, (candidate, id))| {
                    candidate.id == id && candidate.text == if alternating && index % 2 == 1 { BAD } else { "trivial" }
                })
    };

    match (file, request.source.as_str(), name.as_str(), position) {
        ("/attempt/success.lean", SUCCESS, "t", First)
        | (
            "/attempt/bad-selector.lean",
            "import Lean\n\ntheorem t : True := by\n  trivial\n",
            "t",
            Index { index: 99 },
        )
        | ("/attempt/single-line.lean", "theorem t : True := by skip\n", "t", First) => {
            ordinary && one("trivial", "trivial")
        }
        ("/attempt/entry-state.lean", ENTRY_STATE, "t", Index { index: 2 }) => {
            ordinary && one("exact", "exact Nat.add_zero n")
        }
        ("/attempt/unicode.lean", UNICODE, "unicodeSig", First) => ordinary && one("bad", BAD),
        (
            "/attempt/bad.lean",
            "theorem t : True := by\n  skip\n/-- following docstring must remain parseable -/\ntheorem u : True := by\n  trivial\n",
            "t",
            First,
        ) => ordinary && one("bad", "exact missingIdentifier"),
        ("/attempt/entry.lean", ENTRY_GOAL, "t", Entry | First) => {
            ordinary && one("from-scratch", "intro p hp; exact hp")
        }
        ("/attempt/cap.lean", SKIP, "t", First) => ordinary && numbered(18, false),
        ("/attempt/ten.lean", SKIP, "t", First) => ordinary && numbered(10, true),
        ("/attempt/budget.lean", SKIP, "t", First) => {
            request.budgets
                == (ModuleQueryOutputBudgets {
                    per_field_bytes: 16,
                    total_bytes: 1,
                })
                && numbered(3, false)
        }
        _ => false,
    }
}
struct FixedBatchPolicy {
    limits: [&'static str; 3],
    audit: bool,
}

fn pool_batch_policy(file: &str, batch: &HostModuleBatch, imports: &[String]) -> Option<FixedBatchPolicy> {
    if file != "<elaborate>"
        || imports != ["LeanRsInteropConsumer.Callback"]
        || batch.source != "theorem poolBatchProbe (h : True) : True := by\n  exact h\n"
    {
        return None;
    }
    use ModuleQuerySelector::{Diagnostics, ProofState};
    let allowed = match std::env::var("LEAN_RS_METADATA_STREAM_CASE").as_deref() {
        Ok("lease_module_query_batch_reuses_one_warm_session") => {
            batch.budgets.per_field_bytes == 8192
                && batch.budgets.total_bytes == 65536
                && matches!(batch.selectors.as_slice(), [Diagnostics { id: diagnostics }, ProofState { id: state, line: 2, column: 4 }] if diagnostics == "diagnostics" && state == "state")
        }
        Ok("lease_module_query_batch_preserves_item_level_budget_failures") => {
            batch.budgets.per_field_bytes == 128
                && batch.budgets.total_bytes == 0
                && matches!(batch.selectors.as_slice(), [ProofState { id: state, line: 2, column: 4 }] if state == "state")
        }
        _ => false,
    };
    allowed.then_some(FixedBatchPolicy {
        limits: ["4", "300000", "67108864"],
        audit: true,
    })
}

fn fixed_batch_policy(file: &str, batch: &HostModuleBatch, imports: &[String]) -> Option<FixedBatchPolicy> {
    use ModuleQuerySelector::{
        DeclarationOutline, DeclarationTarget, Diagnostics, ProofState, ProofStateInDeclaration, References,
    };
    use ProofPositionSelector::{AfterText, Default as First, Entry};

    const BASE: &str = "theorem t (h : True) : True := by\n  exact h\n";
    const CHANGED: &str = "theorem t (h : True) : True := by\n  trivial\n";
    const EXTENDED: &str = "theorem t (h : True) : True := by\n  exact h\n\ntheorem u : 1 = 1 := by\n  rfl\n";
    const SHIFTED: &str = "theorem s : 2 = 2 := by\n  rfl\n\ntheorem t (h : True) : True := by\n  exact h\n";
    const OUTLINE: &str = "namespace WorkerOutline\ntheorem first : True := by\n  trivial\ndef second (n : Nat) : Nat :=\n  n + 1\nend WorkerOutline\n";
    const LOCALS: &str = "theorem t (n : Nat) (h : n + 0 = n) : n + 0 = n := by\n  exact h\n";
    const TRIVIAL: &str = "theorem t : True := by\n  trivial\n";
    const PRISTINE: &str = "theorem t : ∀ (p : Prop), p → p := by\n  intro p hp\n  exact hp\n";
    const INCOMPLETE: &str = "import LeanRsFixture.DoesNotExist\n\ntheorem present : True := totallyUndefinedSymbol\n";
    const REUSE_STATE: &str =
        "theorem reuseProbeDecl : True := trivial\ntheorem reuseProbeSorry : True := by\n  sorry\n";
    const SCOPE_PROBE: &str =
        "namespace P\n\nsection S\n\nopen Nat in\ntheorem t (h : True) : True := by\n  exact h\n\nend S\n\nend P\n";
    const DEFAULT_LIMITS: [&str; 3] = ["4", "300000", "67108864"];

    let source = batch.source.as_str();
    let selectors = batch.selectors.as_slice();
    let ordinary = imports == ["LeanRsHostShims.Elaboration"];
    let wide = imports == ["LeanRsHostShims.Elaboration", "LeanRsFixture.Handles"];
    let exact_imports = match file {
        "/switch/wide.lean" => wide,
        "/capacity-one/cache.lean" => ordinary || wide,
        "/cache/imports.lean" => ordinary || imports == ["LeanRsHostShims.Elaboration", "Lean"],
        _ => ordinary,
    };
    if !exact_imports {
        return None;
    }
    let state_at = |line| matches!(selectors, [ProofState { id, line: at, column: 4 }] if id == "state" && *at == line);
    let missing_tactic = |name| {
        matches!(
            selectors,
            [ProofStateInDeclaration {
                id,
                declaration,
                position: AfterText { text, occurrence: None },
                locals_raw: false,
            }] if id == "state" && declaration == name && text == "missing tactic"
        )
    };
    if batch.budgets != ModuleQueryOutputBudgets::default() {
        return (file == "<elaborate>"
            && source == BASE
            && state_at(2)
            && batch.budgets
                == (ModuleQueryOutputBudgets {
                    per_field_bytes: 128,
                    total_bytes: 0,
                }))
        .then_some(FixedBatchPolicy {
            limits: DEFAULT_LIMITS,
            audit: true,
        });
    }
    let (limits, audit) = match (file, source, selectors) {
        ("/cache/reuse.lean" | "/cache/clear.lean" | "/cache/imports.lean", BASE, _)
        | ("/cache/rebuild.lean", BASE | CHANGED, _)
        | ("/cache/incremental.lean", BASE | EXTENDED, _)
            if state_at(2) =>
        {
            (DEFAULT_LIMITS, false)
        }
        ("/cache/incremental.lean" | "/cache/incremental-cold.lean", SHIFTED, _) if state_at(5) => {
            (DEFAULT_LIMITS, false)
        }
        ("/cache/ttl.lean", BASE, _) if state_at(2) => (["4", "1", "67108864"], false),
        ("/cache/entry-a.lean" | "/cache/entry-b.lean", BASE, _) if state_at(2) => (["1", "300000", "67108864"], false),
        ("/cache/tiny.lean", BASE, _) if state_at(2) => (["4", "300000", "1"], false),

        (
            "/reuse/cache.lean"
            | "/shared/cache.lean"
            | "/switch/narrow.lean"
            | "/switch/wide.lean"
            | "/capacity-one/cache.lean",
            BASE,
            _,
        ) if state_at(2) => (DEFAULT_LIMITS, true),
        ("/reuse/state.lean", REUSE_STATE, _) if state_at(2) => (DEFAULT_LIMITS, true),
        ("/capacity/evicted.lean", SCOPE_PROBE, _) if state_at(7) => (DEFAULT_LIMITS, true),
        (
            "/staleness/probe-first.lean"
            | "/staleness/probe-second.lean"
            | "/staleness/evict.lean"
            | "/staleness/inert.lean"
            | "/staleness/capacity-one-first.lean"
            | "/staleness/capacity-one-second.lean",
            SCOPE_PROBE,
            _,
        ) if state_at(7) && std::env::var_os("LEAN_RS_TEST_STALENESS_PROFILE").is_some_and(|v| v == "1") => {
            (DEFAULT_LIMITS, true)
        }
        (
            "<elaborate>",
            BASE,
            [
                Diagnostics { id: diagnostics },
                ProofState {
                    id: state,
                    line: 2,
                    column: 4,
                },
                DeclarationTarget {
                    id: target,
                    name: Some(name),
                    line: None,
                    column: None,
                },
            ],
        ) if diagnostics == "diagnostics" && state == "state" && target == "target" && name == "t" => {
            (DEFAULT_LIMITS, true)
        }
        ("<elaborate>", OUTLINE, [DeclarationOutline { id }]) if id == "outline" => (DEFAULT_LIMITS, true),
        (
            "<elaborate>",
            LOCALS,
            [
                ProofStateInDeclaration {
                    id,
                    declaration,
                    position: First,
                    locals_raw: _,
                },
            ],
        ) if id == "state" && declaration == "t" => (DEFAULT_LIMITS, true),
        ("<elaborate>", TRIVIAL, _) if missing_tactic("t") => (DEFAULT_LIMITS, true),
        ("<elaborate>", _, _) if missing_tactic("many") && matches_boundary_fixture(source) => (DEFAULT_LIMITS, true),
        (
            "<elaborate>",
            PRISTINE,
            [
                ProofStateInDeclaration {
                    id,
                    declaration,
                    position: Entry,
                    locals_raw: false,
                },
            ],
        ) if id == "entry" && declaration == "t" => (DEFAULT_LIMITS, true),
        ("/scan/incomplete-closure.lean", INCOMPLETE, [Diagnostics { id: diagnostics }, References { id, name }])
            if diagnostics == "diagnostics" && id == "refs" && name == "present" =>
        {
            (DEFAULT_LIMITS, true)
        }
        _ => return None,
    };
    Some(FixedBatchPolicy { limits, audit })
}

fn matches_boundary_fixture(source: &str) -> bool {
    let mut lines = source.split_inclusive('\n');
    lines.next() == Some("theorem many : True := by\n")
        && (0..40).all(|index| {
            let exact = format!("  have h{index} : True := by trivial\n");
            lines.next() == Some(exact.as_str())
        })
        && lines.next() == Some("  trivial\n")
        && lines.next().is_none()
}
fn matches_ordinary_verification(file: &str, request: &DeclarationVerificationRequest) -> bool {
    const SIMILAR: &str = "namespace Probe\ntheorem fooBar : True := by\n  trivial\ntheorem unrelated : True := by\n  trivial\nend Probe\n";
    const PUBLIC_SECTION: &str = "module\n\n@[expose] public section\nnamespace SSet.boundary\n\ntheorem isPushout : True := by\n  trivial\n\nend SSet.boundary\nend\n";
    const AMBIGUOUS: &str = "namespace A\ntheorem dup : True := by trivial\nend A\n\nnamespace B\ntheorem dup : True := by trivial\nend B\n";
    const AMBIGUOUS_OPEN: &str = "namespace AmbigA\ndef collide : Nat := 0\nend AmbigA\n\nnamespace AmbigB\ndef collide : Nat := 1\nend AmbigB\n\nopen AmbigA AmbigB\n\nexample : Nat := collide\n";
    const NEEDS_BUILD: &str = "import LeanRsFixture.DoesNotExist\n\ntheorem present : True := by trivial\n";

    let DeclarationVerificationTarget::Name { name } = &request.target else {
        return false;
    };
    request.sorry_policy == SorryPolicy::Deny
        && request.budgets == ModuleQueryOutputBudgets::default()
        && matches!(
            (file, request.source.as_str(), name.as_str(), request.report_axioms),
            ("/verify/similar.lean", SIMILAR, "Other.fooBar", false)
                | (
                    "/verify/theorem.lean",
                    "theorem t : True := by\n  trivial\n" | "theorem t : True := by\n  sorry\n",
                    "t",
                    true
                )
                | (
                    "/verify/module.lean",
                    PUBLIC_SECTION,
                    "isPushout" | "SSet.boundary.isPushout",
                    true
                )
                | ("/verify/ambiguous.lean", AMBIGUOUS, "dup", true)
                | ("/verify/ambiguous-open.lean", AMBIGUOUS_OPEN, "collide", true)
                | ("/verify/needs-build.lean", NEEDS_BUILD, "notDefinedHere", true)
        )
}
impl WorkerAuthority for MetadataApp {
    fn use_generated_project_imports(
        &self,
        project: &Path,
        capability: &TrustedWorkerCapability<'static>,
    ) -> LeanResult<bool> {
        let held = self.deployment()?;
        let d = &held.record;
        Ok(d.stream && project == d.project && capability.library_path() == d.consumer_image)
    }
    fn manifest_read(&self, path: &Path) -> LeanResult<WorkerManifestRead> {
        let held = self.deployment()?;
        let d = &held.record;
        if d.broken_import || path != d.consumer_manifest {
            return Err(denied());
        }
        let root = if d.typed_session {
            if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
                || path.file_name().and_then(|name| name.to_str())
                    != Some(if d.stream {
                        STREAM_MANIFEST_NAME
                    } else {
                        JSON_MANIFEST_NAME
                    })
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
        let contracts = if d.stream {
            let requested = std::env::var("LEAN_RS_METADATA_STREAM_CASE").map_err(|_| denied())?;
            let (_, exports) = stream_cases::CASES
                .iter()
                .find(|(names, _)| names.contains(&requested.as_str()))
                .ok_or_else(denied)?;
            let manifest: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| denied())?;
            let signatures = manifest
                .get("exports")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(denied)?;
            let event = match exports {
                stream_cases::Exports::Metadata => "authorize-fixed-metadata",
                stream_cases::Exports::Doctor | stream_cases::Exports::MalformedMetadataAndDoctor => {
                    "authorize-fixed-doctor"
                }
                stream_cases::Exports::NoExports => "admit-fixed-no-exports",
                stream_cases::Exports::Json | stream_cases::Exports::MalformedJson => "authorize-fixed-json",
                stream_cases::Exports::Shape(_, _) => "authorize-fixed-shape",
                stream_cases::Exports::Streams(_) | stream_cases::Exports::StreamAndJson(_) => "authorize-fixed-stream",
            };
            let mut contracts = Vec::new();
            for (signature, streaming) in exports.signatures() {
                if !signatures.contains(&signature.to_json()) {
                    return Err(denied());
                }
                let contract = if streaming {
                    WorkerExportContract::StringStream
                } else {
                    WorkerExportContract::StringIoString
                };
                contracts.push((signature.symbol().to_owned(), contract));
            }
            // With NoExports this records image admission only; the attested table is empty.
            audit(&format!("{event}-{requested}"));
            contracts
        } else {
            let export = if d.typed_session {
                if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1") {
                    return Err(denied());
                }
                JSON_COMMAND_EXPORT
            } else {
                EXPORT
            };
            audit("authorize-consumer-load");
            vec![(export.into(), WorkerExportContract::StringIoString)]
        };
        // SAFETY: Held checks the exact sealed images, initializer and runtime.
        // Each named interop case selects only its exact checked contracts,
        // or no contracts for the missing-export case.
        Ok(unsafe { AuthorizedWorkerSnapshot::attest(runtime, path.into(), bytes.to_vec(), contracts, held.clone()) })
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
        if !d.broken_import && (!d.typed_session || d.stream) {
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
        if let Some(event) = held.record.host_builder_import_event(&names) {
            audit(event);
        } else if held.record.adapter_handles_case() {
            audit("authorize-adapter-handles");
        } else if held.record.adapter_kernel_case() {
            audit("authorize-adapter-elaboration");
        } else if !held.record.broken_import {
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
            || held.record.stream
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
        let adapter = d.adapter_kernel_case()
            && matches!(request.as_str(), "(1 + 1 : Nat)" | "1 +")
            && session
                .imports
                .matches_request(&paths, &["LeanRsHostShims.Elaboration".to_owned()]);
        let meta = !d.adapter_kernel_case()
            && matches!(request.as_str(), "(1 + 1 : Nat)" | "(2 : Nat)" | "(Nat.succ 0 : Nat)")
            && session
                .imports
                .matches_request(&paths, &["LeanRsHostShims.Elaboration".to_owned()]);
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
            || d.broken_import
            || !d.typed_session
            || d.stream
            || d.verification
            || !(recovery || meta || adapter)
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
        session: WorkerSourceContext<'_, 'cap>,
        request: String,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedHostKernelRequest<'static, 'cap>> {
        let d = &self.deployment()?.record;
        let paths: Vec<_> = d.import_roots.iter().map(|path| path.display().to_string()).collect();
        if !d.adapter_kernel_case()
            || !matches!(
                request.as_str(),
                "theorem lean_rs_worker_checked : 1 + 1 = 2 := rfl"
                    | "theorem lean_rs_worker_rejected : 1 + 1 = 3 := rfl"
            )
            || !session
                .imports
                .matches_request(&paths, &["LeanRsHostShims.Elaboration".to_owned()])
            || *session.imports.options() != HostImportOptions::default()
            || !options.namespace.is_empty()
            || options.file != "<elaborate>"
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || options.options_json != "{}"
        {
            return Err(denied());
        }
        // SAFETY: Both fixed declarations and default options are reviewed
        // against the exact Elaboration import and its retained deployment.
        unsafe {
            session.capability.kernel_scope(
                session.environment,
                session.imports,
                d.verification_roots(),
                request,
                options,
            )
        }
    }
    fn query<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: HostModuleQuery,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, HostModuleQuery>> {
        let d = &self.deployment()?.record;
        let exact_request = match (request.source.as_str(), &request.query, options.file.as_str()) {
            ("import Lean\n\ntheorem ok : True := by trivial\n", ModuleQuery::Diagnostics, "<elaborate>")
            | ("theorem u : True := by\n  trivial\n", ModuleQuery::Diagnostics, "<elaborate>")
            | (
                "theorem bad : True := by\n  exact missingIdentifier\n",
                ModuleQuery::Diagnostics,
                "/diagnostics/original.lean",
            )
            | (
                "def x := 1\ntheorem t : x = 1 := by rfl\n",
                ModuleQuery::TypeAt { line: 2, column: 13 },
                "<elaborate>",
            )
            | ("theorem t : True := by\n  trivial\n", ModuleQuery::GoalAt { line: 2, column: 4 }, "<elaborate>") => {
                true
            }
            ("def x := 1\n#check x\n", ModuleQuery::References { name }, "<elaborate>") => name == "x",
            _ => false,
        };
        const MODULE_SYNTAX_SOURCE: &str = concat!(
            "module\n\n",
            "public import Fixture.Imported\n",
            "import all Fixture.Internal\n",
            "import Fixture.PrivateScope\n\n",
            "def moduleSyntaxFoo : Nat := imported + internalSecret\n",
        );
        let module_syntax = module_syntax_case()
            && matches!(
                (request.source.as_str(), &request.query, options.file.as_str()),
                (MODULE_SYNTAX_SOURCE, ModuleQuery::Diagnostics, "<elaborate>")
            );
        let exact_imports: &[&str] = if module_syntax {
            &MODULE_SYNTAX_IMPORTS
        } else {
            &["LeanRsHostShims.Elaboration"]
        };
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
            || d.broken_import
            || !d.typed_session
            || d.stream
            || d.verification
            || !(exact_request || module_syntax)
            || !session.imports.matches_request(
                &d.import_roots
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>(),
                &exact_imports.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>(),
            )
            || *session.imports.options() != HostImportOptions::default()
            || !options.namespace.is_empty()
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || options.options_json != "{}"
        {
            return Err(denied());
        }
        // SAFETY: grant_source checked the complete search path and ruled out plugins.
        // Held retains the sealed images and imports. Each exact query above is tied
        // to its own fixed import scope, with unchanged resource limits.
        let grant = unsafe {
            session.capability.module_query_scope(
                session.environment,
                session.imports,
                d.verification_roots(),
                request,
                options,
            )?
        };
        audit("authorize-fixed-module-query");
        Ok(grant)
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
        let d = &self.deployment()?.record;
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
            || d.broken_import
            || !d.typed_session
            || d.verification
            || !options.namespace.is_empty()
            || options.options_json != "{}"
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || *session.imports.options() != HostImportOptions::default()
        {
            return Err(denied());
        }
        let names = session.imports.imports();
        let fixed = if d.stream {
            pool_batch_policy(&options.file, &request.batch, names)
        } else {
            fixed_batch_policy(&options.file, &request.batch, names)
        }
        .ok_or_else(denied)?;
        let [entries, ttl, bytes] = fixed.limits;
        let paths = d
            .import_roots
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        if !session.imports.matches_request(&paths, names) {
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
        if *file != options.file
            || key.len() != 64
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || *max_entries != entries
            || *ttl_millis != ttl
            || *max_bytes != bytes
        {
            return Err(denied());
        }
        // SAFETY: grant_source checked the actual retained capability, environment
        // and complete observed roots. Held protects all sealed imports and images.
        // Only the fixed sources, projections and limits above enter this frontend.
        let grant = unsafe {
            session.capability.cached_module_batch_scope(
                session.environment,
                session.imports,
                d.verification_roots(),
                request,
                options,
            )?
        };
        if fixed.audit {
            audit("authorize-fixed-module-batch");
        }
        Ok(grant)
    }
    fn proof<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: ProofAttemptRequest,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, ProofAttemptRequest>> {
        let d = &self.deployment()?.record;
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") != Some("1")
            || d.broken_import
            || !d.typed_session
            || d.stream
            || d.verification
            || !session.imports.matches_request(
                &d.import_roots
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>(),
                &["LeanRsHostShims.Elaboration".to_owned()],
            )
            || *session.imports.options() != HostImportOptions::default()
            || !options.namespace.is_empty()
            || options.heartbeats != lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT
            || options.diagnostic_bytes != lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT
            || options.options_json != "{}"
            || !matches_fixed_proof(&options.file, &request)
        {
            return Err(denied());
        }
        // SAFETY: grant_source checked the exact search path and ruled out plugins.
        // Held pins the sealed images and imports. The checker admits only the fixed
        // in-memory source, targets, candidate order and budgets in the table.
        let grant = unsafe {
            session.capability.proof_attempt_scope(
                session.environment,
                session.imports,
                d.verification_roots(),
                request,
                options,
            )?
        };
        audit("authorize-fixed-proof-attempt");
        Ok(grant)
    }
    fn validate_source_context(
        &self,
        context: &WorkerSourceContext<'_, '_>,
        observed: Option<&[String]>,
    ) -> LeanResult<()> {
        let d = &self.deployment()?.record;
        let roots = d.verification_roots();
        if d.broken_import
            || !d.typed_session
            || (d.stream
                && !matches!(
                    std::env::var("LEAN_RS_METADATA_STREAM_CASE").as_deref(),
                    Ok("lease_module_query_batch_reuses_one_warm_session"
                        | "lease_module_query_batch_preserves_item_level_budget_failures")
                ))
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
        if option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") == Some("1") {
            return self.verify_ordinary(session, request, options);
        }
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
        let event = match (request.source.as_str(), name.as_str(), request.budgets.per_field_bytes) {
            ("theorem t : True := by\n  trivial\n", "t", n) if n == ordinary => "authorize-verify-proved",
            ("theorem t : True := by\n  sorry\n", "t", n) if n == ordinary => "authorize-verify-sorry",
            ("theorem absent : Nat := 0\n", "absent", n) if n == ordinary => "authorize-verify-unavailable",
            ("theorem t : True := by\n  sorry\n", "t", 1) => "authorize-verify-truncated",
            _ => return Err(denied()),
        };
        let roots = self.deployment()?.record.verification_roots();
        // SAFETY: grant_source validated this opened environment's exact import order immediately
        // before calling verify. Held retains the sealed libraries, modules and release throughout.
        // Only the four reviewed in-memory declarations and their exact options reach the shim.
        let grant = unsafe {
            session
                .capability
                .verification_scope(session.environment, session.imports, roots, request, options)?
        };
        audit(event);
        Ok(grant)
    }
    fn verify_batch<'cap>(
        &self,
        session: WorkerSourceContext<'_, 'cap>,
        request: DeclarationVerificationBatchRequest,
        options: HostSourceOptions,
    ) -> LeanResult<TrustedModuleSource<'static, 'cap, DeclarationVerificationBatchRequest>> {
        let d = self.ordinary_verification_record(&session, &options)?;
        let [closed, sorry] = request.targets.as_slice() else {
            return Err(denied());
        };
        if options.file != "/verify/batch.lean"
            || request.source != "theorem closed : True := by\n  trivial\ntheorem withSorry : True := by\n  sorry\n"
            || request.sorry_policy != SorryPolicy::Deny
            || !request.report_axioms
            || request.budgets != ModuleQueryOutputBudgets::default()
            || closed.id != "closed-row"
            || sorry.id != "sorry-row"
            || !matches!(&closed.target, DeclarationVerificationTarget::Name { name } if name == "closed")
            || !matches!(&sorry.target, DeclarationVerificationTarget::Name { name } if name == "withSorry")
        {
            return Err(denied());
        }
        // SAFETY: grant_source verified the session and its exact sealed roots.
        // Held pins the artifacts. This is one checked batch with its exact
        // ordered targets, source, file label, verification policy and budgets.
        let grant = unsafe {
            session.capability.verification_batch_scope(
                session.environment,
                session.imports,
                d.verification_roots(),
                request,
                options,
            )?
        };
        audit("authorize-fixed-verification-batch");
        Ok(grant)
    }
}
fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    // A policy restart can shut down before the replacement receives any request.
    if (option_env!("LEAN_RS_METADATA_ORDINARY_TYPED") == Some("1")
        || (broken_import_profile() && option_env!("LEAN_RS_TEST_HOST_BUILDER_CASE").is_some()))
        && std::env::var_os("LEAN_RS_TEST_AUDIT_WORKER_START").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        audit("worker-started");
    }
    let app = MetadataApp(OnceLock::new());
    Ok(lean_rs_worker_child::run_worker_child_with_authority(app)?)
}
