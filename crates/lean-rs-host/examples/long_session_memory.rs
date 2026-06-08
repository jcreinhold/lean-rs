//! RSS-shaped long-session memory reproducer.
//!
//! Run with:
//!
//! ```sh
//! LEAN_RS_NUM_THREADS=1 cargo run --release -p lean-rs-host --example long_session_memory
//! ```
//!
//! This is intentionally an example, not a Criterion bench. The
//! question is retained resident set size across lifetime boundaries
//! (`LeanRuntime`, module initializers, `SessionPool`, `LeanSession`,
//! and `Obj<'lean>` drops), not per-iteration latency.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, clippy::print_stdout)]

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use lean_rs::{LeanDiagnosticCode, LeanError, LeanResult, LeanRuntime};
use lean_rs_host::host::process::{
    ModuleQueryOutputBudgets, ModuleQuerySelector, ProofAttemptRequest, ProofCandidate, ProofEditTarget,
    ProofPositionSelector, SorryPolicy,
};
use lean_rs_host::{
    DeclarationInspectionFields, DeclarationInspectionRequest, DeclarationInspectionResult, DeclarationSearchRequest,
    DeclarationVerificationRequest, DeclarationVerificationTarget, LeanBracketedImportRequest, LeanCapabilities,
    LeanDerivedWorkFacts, LeanElabOptions, LeanHost, LeanImportProfileMode, LeanImportProfilerOptions, LeanImportStats,
    LeanProgressEvent, LeanProgressSink, LeanSessionImportProfile, PoolStats, SessionPool, SessionPoolMemoryPolicy,
};
use lean_toolchain::LEAN_VERSION;

const DEFAULT_IMPORTS: usize = 4;
const DEFAULT_BULK: usize = 64;
const DEFAULT_ELAB: usize = 64;
const DEFAULT_POOL_CAPACITY: usize = 1;
const DEFAULT_CHECKPOINT_EVERY: usize = 1;
const STEADY_STATE_PAUSE_MS: u64 = 2_000;

const IMPORTS: [&str; 1] = ["LeanRsFixture.Handles"];
const MIXED_IMPORTS: [&str; 2] = ["LeanRsFixture.Handles", "LeanRsHostShims.Elaboration"];
const BULK_NAMES: [&str; 16] = [
    "LeanRsFixture.Handles.nameAnonymous",
    "LeanRsFixture.Handles.nameMkStr",
    "LeanRsFixture.Handles.nameMkNum",
    "LeanRsFixture.Handles.nameToString",
    "LeanRsFixture.Handles.nameBeq",
    "LeanRsFixture.Handles.levelZero",
    "LeanRsFixture.Handles.levelSucc",
    "LeanRsFixture.Handles.exprConstNat",
    "LeanRsFixture.Handles.nameAnonymous",
    "LeanRsFixture.Handles.nameMkStr",
    "LeanRsFixture.Handles.nameMkNum",
    "LeanRsFixture.Handles.nameToString",
    "LeanRsFixture.Handles.nameBeq",
    "LeanRsFixture.Handles.levelZero",
    "LeanRsFixture.Handles.levelSucc",
    "LeanRsFixture.Handles.exprConstNat",
];
const ELAB_TERMS: [&str; 4] = ["(1 + 1 : Nat)", "(Nat.succ 0 : Nat)", "1 +", "(1 + \"hi\" : Nat)"];
const BRACKETED_DECLS: [&str; 2] = [
    "LeanRsFixture.Handles.nameAnonymous",
    "LeanRsFixture.Handles.noSuchDeclaration",
];

#[derive(Debug)]
struct Config {
    mode: Mode,
    imports: usize,
    bulk: usize,
    elab: usize,
    pool_capacity: usize,
    checkpoint_every: usize,
    max_rss_kib: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    FreshImport,
    PooledReuse,
    SteadyState,
    ImportMatrix,
    BracketedLightweight,
    DerivedIndexes,
    All,
}

