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

use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::{LeanCapabilities, LeanElabOptions, LeanHost, PoolStats, SessionPool};

const DEFAULT_IMPORTS: usize = 192;
const DEFAULT_BULK: usize = 512;
const DEFAULT_ELAB: usize = 512;
const DEFAULT_POOL_CAPACITY: usize = 4;
const DEFAULT_CHECKPOINT_EVERY: usize = 64;
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

#[derive(Debug)]
struct Config {
    imports: usize,
    bulk: usize,
    elab: usize,
    pool_capacity: usize,
    checkpoint_every: usize,
}

fn main() -> ExitCode {
    install_tracing();
    let config = Config::from_env();
    println!("workload=long_session_memory");
    println!("pid={}", std::process::id());
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("lean_version={}", lean_rs_sys::LEAN_VERSION);
    println!("lean_resolved_version={}", lean_rs_sys::LEAN_RESOLVED_VERSION);
    println!("imports_n={}", config.imports);
    println!("bulk_m={}", config.bulk);
    println!("elab_k={}", config.elab);
    println!("pool_capacity={}", config.pool_capacity);
    println!("checkpoint_every={}", config.checkpoint_every);

    match run(&config) {
        Ok(()) => {
            println!("status=ok");
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

    let fresh_stats = run_fresh_import_drop_loop(runtime, &caps, config)?;
    report_pool_stats("fresh_import_drop", fresh_stats);
    snapshot("after_fresh_import_drop");

    {
        let pooled_stats = run_bounded_pool_loop(runtime, &caps, config)?;
        report_pool_stats("bounded_pool", pooled_stats);
        snapshot("after_bounded_pool");

        let pool = SessionPool::with_capacity(runtime, config.pool_capacity);
        let mut session = pool.acquire(&caps, &MIXED_IMPORTS)?;

        for iteration in 1..=config.bulk {
            let decls = session.query_declarations_bulk(&BULK_NAMES)?;
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
            match session.elaborate(term, None, &opts)? {
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
    }

    snapshot("after_drop_sessions_pools");
    thread::sleep(Duration::from_millis(STEADY_STATE_PAUSE_MS));
    snapshot("steady_state_after_pause");
    Ok(())
}

impl Config {
    fn from_env() -> Self {
        Self {
            imports: env_usize("LEAN_RS_LONG_SESSION_IMPORTS", DEFAULT_IMPORTS),
            bulk: env_usize("LEAN_RS_LONG_SESSION_BULK", DEFAULT_BULK),
            elab: env_usize("LEAN_RS_LONG_SESSION_ELAB", DEFAULT_ELAB),
            pool_capacity: env_usize("LEAN_RS_LONG_SESSION_POOL_CAPACITY", DEFAULT_POOL_CAPACITY),
            checkpoint_every: env_usize("LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY", DEFAULT_CHECKPOINT_EVERY).max(1),
        }
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn run_fresh_import_drop_loop(
    runtime: &'static LeanRuntime,
    caps: &LeanCapabilities<'static, '_>,
    config: &Config,
) -> LeanResult<PoolStats> {
    let pool = SessionPool::with_capacity(runtime, 0);
    for iteration in 1..=config.imports {
        let session = pool.acquire(caps, &IMPORTS)?;
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
    let pool = SessionPool::with_capacity(runtime, config.pool_capacity);
    if config.pool_capacity > 0 {
        let mut warm = Vec::with_capacity(config.pool_capacity);
        for _ in 0..config.pool_capacity {
            warm.push(pool.acquire(caps, &IMPORTS)?);
        }
        drop(warm);
    }
    snapshot("after_bounded_pool_warm");

    for iteration in 1..=config.imports {
        let session = pool.acquire(caps, &IMPORTS)?;
        drop(session);
        maybe_checkpoint("bounded_pool", iteration, config.checkpoint_every);
    }
    println!("bounded_pool_len={}", pool.len());
    Ok(pool.stats())
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
        "pool_stats={label} imports_performed={} reused={} acquired={} released_to_pool={} released_dropped={}",
        stats.imports_performed, stats.reused, stats.acquired, stats.released_to_pool, stats.released_dropped
    );
}
