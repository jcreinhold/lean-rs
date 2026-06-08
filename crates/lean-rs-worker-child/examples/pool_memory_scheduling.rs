//! Worker-pool memory scheduling workload.
//!
//! Build the child binary first, then run:
//!
//! ```sh
//! cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
//! LEAN_RS_POOL_MEMORY_MAX_WORKERS=1 \
//! LEAN_RS_POOL_MEMORY_TOTAL_RSS_KIB=1572864 \
//! LEAN_RS_POOL_MEMORY_PER_WORKER_RSS_KIB=1572864 \
//! LEAN_RS_POOL_MEMORY_MAX_IMPORTS=1 \
//! cargo run -p lean-rs-worker-child --example pool_memory_scheduling
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use lean_rs_worker_parent::{
    LeanWorkerCapabilityBuilder, LeanWorkerError, LeanWorkerJsonCommand, LeanWorkerPool, LeanWorkerPoolConfig,
    LeanWorkerRestartPolicy,
};
use serde::{Deserialize, Serialize};

const DEFAULT_MAX_WORKERS: usize = 1;
const DEFAULT_TOTAL_CHILD_RSS_KIB: u64 = 1_572_864;
const DEFAULT_PER_WORKER_RSS_KIB: u64 = 1_572_864;
const DEFAULT_MAX_IMPORTS: u64 = 1;