fn main() -> ExitCode {
    install_tracing();
    let config = Config::from_env();
    println!("workload=long_session_memory");
    println!("pid={}", std::process::id());
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("lean_version={LEAN_VERSION}");
    println!("mode={}", config.mode.as_str());
    println!("imports_n={}", config.imports);
    println!("bulk_m={}", config.bulk);
    println!("elab_k={}", config.elab);
    println!("pool_capacity={}", config.pool_capacity);
    println!("checkpoint_every={}", config.checkpoint_every);
    println!(
        "max_rss_kib={}",
        config
            .max_rss_kib
            .map_or_else(|| "none".to_owned(), |value| value.to_string())
    );

    if config.mode == Mode::All {
        return match run_all_children() {
            Ok(()) => {
                println!("status=ok");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("{err}");
                ExitCode::FAILURE
            }
        };
    }

    match run(&config) {
        Ok(()) => {
            println!("status=ok");
            ExitCode::SUCCESS
        }
        Err(err) if err.code() == LeanDiagnosticCode::ResourceExhausted => {
            eprintln!("[{}] {err}", err.code());
            println!("status=resource_exhausted");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("[{}] {err}", err.code());
            ExitCode::FAILURE
        }
    }
}

fn install_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
        .try_init();
}

fn run(config: &Config) -> LeanResult<()> {
    snapshot("start");

    let runtime = LeanRuntime::init()?;
    snapshot("after_runtime_init");

    let host = LeanHost::from_lake_project(runtime, fixture_lake_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;
    snapshot("after_host_capabilities");

    match config.mode {
        Mode::FreshImport => {
            let fresh_stats = run_fresh_import_drop_loop(runtime, &caps, config)?;
            report_pool_stats("fresh_import_drop", fresh_stats);
            snapshot("after_fresh_import_drop");
        }
        Mode::PooledReuse => {
            let pooled_stats = run_bounded_pool_loop(runtime, &caps, config)?;
            report_pool_stats("bounded_pool", pooled_stats);
            snapshot("after_bounded_pool");
        }
        Mode::SteadyState => {
            run_steady_state_loop(runtime, &caps, config)?;
        }
        Mode::ImportMatrix => {
            run_import_matrix(runtime, &caps, config)?;
        }
        Mode::BracketedLightweight => {
            run_bracketed_lightweight(&caps, config)?;
        }
        Mode::DerivedIndexes => {
            run_derived_indexes(&caps, config)?;
        }
        Mode::All => {}
    }

    snapshot("after_drop_sessions_pools");
    thread::sleep(Duration::from_millis(STEADY_STATE_PAUSE_MS));
    snapshot("steady_state_after_pause");
    Ok(())
}

impl Config {
    fn from_env() -> Self {
        Self {
            mode: Mode::from_env(),
            imports: env_usize("LEAN_RS_LONG_SESSION_IMPORTS", DEFAULT_IMPORTS),
            bulk: env_usize("LEAN_RS_LONG_SESSION_BULK", DEFAULT_BULK),
            elab: env_usize("LEAN_RS_LONG_SESSION_ELAB", DEFAULT_ELAB),
            pool_capacity: env_usize("LEAN_RS_LONG_SESSION_POOL_CAPACITY", DEFAULT_POOL_CAPACITY),
            checkpoint_every: env_usize("LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY", DEFAULT_CHECKPOINT_EVERY).max(1),
            max_rss_kib: env_u64_optional("LEAN_RS_LONG_SESSION_MAX_RSS_KIB"),
        }
    }
}

impl Mode {
    fn from_env() -> Self {
        std::env::var("LEAN_RS_LONG_SESSION_MODE")
            .ok()
            .and_then(|raw| Self::parse(raw.trim()))
            .unwrap_or(Self::All)
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "fresh-import" => Some(Self::FreshImport),
            "pooled-reuse" => Some(Self::PooledReuse),
            "steady-state" => Some(Self::SteadyState),
            "import-matrix" => Some(Self::ImportMatrix),
            "bracketed-lightweight" => Some(Self::BracketedLightweight),
            "derived-indexes" => Some(Self::DerivedIndexes),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::FreshImport => "fresh-import",
            Self::PooledReuse => "pooled-reuse",
            Self::SteadyState => "steady-state",
            Self::ImportMatrix => "import-matrix",
            Self::BracketedLightweight => "bracketed-lightweight",
            Self::DerivedIndexes => "derived-indexes",
            Self::All => "all",
        }
    }
}

fn run_all_children() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    for mode in [
        Mode::FreshImport,
        Mode::PooledReuse,
        Mode::SteadyState,
        Mode::ImportMatrix,
        Mode::BracketedLightweight,
        Mode::DerivedIndexes,
    ] {
        println!("child_mode_begin={}", mode.as_str());
        let status = Command::new(&exe)
            .env("LEAN_RS_LONG_SESSION_MODE", mode.as_str())
            .status()?;
        println!("child_mode_end={} success={}", mode.as_str(), status.success());
        if !status.success() {
            return Err(format!("long_session_memory child mode {} failed", mode.as_str()).into());
        }
    }
    Ok(())
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_u64_optional(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(|value| value.max(1))
}

