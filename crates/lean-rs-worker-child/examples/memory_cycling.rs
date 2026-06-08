//! Worker memory-cycling reproducer.
//!
//! Build the child binary first, then run:
//!
//! ```sh
//! cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
//! LEAN_RS_WORKER_MEMORY_IMPORTS=8 \
//! LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=1 \
//! LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB=1572864 \
//! cargo run -p lean-rs-worker-child --example memory_cycling
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use lean_rs_worker_parent::{LeanWorker, LeanWorkerConfig, LeanWorkerRestartPolicy, LeanWorkerSessionConfig};

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
    println!("worker_policy max_imports_per_child={max_imports} max_rss_kib={max_rss_kib}");

    let session_config =
        LeanWorkerSessionConfig::new(&fixture, "lean_rs_fixture", "LeanRsFixture", ["LeanRsFixture.Handles"]);
    for iteration in 1..=imports {
        let before = worker.stats();
        let started = Instant::now();
        let session = worker.open_session(&session_config, None, None)?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        drop(session);
        let rss = worker.rss_kib();
        let stats = worker.stats();
        let open_kind = if before.requests == 0 || stats.restarts > before.restarts {
            "cold"
        } else {
            "warm-same-child"
        };
        println!(
            "session_open_timing=worker_cycling iteration={iteration} kind={open_kind} elapsed_ms={elapsed_ms:.3} max_imports_per_child={max_imports} max_rss_kib={max_rss_kib} rss_kib={}",
            stats
                .last_rss_kib
                .or(rss)
                .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        );
        report_admission(iteration, open_kind, &before, &stats);
        println!(
            "iteration={iteration} requests={} imports={} restarts={} exits={} rss_kib={} last_reason={:?}",
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
        if let Some(import_stats) = stats.last_import_stats.as_ref() {
            report_import_stats("worker_cycling_session_open", iteration, import_stats);
        }
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

fn report_admission(
    iteration: u64,
    kind: &str,
    before: &lean_rs_worker_parent::LeanWorkerStats,
    after: &lean_rs_worker_parent::LeanWorkerStats,
) {
    let cold_open = u64::from(kind == "cold");
    let import_like_requests = after
        .import_like_admission_attempts
        .saturating_sub(before.import_like_admission_attempts);
    let import_like_admitted = after.import_like_admitted.saturating_sub(before.import_like_admitted);
    println!(
        "admission=worker_session_open iteration={iteration} kind={kind} cold_open_attempts={cold_open} cold_open_admitted={cold_open} cold_open_refusals=0 import_like_requests={import_like_requests} import_like_admitted={import_like_admitted} concurrent_cold_opens_observed=0 rss_before_admission_kib={} rss_after_open_kib={} refusal_reason=none",
        after
            .last_import_like_rss_before_admission_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        after
            .last_rss_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
    );
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

fn report_import_stats(label: &str, iteration: u64, stats: &lean_rs_worker_protocol::types::LeanWorkerImportStats) {
    println!(
        "import_stats={label} iteration={iteration} profile_mode=worker-session direct_imports={} effective_modules={} compacted_regions={} memory_mapped_regions={} compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} imported_bytes={} imported_constants={} extension_count={} total_imported_extension_entries={} import_level={} import_all={} load_exts={}",
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