#[derive(Clone, Copy)]
struct MemoryPolicyKnobs {
    max_workers: usize,
    total_child_rss_kib: u64,
    per_worker_rss_kib: u64,
    max_imports: u64,
}

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
    let fixture = interop_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsInteropConsumer")?;
    let worker_binary = worker_binary()?;
    let knobs = MemoryPolicyKnobs::from_env();

    println!("workload=worker_pool_memory_scheduling");
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!(
        "pool_policy max_workers={} max_total_child_rss_kib={} per_worker_rss_ceiling_kib={} max_imports_per_child={}",
        knobs.max_workers, knobs.total_child_rss_kib, knobs.per_worker_rss_kib, knobs.max_imports
    );
    println!(
        "parent_rss_start_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );

    fixture_import_reuse(&worker_binary, knobs)?;
    mathlib_shaped_fallback(&worker_binary, knobs)?;
    repeated_session_reuse(&worker_binary, knobs)?;
    admission_refusal_stress(&worker_binary, knobs)?;

    println!(
        "parent_rss_end_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );
    println!("status=ok");
    Ok(())
}

fn fixture_import_reuse(worker_binary: &Path, knobs: MemoryPolicyKnobs) -> Result<(), LeanWorkerError> {
    let mut pool = LeanWorkerPool::new(memory_pool_config(knobs).idle_cycle_after(Duration::from_mins(10)));
    for iteration in 1..=3 {
        let started = Instant::now();
        let mut lease = pool.acquire_lease(memory_bounded_builder(worker_binary, knobs))?;
        let response = lease.run_json_command(
            &json_command(),
            &FixtureRequest {
                source: format!("fixture-import-reuse-{iteration}"),
            },
            None,
            None,
        )?;
        println!(
            "fixture_import_reuse iteration={iteration} accepted={} kind={}",
            response.accepted, response.kind
        );
        drop(lease);
        print_timing("fixture_import_reuse", iteration, started.elapsed(), &pool);
    }
    print_snapshot("fixture_import_reuse", &pool);
    Ok(())
}

fn mathlib_shaped_fallback(worker_binary: &Path, knobs: MemoryPolicyKnobs) -> Result<(), LeanWorkerError> {
    let mut pool = LeanWorkerPool::new(memory_pool_config(knobs));
    let started = Instant::now();
    let mut lease = pool.acquire_lease(memory_bounded_builder(worker_binary, knobs))?;
    let response = lease.run_json_command(
        &json_command(),
        &FixtureRequest {
            source: "mathlib-shaped-fallback".to_owned(),
        },
        None,
        None,
    )?;
    println!(
        "mathlib_shaped_import_set fallback=interop-shims accepted={} kind={}",
        response.accepted, response.kind
    );
    drop(lease);
    print_timing("mathlib_shaped_import_set", 1, started.elapsed(), &pool);
    print_snapshot("mathlib_shaped_import_set", &pool);
    Ok(())
}

fn repeated_session_reuse(worker_binary: &Path, knobs: MemoryPolicyKnobs) -> Result<(), LeanWorkerError> {
    let builder =
        builder(worker_binary).restart_policy(LeanWorkerRestartPolicy::memory_bounded(1, knobs.per_worker_rss_kib));
    let mut pool = LeanWorkerPool::new(memory_pool_config(knobs));
    for iteration in 1..=3 {
        let started = Instant::now();
        let mut lease = pool.acquire_lease(builder.clone())?;
        let response = lease.run_json_command(
            &json_command(),
            &FixtureRequest {
                source: format!("bounded-reuse-no-cycle-{iteration}"),
            },
            None,
            None,
        )?;
        println!(
            "bounded_reuse_no_cycle iteration={iteration} accepted={} kind={}",
            response.accepted, response.kind
        );
        drop(lease);
        print_timing("bounded_reuse_no_cycle", iteration, started.elapsed(), &pool);
    }
    print_snapshot("bounded_reuse_no_cycle", &pool);
    Ok(())
}

fn admission_refusal_stress(worker_binary: &Path, knobs: MemoryPolicyKnobs) -> Result<(), LeanWorkerError> {
    let mut pool = LeanWorkerPool::new(memory_pool_config(MemoryPolicyKnobs {
        max_workers: 1,
        ..knobs
    }));
    {
        let mut lease = pool.acquire_lease(builder(worker_binary))?;
        let response = lease.run_json_command(
            &json_command(),
            &FixtureRequest {
                source: "admission-refusal-first".to_owned(),
            },
            None,
            None,
        )?;
        println!(
            "admission_refusal_stress first_accepted={} kind={}",
            response.accepted, response.kind
        );
    }

    let distinct = builder(worker_binary).restart_policy(LeanWorkerRestartPolicy::default().max_requests(99));
    match pool.acquire_lease(distinct) {
        Err(LeanWorkerError::WorkerPoolExhausted { max_workers }) => {
            println!("admission_refusal_stress refused=max_workers max_workers={max_workers}");
        }
        Err(LeanWorkerError::WorkerPoolMemoryBudgetExceeded {
            current_kib, limit_kib, ..
        }) => {
            println!("admission_refusal_stress refused=rss_budget current_kib={current_kib} limit_kib={limit_kib}");
        }
        Err(err) => return Err(err),
        Ok(lease) => {
            drop(lease);
            println!("admission_refusal_stress refused=none");
        }
    }
    print_admission("admission_refusal_stress", 1, &pool);
    print_snapshot("admission_refusal_stress", &pool);
    Ok(())
}

fn memory_pool_config(knobs: MemoryPolicyKnobs) -> LeanWorkerPoolConfig {
    LeanWorkerPoolConfig::new(knobs.max_workers)
        .max_total_child_rss_kib(knobs.total_child_rss_kib)
        .per_worker_rss_ceiling_kib(knobs.per_worker_rss_kib)
}

fn memory_bounded_builder(worker_binary: &Path, knobs: MemoryPolicyKnobs) -> LeanWorkerCapabilityBuilder {
    builder(worker_binary).restart_policy(LeanWorkerRestartPolicy::memory_bounded(
        knobs.max_imports,
        knobs.per_worker_rss_kib,
    ))
}

impl MemoryPolicyKnobs {
    fn from_env() -> Self {
        Self {
            max_workers: env_usize("LEAN_RS_POOL_MEMORY_MAX_WORKERS", DEFAULT_MAX_WORKERS),
            total_child_rss_kib: env_u64("LEAN_RS_POOL_MEMORY_TOTAL_RSS_KIB", DEFAULT_TOTAL_CHILD_RSS_KIB),
            per_worker_rss_kib: env_u64("LEAN_RS_POOL_MEMORY_PER_WORKER_RSS_KIB", DEFAULT_PER_WORKER_RSS_KIB),
            max_imports: env_u64("LEAN_RS_POOL_MEMORY_MAX_IMPORTS", DEFAULT_MAX_IMPORTS),
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn print_timing(name: &str, iteration: u64, elapsed: Duration, pool: &LeanWorkerPool) {
    let snapshot = pool.snapshot();
    let kind = if iteration == 1 { "cold" } else { "warm-pool" };
    println!(
        "pool_request_timing={name} iteration={iteration} kind={kind} elapsed_ms={:.3} workers={} total_child_rss_kib={} worker_restarts={} max_import_restarts={} policy_restarts={}",
        elapsed.as_secs_f64() * 1_000.0,
        snapshot.workers,
        snapshot
            .total_child_rss_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot.worker_restarts,
        snapshot.max_import_restarts,
        snapshot.policy_restarts,
    );
    print_admission(name, iteration, pool);
    print_session_reuse(name, iteration, pool);
    print_replacement(name, iteration, pool);
}

fn print_snapshot(name: &str, pool: &LeanWorkerPool) {
    let snapshot = pool.snapshot();
    println!(
        "{name} workers={} total_child_rss_kib={} rss_unavailable={} worker_restarts={} max_import_restarts={} policy_restarts={} memory_budget_rejections={} queue_timeouts={} last_restart_reason={:?}",
        snapshot.workers,
        snapshot
            .total_child_rss_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot.rss_samples_unavailable,
        snapshot.worker_restarts,
        snapshot.max_import_restarts,
        snapshot.policy_restarts,
        snapshot.memory_budget_rejections,
        snapshot.queue_timeouts,
        snapshot.last_restart_reason,
    );
    if let Some(import_stats) = snapshot.last_import_stats.as_ref() {
        report_import_stats(name, import_stats);
    }
}

fn print_admission(name: &str, iteration: u64, pool: &LeanWorkerPool) {
    let snapshot = pool.snapshot();
    println!(
        "admission={name} iteration={iteration} kind=pool cold_open_attempts={} cold_open_admitted={} cold_open_refusals={} import_like_requests={} import_like_admitted={} concurrent_cold_opens_observed={} rss_before_admission_kib={} rss_after_open_kib={} refusal_reason={}",
        snapshot.cold_open_attempts,
        snapshot.cold_open_admitted,
        snapshot.cold_open_refusals,
        snapshot.import_like_requests,
        snapshot.imports,
        snapshot.concurrent_cold_opens_observed,
        snapshot
            .rss_before_admission_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot
            .rss_after_open_kib
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        snapshot.refusal_reason.as_deref().unwrap_or("none"),
    );
}

fn print_session_reuse(name: &str, iteration: u64, pool: &LeanWorkerPool) {
    let snapshot = pool.snapshot();
    println!(
        "session_reuse={name} iteration={iteration} layer=worker-pool key_hits={} key_misses={} distinct_keys_seen={} fresh_imports_avoided={} miss_empty_pool={} miss_reuse_disabled=0 miss_no_matching_key={} last_miss_reason={}",
        snapshot.key_hits,
        snapshot.key_misses,
        snapshot.distinct_keys_seen,
        snapshot.fresh_cold_opens_avoided,
        snapshot.miss_empty_pool,
        snapshot.miss_no_matching_key,
        snapshot.last_key_miss_reason.as_deref().unwrap_or("none"),
    );
}

fn print_replacement(name: &str, iteration: u64, pool: &LeanWorkerPool) {
    let snapshot = pool.snapshot();
    let timing = snapshot.last_replacement_timing.as_ref();
    let kind = if iteration == 1 { "cold" } else { "warm-pool" };
    println!(
        "replacement={name} iteration={iteration} kind={kind} replacement_attempts={} replacement_successes={} replacement_failures={} replacement_budget_admitted={} replacement_budget_skipped={} spawn_handshake_ms={} capability_load_ms={} session_open_import_ms={} first_command_ms={} warm_command_ms={} replacement_total_ms={} replacement_reason={} replacement_budget_status={} skipped_reason={}",
        snapshot.replacement_attempts,
        snapshot.replacement_successes,
        snapshot.replacement_failures,
        snapshot.replacement_budget_admitted,
        snapshot.replacement_budget_skipped,
        timing.map_or_else(
            || {
                snapshot
                    .last_spawn_handshake_elapsed
                    .map_or_else(|| "unavailable".to_owned(), duration_ms)
            },
            |value| duration_ms(value.spawn_handshake)
        ),
        snapshot
            .last_capability_load_elapsed
            .map_or_else(|| "unavailable".to_owned(), duration_ms),
        snapshot
            .last_session_open_import_elapsed
            .map_or_else(|| "unavailable".to_owned(), duration_ms),
        snapshot
            .last_first_command_elapsed
            .map_or_else(|| "unavailable".to_owned(), duration_ms),
        snapshot
            .last_warm_command_elapsed
            .map_or_else(|| "unavailable".to_owned(), duration_ms),
        timing.map_or_else(
            || "unavailable".to_owned(),
            |value| duration_ms(value.replacement_total)
        ),
        timing.map_or("none", |value| value.replacement_reason.as_str()),
        timing.map_or("none", |value| value.replacement_budget_status.as_str()),
        snapshot.last_replacement_skipped_reason.as_deref().unwrap_or("none"),
    );
}

fn report_import_stats(label: &str, stats: &lean_rs_worker_protocol::types::LeanWorkerImportStats) {
    println!(
        "import_stats={label} iteration=0 profile_mode=worker-pool direct_imports={} effective_modules={} compacted_regions={} memory_mapped_regions={} compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} imported_bytes={} imported_constants={} extension_count={} total_imported_extension_entries={} import_level={} import_all={} load_exts={}",
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

fn duration_ms(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64() * 1_000.0)
}

fn builder(worker_binary: &Path) -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
    .json_command_export("lean_rs_interop_consumer_worker_json_command")
    .worker_executable(worker_binary)
}

fn json_command() -> LeanWorkerJsonCommand<FixtureRequest, FixtureResponse> {
    LeanWorkerJsonCommand::new("lean_rs_interop_consumer_worker_json_command")
}

#[derive(Debug, Serialize)]
struct FixtureRequest {
    source: String,
}

#[derive(Debug, Deserialize)]
struct FixtureResponse {
    accepted: bool,
    kind: String,
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

fn interop_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(Path::parent).map_or_else(
        || PathBuf::from("fixtures/interop-shims"),
        |workspace| workspace.join("fixtures").join("interop-shims"),
    )
}

fn current_process_rss_kib() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}