fn run_fresh_import_drop_loop(
    runtime: &'static LeanRuntime,
    caps: &LeanCapabilities<'static, '_>,
    config: &Config,
) -> LeanResult<PoolStats> {
    let pool = SessionPool::with_memory_policy(runtime, 0, memory_policy(config));
    for iteration in 1..=config.imports {
        let session = pool.acquire(caps, &IMPORTS, None, None)?;
        report_import_stats(
            "fresh_import_drop",
            iteration,
            LeanSessionImportProfile::default().label(),
            session.import_stats(),
        );
        drop(session);
        maybe_checkpoint("fresh_import_drop", iteration, config.checkpoint_every);
    }
    Ok(pool.stats())
}

fn run_bounded_pool_loop(
    runtime: &'static LeanRuntime,
    caps: &LeanCapabilities<'static, '_>,
    config: &Config,
) -> LeanResult<PoolStats> {
    let pool = SessionPool::with_memory_policy(runtime, config.pool_capacity, memory_policy(config));
    if config.pool_capacity > 0 {
        let mut warm = Vec::with_capacity(config.pool_capacity);
        for iteration in 1..=config.pool_capacity {
            let session = pool.acquire(caps, &IMPORTS, None, None)?;
            report_import_stats(
                "bounded_pool_warm",
                iteration,
                LeanSessionImportProfile::default().label(),
                session.import_stats(),
            );
            warm.push(session);
        }
        drop(warm);
    }
    snapshot("after_bounded_pool_warm");

    for iteration in 1..=config.imports {
        let session = pool.acquire(caps, &IMPORTS, None, None)?;
        drop(session);
        maybe_checkpoint("bounded_pool", iteration, config.checkpoint_every);
    }
    println!("bounded_pool_len={}", pool.len());
    Ok(pool.stats())
}

fn run_steady_state_loop(
    runtime: &'static LeanRuntime,
    caps: &LeanCapabilities<'static, '_>,
    config: &Config,
) -> LeanResult<()> {
    let pool = SessionPool::with_memory_policy(
        runtime,
        config.pool_capacity,
        memory_policy(config).max_fresh_imports(1),
    );
    let mut session = pool.acquire(caps, &MIXED_IMPORTS, None, None)?;
    report_import_stats(
        "steady_state_warm",
        1,
        LeanSessionImportProfile::default().label(),
        session.import_stats(),
    );

    for iteration in 1..=config.bulk {
        let decls = session.query_declarations_bulk(&BULK_NAMES, None, None)?;
        assert_eq!(decls.len(), BULK_NAMES.len());
        drop(decls);
        maybe_checkpoint("bulk_introspection", iteration, config.checkpoint_every);
    }
    println!("bulk_calls={}", config.bulk);
    snapshot("after_bulk_introspection");

    let opts = LeanElabOptions::new();
    let mut ok = 0usize;
    let mut err = 0usize;
    for (iteration, term) in (1..=config.elab).zip(ELAB_TERMS.iter().cycle()) {
        match session.elaborate(term, None, &opts, None)? {
            Ok(expr) => {
                ok = ok.saturating_add(1);
                drop(expr);
            }
            Err(failure) => {
                err = err.saturating_add(1);
                drop(failure);
            }
        }
        maybe_checkpoint("elaboration", iteration, config.checkpoint_every);
    }
    println!("elab_calls={}", config.elab);
    println!("elab_ok={ok}");
    println!("elab_err={err}");
    snapshot("after_elaboration");

    drop(session);
    report_pool_stats("mixed_pool_before_drop", pool.stats());
    Ok(())
}

fn run_import_matrix(
    _runtime: &'static LeanRuntime,
    caps: &LeanCapabilities<'static, '_>,
    config: &Config,
) -> LeanResult<()> {
    for mode in [
        LeanImportProfileMode::FullSession(LeanSessionImportProfile::ExportedPublic),
        LeanImportProfileMode::FullSession(LeanSessionImportProfile::Server),
        LeanImportProfileMode::FullSession(LeanSessionImportProfile::Private),
        LeanImportProfileMode::FullSession(LeanSessionImportProfile::FullPrivateCompat),
        LeanImportProfileMode::ExportedNoExts,
    ] {
        for iteration in 1..=config.imports {
            enforce_matrix_rss_cap(config)?;
            let profiler_options = import_profiler_options(mode, iteration);
            match caps.profiling_session(&IMPORTS, mode, &profiler_options) {
                Ok(mut session) => {
                    report_import_stats("import_matrix", iteration, mode.label(), session.import_stats());
                    if !mode.load_exts() {
                        report_no_ext_capability_gap(mode, &mut session);
                    }
                    drop(session);
                }
                Err(err) => {
                    report_import_profile_gap(mode, iteration, &err);
                }
            }
            maybe_checkpoint(
                &format!("import_matrix_{}", mode.label().replace('-', "_")),
                iteration,
                config.checkpoint_every,
            );
        }
    }
    Ok(())
}

