//! Worker-pool memory scheduling workload.
//!
//! Build the child binary first, then run:
//!
//! ```sh
//! cargo build -p lean-rs-worker --bin lean-rs-worker-child
//! cargo run -p lean-rs-worker --example pool_memory_scheduling
//! ```

#![allow(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use lean_rs_worker::{
    LeanWorkerCapabilityBuilder, LeanWorkerError, LeanWorkerJsonCommand, LeanWorkerPool, LeanWorkerPoolConfig,
    LeanWorkerRestartPolicy,
};
use serde::{Deserialize, Serialize};

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

    println!("workload=worker_pool_memory_scheduling");
    println!("platform={} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!(
        "parent_rss_start_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );

    fixture_import_reuse(&worker_binary)?;
    mathlib_shaped_fallback(&worker_binary)?;
    repeated_cycle_reuse(&worker_binary)?;

    println!(
        "parent_rss_end_kib={}",
        current_process_rss_kib().map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
    );
    println!("status=ok");
    Ok(())
}

fn fixture_import_reuse(worker_binary: &Path) -> Result<(), LeanWorkerError> {
    let mut pool = LeanWorkerPool::new(
        LeanWorkerPoolConfig::new(1)
            .max_total_child_rss_kib(u64::MAX)
            .idle_cycle_after(Duration::from_mins(10)),
    );
    for iteration in 1..=3 {
        let mut lease = pool.acquire_lease(builder(worker_binary))?;
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
    }
    print_snapshot("fixture_import_reuse", &pool);
    Ok(())
}

fn mathlib_shaped_fallback(worker_binary: &Path) -> Result<(), LeanWorkerError> {
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).max_total_child_rss_kib(u64::MAX));
    let mut lease = pool.acquire_lease(builder(worker_binary))?;
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
    print_snapshot("mathlib_shaped_import_set", &pool);
    Ok(())
}

fn repeated_cycle_reuse(worker_binary: &Path) -> Result<(), LeanWorkerError> {
    let builder = builder(worker_binary).restart_policy(LeanWorkerRestartPolicy::default().max_imports(1));
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).max_total_child_rss_kib(u64::MAX));
    for iteration in 1..=3 {
        let mut lease = pool.acquire_lease(builder.clone())?;
        let response = lease.run_json_command(
            &json_command(),
            &FixtureRequest {
                source: format!("max-import-cycle-{iteration}"),
            },
            None,
            None,
        )?;
        println!(
            "small_max_import_cycle iteration={iteration} accepted={} kind={}",
            response.accepted, response.kind
        );
    }
    print_snapshot("small_max_import_cycle", &pool);
    Ok(())
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
}

fn builder(worker_binary: &Path) -> LeanWorkerCapabilityBuilder {
    LeanWorkerCapabilityBuilder::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
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
