//! Worker memory-cycling reproducer.
//!
//! Build the child binary first, then run:
//!
//! ```sh
//! cargo build -p lean-rs-worker --bin lean-rs-worker-child
//! LEAN_RS_WORKER_MEMORY_IMPORTS=8 \
//! LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=2 \
//! cargo run -p lean-rs-worker --example memory_cycling
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lean_rs_worker_parent::{LeanWorker, LeanWorkerConfig, LeanWorkerRestartPolicy};

const DEFAULT_IMPORTS: u64 = 8;
const DEFAULT_MAX_IMPORTS: u64 = 2;
const DEFAULT_MAX_RSS_KIB: u64 = 1_572_864;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let imports = env_u64("LEAN_RS_WORKER_MEMORY_IMPORTS", DEFAULT_IMPORTS);
    let max_imports = env_u64("LEAN_RS_WORKER_MEMORY_MAX_IMPORTS", DEFAULT_MAX_IMPORTS);
    let max_rss_kib = env_u64("LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB", DEFAULT_MAX_RSS_KIB);
    let fixture = fixture_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsFixture")?;

    let worker_binary = worker_binary()?;
    let policy = LeanWorkerRestartPolicy::memory_bounded(max_imports, max_rss_kib);
    let mut worker = LeanWorker::spawn(&LeanWorkerConfig::new(worker_binary).restart_policy(policy))?;

    println!("workload=worker_memory_cycling");
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("imports={imports}");
    println!("max_imports_per_child={max_imports}");
    println!("max_rss_kib={max_rss_kib}");

    for iteration in 1..=imports {
        let value = worker.call_fixture_mul(&fixture, iteration, 2)?;
        let rss = worker.rss_kib();
        let stats = worker.stats();
        println!(
            "iteration={iteration} value={value} requests={} imports={} restarts={} exits={} rss_kib={} last_reason={:?}",
            stats.requests,
            stats.imports,
            stats.restarts,
            stats.exits,
            stats
                .last_rss_kib
                .or(rss)
                .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
            stats.last_restart_reason
        );
    }

    let stats = worker.stats();
    println!(
        "summary requests={} imports={} restarts={} exits={} rss_unavailable={}",
        stats.requests, stats.imports, stats.restarts, stats.exits, stats.rss_samples_unavailable
    );
    let exit = worker.terminate()?;
    println!("worker_exit_success={}", exit.success);
    println!("status=ok");
    Ok(())
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn worker_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let current = std::env::current_exe()?;
    let profile_dir = current
        .parent()
        .and_then(Path::parent)
        .ok_or("example binary is not under target/<profile>/examples")?;
    let binary = profile_dir.join(format!("lean-rs-worker-child{}", std::env::consts::EXE_SUFFIX));
    if !binary.exists() {
        return Err(format!(
            "worker child binary not found at {}; run `cargo build -p lean-rs-worker --bin lean-rs-worker-child` first",
            binary.display()
        )
        .into());
    }
    Ok(binary)
}

fn fixture_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