fn run_bracketed_lightweight(caps: &LeanCapabilities<'static, '_>, config: &Config) -> LeanResult<()> {
    for iteration in 1..=config.imports {
        enforce_matrix_rss_cap(config)?;
        snapshot(&format!("bracketed_before_dispatch_{iteration}"));
        let sink = BracketedCheckpointSink { iteration };
        let result =
            caps.bracketed_import_query(&IMPORTS, LeanBracketedImportRequest::new(BRACKETED_DECLS), Some(&sink))?;
        report_bracketed_import_stats(
            "bracketed_lightweight",
            iteration,
            &result.import_stats,
            result.free_regions_ran,
        );
        snapshot(&format!("bracketed_after_rust_return_{iteration}"));
        thread::sleep(Duration::from_millis(250));
        snapshot(&format!("bracketed_after_pause_{iteration}"));
    }
    Ok(())
}

fn run_derived_indexes(caps: &LeanCapabilities<'static, '_>, config: &Config) -> LeanResult<()> {
    enforce_matrix_rss_cap(config)?;
    let mut session = caps.session(&["LeanRsFixture.SourceRanges"], None, None)?;
    report_import_stats(
        "derived_indexes",
        1,
        LeanSessionImportProfile::default().label(),
        session.import_stats(),
    );

    let decl = "LeanRsFixture.SourceRanges.documentedSimpTheorem";

    let mut cheap_request = DeclarationInspectionRequest::new(decl);
    cheap_request.fields = DeclarationInspectionFields {
        source: true,
        statement: false,
        docstring: false,
        attributes: true,
        flags: true,
        statement_pretty: false,
        proof_search: false,
    };
    report_inspection_derived_work("cheap_inspection", &mut session, &cheap_request)?;

    let mut raw_request = cheap_request.clone();
    raw_request.fields.statement = true;
    report_inspection_derived_work("raw_statement_inspection", &mut session, &raw_request)?;

    let mut pretty_request = raw_request.clone();
    pretty_request.fields.statement_pretty = true;
    report_inspection_derived_work("pretty_statement_inspection", &mut session, &pretty_request)?;

    let mut proof_request = pretty_request.clone();
    proof_request.fields.proof_search = true;
    proof_request.fields.docstring = true;
    report_inspection_derived_work("proof_search_inspection", &mut session, &proof_request)?;

    let mut search = DeclarationSearchRequest::new("documentedSimpTheorem");
    search.include_source = false;
    let search_without_source = session.search_declarations(&search, None)?;
    report_derived_work(
        "declaration_search_no_source",
        1,
        &search_without_source.facts.derived_work,
    );

    search.include_source = true;
    let search_with_source = session.search_declarations(&search, None)?;
    report_derived_work(
        "declaration_search_with_source",
        1,
        &search_with_source.facts.derived_work,
    );

    let options = LeanElabOptions::new();
    let source = "theorem derivedProbe (h : True) : True := by\n  exact h\n";
    let _module_query = session.process_module_query_batch(
        source,
        &[ModuleQuerySelector::ProofState {
            id: "state".to_owned(),
            line: 2,
            column: 4,
        }],
        &ModuleQueryOutputBudgets::default(),
        &options,
        None,
    )?;
    report_derived_work(
        "module_query_proof_state",
        1,
        &LeanDerivedWorkFacts {
            parser_elaborator_runs: 1,
            module_snapshot_builds: 1,
            ..LeanDerivedWorkFacts::default()
        },
    );

    let proof_request = ProofAttemptRequest {
        source: source.to_owned(),
        edit: ProofEditTarget::Declaration {
            name: "derivedProbe".to_owned(),
            position: ProofPositionSelector::Default,
        },
        candidates: vec![ProofCandidate {
            id: "exact_h".to_owned(),
            text: "exact h".to_owned(),
        }],
        budgets: ModuleQueryOutputBudgets::default(),
    };
    let _attempt = session.attempt_proof(&proof_request, &options, None)?;
    report_derived_work(
        "proof_attempt",
        1,
        &LeanDerivedWorkFacts {
            parser_elaborator_runs: 1,
            module_snapshot_builds: 1,
            ..LeanDerivedWorkFacts::default()
        },
    );

    let verification_request = DeclarationVerificationRequest {
        source: source.to_owned(),
        target: DeclarationVerificationTarget::Name {
            name: "derivedProbe".to_owned(),
        },
        sorry_policy: SorryPolicy::Deny,
        report_axioms: false,
        budgets: ModuleQueryOutputBudgets::default(),
    };
    let _verification = session.verify_declaration(&verification_request, &options, None)?;
    report_derived_work(
        "verify_declaration",
        1,
        &LeanDerivedWorkFacts {
            parser_elaborator_runs: 1,
            module_snapshot_builds: 1,
            ..LeanDerivedWorkFacts::default()
        },
    );

    Ok(())
}

fn report_inspection_derived_work(
    label: &str,
    session: &mut lean_rs_host::LeanSession<'_, '_>,
    request: &DeclarationInspectionRequest,
) -> LeanResult<()> {
    match session.inspect_declaration(request, None)? {
        DeclarationInspectionResult::Found { declaration } => {
            report_derived_work(label, 1, &declaration.derived_work);
        }
        DeclarationInspectionResult::NotFound { name } => {
            println!("query_derived_work_gap={label} status=not_found name={name}");
        }
        DeclarationInspectionResult::Unsupported => {
            println!("query_derived_work_gap={label} status=unsupported");
        }
    }
    Ok(())
}

struct BracketedCheckpointSink {
    iteration: usize,
}

impl LeanProgressSink for BracketedCheckpointSink {
    fn report(&self, event: LeanProgressEvent) {
        let stage = match event.current {
            1 => "after_lean_import",
            2 => "after_query_before_free",
            3 => "after_free",
            _ => "unknown",
        };
        snapshot(&format!("bracketed_{stage}_{}", self.iteration));
    }
}

fn memory_policy(config: &Config) -> SessionPoolMemoryPolicy {
    let fresh_imports = config.imports.max(config.pool_capacity) as u64;
    let mut policy = SessionPoolMemoryPolicy::disabled().max_fresh_imports(fresh_imports);
    if let Some(limit) = config.max_rss_kib {
        policy = policy.max_rss_kib(limit);
    }
    policy
}

fn enforce_matrix_rss_cap(config: &Config) -> LeanResult<()> {
    let Some(limit) = config.max_rss_kib else {
        return Ok(());
    };
    match rss_kib() {
        Ok(current) if current >= limit => Err(lean_rs::__host_internals::host_resource_exhausted(format!(
            "import matrix refused fresh import: current RSS {current} KiB reached max_rss_kib={limit}"
        ))),
        Ok(_) => Ok(()),
        Err(err) => Err(lean_rs::__host_internals::host_resource_exhausted(format!(
            "import matrix refused fresh import: RSS sample unavailable while max_rss_kib={limit}: {err}"
        ))),
    }
}

fn import_profiler_options(mode: LeanImportProfileMode, iteration: usize) -> LeanImportProfilerOptions {
    let mut options = LeanImportProfilerOptions::new()
        .profiler(env_bool("LEAN_RS_IMPORT_PROFILE_PROFILER"))
        .trace_profiler(env_bool("LEAN_RS_IMPORT_PROFILE_TRACE_PROFILER"));
    if let Ok(dir) = std::env::var("LEAN_RS_IMPORT_PROFILE_TRACE_PROFILER_OUTPUT_DIR") {
        let path = PathBuf::from(dir).join(format!("{}-{iteration}.json", mode.label()));
        options = options.trace_profiler_output(path.to_string_lossy().into_owned());
    } else if let Ok(path) = std::env::var("LEAN_RS_IMPORT_PROFILE_TRACE_PROFILER_OUTPUT") {
        options = options.trace_profiler_output(path);
    }
    options
}

fn env_bool(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn report_import_profile_gap(mode: LeanImportProfileMode, iteration: usize, err: &LeanError) {
    let message = format!("{err:?}").replace(char::is_whitespace, "_");
    println!(
        "import_profile_gap=import_matrix iteration={iteration} profile_mode={} import_all={} import_level={} load_exts={} error={message}",
        mode.label(),
        mode.import_all(),
        mode.import_level().as_str(),
        mode.load_exts(),
    );
}

fn report_no_ext_capability_gap(mode: LeanImportProfileMode, session: &mut lean_rs_host::LeanSession<'_, '_>) {
    let opts = LeanElabOptions::new();
    match session.elaborate(ELAB_TERMS[0], None, &opts, None) {
        Ok(Ok(expr)) => {
            drop(expr);
            println!(
                "import_mode_capability mode={} service=elaborate status=ok",
                mode.label()
            );
        }
        Ok(Err(failure)) => {
            drop(failure);
            println!(
                "import_mode_capability_gap mode={} service=elaborate status=diagnostic_failure",
                mode.label()
            );
        }
        Err(err) => {
            println!(
                "import_mode_capability_gap mode={} service=elaborate status=host_error code={}",
                mode.label(),
                err.code()
            );
        }
    }
}

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}

fn maybe_checkpoint(stage: &str, iteration: usize, checkpoint_every: usize) {
    if iteration.is_multiple_of(checkpoint_every) {
        snapshot(&format!("{stage}_{iteration}"));
    }
}

fn snapshot(stage: &str) {
    match rss_kib() {
        Ok(kib) => println!("checkpoint={stage} rss_kib={kib}"),
        Err(err) => println!("checkpoint={stage} rss_error={err}"),
    }
}

fn rss_kib() -> std::io::Result<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.trim().parse::<u64>().unwrap_or(0))
}

fn report_pool_stats(label: &str, stats: PoolStats) {
    println!(
        "pool_stats={label} imports_performed={} reused={} acquired={} released_to_pool={} released_dropped={} drains={} drained={}",
        stats.imports_performed,
        stats.reused,
        stats.acquired,
        stats.released_to_pool,
        stats.released_dropped,
        stats.drains,
        stats.drained
    );
}

fn report_import_stats(label: &str, iteration: usize, profile_mode: &str, stats: &LeanImportStats) {
    println!(
        "import_stats={label} iteration={iteration} profile_mode={profile_mode} direct_imports={} effective_modules={} compacted_regions={} memory_mapped_regions={} compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} imported_bytes={} imported_constants={} extension_count={} total_imported_extension_entries={} import_level={} import_all={} load_exts={}",
        stats.direct_import_names.join(","),
        stats.effective_module_count,
        stats.compacted_region_count,
        stats.memory_mapped_region_count,
        stats.compacted_region_bytes,
        stats.memory_mapped_region_bytes,
        stats.non_memory_mapped_region_bytes,
        stats.imported_bytes,
        stats.imported_constant_count,
        stats.extension_count,
        stats.total_imported_extension_entries,
        stats.import_level,
        stats.import_all,
        stats.load_exts,
    );
}

fn report_bracketed_import_stats(label: &str, iteration: usize, stats: &LeanImportStats, free_regions_ran: bool) {
    println!(
        "bracketed_import_stats={label} iteration={iteration} profile_mode=bracketed-private-no-exts direct_imports={} effective_modules={} compacted_regions={} memory_mapped_regions={} compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} imported_bytes={} imported_constants={} extension_count={} total_imported_extension_entries={} import_level={} import_all={} load_exts={} free_regions_ran={free_regions_ran}",
        stats.direct_import_names.join(","),
        stats.effective_module_count,
        stats.compacted_region_count,
        stats.memory_mapped_region_count,
        stats.compacted_region_bytes,
        stats.memory_mapped_region_bytes,
        stats.non_memory_mapped_region_bytes,
        stats.imported_bytes,
        stats.imported_constant_count,
        stats.extension_count,
        stats.total_imported_extension_entries,
        stats.import_level,
        stats.import_all,
        stats.load_exts,
    );
}

fn report_derived_work(label: &str, iteration: usize, facts: &LeanDerivedWorkFacts) {
    println!(
        "query_derived_work={label} iteration={iteration} source_range_lookups={} docstring_lookups={} raw_type_renderings={} pretty_prints={} proof_search_fact_collections={} simp_extension_lookups={} parser_elaborator_runs={} module_snapshot_builds={} lazy_discr_tree_import_initialization_observed={}",
        facts.source_range_lookups,
        facts.docstring_lookups,
        facts.raw_type_renderings,
        facts.pretty_prints,
        facts.proof_search_fact_collections,
        facts.simp_extension_lookups,
        facts.parser_elaborator_runs,
        facts.module_snapshot_builds,
        facts.lazy_discr_tree_import_initialization_observed,
    );
}
