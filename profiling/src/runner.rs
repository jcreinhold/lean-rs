use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use lean_toolchain::LEAN_VERSION;

use crate::common::{git_output, platform, profiling_example, results_dir, timestamp_utc, workspace_root};
use crate::report_schema::{
    AdmissionSample, BaselineMode, DerivedWorkSample, EnvPair, ImportStatsSample, KeyValue, PerformanceReport,
    ProfileArtifact, ReplacementSample, ReportMetadata, RssCheckpoint, SessionReuseSample, TimingSample, WorkloadRun,
};
use crate::report_writer::write_report;

const LOCAL_RSS_CAP_KIB: u64 = 1_572_864;
const WORKER_POLICY_WIDEN_THRESHOLD_KIB: u64 = LOCAL_RSS_CAP_KIB * 70 / 100;

/// Collect a bounded profiling baseline and write JSON/Markdown artifacts.
///
/// # Errors
///
/// Returns an error if profiling binaries cannot be built, a required workload cannot be spawned,
/// raw output cannot be written, or the final report artifacts cannot be generated.
pub fn collect(mode: BaselineMode) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    fs::create_dir_all(results_dir())?;
    build_profiling_binaries()?;

    let mut report = PerformanceReport {
        metadata: metadata(),
        baseline_mode: mode,
        workloads: Vec::new(),
        profiles: Vec::new(),
        notes: vec![
            "Same-process fresh imports retain Lean process-global state; bounded resource exhaustion is an expected safe outcome for that workload.".to_owned(),
            "Worker workloads should be interpreted as production-shape process-boundary measurements, not in-process Lean runtime recycling.".to_owned(),
        ],
    };

    report.workloads.push(run_long_session("fresh-import")?);
    report.workloads.push(run_long_session("pooled-reuse")?);
    report.workloads.push(run_long_session("steady-state")?);
    report.workloads.push(run_long_session("import-matrix")?);
    report.workloads.push(run_long_session("bracketed-lightweight")?);
    report.workloads.push(run_long_session("derived-indexes")?);
    let worker_single_import = run_worker_cycling(1)?;
    let run_two_import_candidate = worker_single_import
        .peak_rss_kib
        .is_some_and(|peak| peak <= WORKER_POLICY_WIDEN_THRESHOLD_KIB);
    report.workloads.push(worker_single_import);
    if run_two_import_candidate {
        report.workloads.push(run_worker_cycling(2)?);
    } else {
        report.notes.push(format!(
            "Skipped worker-cycling max_imports=2 candidate because max_imports=1 peak RSS exceeded the 70% widening threshold ({WORKER_POLICY_WIDEN_THRESHOLD_KIB} KiB of {LOCAL_RSS_CAP_KIB} KiB).",
        ));
    }
    report.workloads.push(run_pool_memory()?);
    report.workloads.push(run_criterion(
        "host-query-declarations-bulk-16",
        &[
            "bench",
            "-p",
            "lean-rs-host",
            "--bench",
            "session",
            "--",
            "host::session::query_declarations_bulk/16",
            "--warm-up-time",
            "1",
            "--measurement-time",
            "3",
            "--sample-size",
            "20",
        ],
    )?);
    report.workloads.push(run_criterion(
        "host-session-reuse-hit",
        &[
            "bench",
            "-p",
            "lean-rs-host",
            "--bench",
            "session",
            "--",
            "host::pool::session_reuse_hit",
            "--warm-up-time",
            "1",
            "--measurement-time",
            "3",
            "--sample-size",
            "20",
        ],
    )?);
    report.workloads.push(run_criterion(
        "worker-first-import-open-session",
        &[
            "bench",
            "-p",
            "lean-rs-worker-child",
            "--bench",
            "worker_capability",
            "--",
            "worker::capability_shape/first_import_open_session",
            "--warm-up-time",
            "1",
            "--measurement-time",
            "3",
            "--sample-size",
            "10",
        ],
    )?);

    if mode == BaselineMode::Full {
        report.profiles.extend(record_samply_profiles()?);
    }

    write_report(&report)
}

fn metadata() -> ReportMetadata {
    ReportMetadata {
        timestamp_utc: timestamp_utc(),
        platform: platform(),
        git_commit: git_output(&["rev-parse", "--short", "HEAD"]),
        git_branch: git_output(&["branch", "--show-current"]),
        lean_version: LEAN_VERSION.to_owned(),
        tooling: "cargo profile=profiling, samply optional".to_owned(),
    }
}

fn build_profiling_binaries() -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace_root())
        .env(
            "RUSTFLAGS",
            std::env::var("RUSTFLAGS")
                .unwrap_or_else(|_| "-C target-cpu=native -C force-frame-pointers=yes".to_owned()),
        )
        .args([
            "build",
            "--profile",
            "profiling",
            "-p",
            "lean-rs-host",
            "--example",
            "long_session_memory",
            "-p",
            "lean-rs-worker-child",
            "--bin",
            "lean-rs-worker-child",
            "--example",
            "memory_cycling",
            "--example",
            "pool_memory_scheduling",
        ]);
    let output = command.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "failed to build profiling binaries\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

fn run_long_session(mode: &str) -> Result<WorkloadRun, Box<dyn Error>> {
    let mut env = default_env();
    env.push(env_pair("LEAN_RS_LONG_SESSION_MODE", mode));
    env.push(env_pair("LEAN_RS_LONG_SESSION_IMPORTS", "1"));
    env.push(env_pair("LEAN_RS_LONG_SESSION_BULK", "64"));
    env.push(env_pair("LEAN_RS_LONG_SESSION_ELAB", "64"));
    env.push(env_pair("LEAN_RS_LONG_SESSION_POOL_CAPACITY", "1"));
    env.push(env_pair("LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY", "1"));
    env.push(env_pair(
        "LEAN_RS_LONG_SESSION_MAX_RSS_KIB",
        &LOCAL_RSS_CAP_KIB.to_string(),
    ));
    let binary = profiling_example("long_session_memory");
    run_binary(&format!("long-session-{mode}"), &binary, &[], env)
}

fn run_worker_cycling(max_imports: u64) -> Result<WorkloadRun, Box<dyn Error>> {
    let mut env = default_env();
    env.push(env_pair("LEAN_RS_WORKER_MEMORY_IMPORTS", "6"));
    env.push(env_pair("LEAN_RS_WORKER_MEMORY_MAX_IMPORTS", &max_imports.to_string()));
    env.push(env_pair(
        "LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB",
        &LOCAL_RSS_CAP_KIB.to_string(),
    ));
    let binary = profiling_example("memory_cycling");
    run_binary(&format!("worker-cycling-max-imports-{max_imports}"), &binary, &[], env)
}

fn run_pool_memory() -> Result<WorkloadRun, Box<dyn Error>> {
    let mut env = default_env();
    env.push(env_pair("LEAN_RS_POOL_MEMORY_MAX_WORKERS", "1"));
    env.push(env_pair(
        "LEAN_RS_POOL_MEMORY_TOTAL_RSS_KIB",
        &LOCAL_RSS_CAP_KIB.to_string(),
    ));
    env.push(env_pair(
        "LEAN_RS_POOL_MEMORY_PER_WORKER_RSS_KIB",
        &LOCAL_RSS_CAP_KIB.to_string(),
    ));
    env.push(env_pair("LEAN_RS_POOL_MEMORY_MAX_IMPORTS", "1"));
    let binary = profiling_example("pool_memory_scheduling");
    run_binary("pool-memory", &binary, &[], env)
}

fn run_criterion(name: &str, args: &[&str]) -> Result<WorkloadRun, Box<dyn Error>> {
    run_command(name, "cargo", args, default_env())
}

fn record_samply_profiles() -> Result<Vec<ProfileArtifact>, Box<dyn Error>> {
    if !command_exists("samply") {
        return Ok(vec![ProfileArtifact {
            workload: "samply".to_owned(),
            path: String::new(),
            status: "samply not found; install with `cargo install samply` to capture CPU profiles".to_owned(),
        }]);
    }

    Ok(vec![
        record_samply(
            "long-session-fresh-import",
            &profiling_example("long_session_memory"),
            &[
                env_pair("LEAN_RS_LONG_SESSION_MODE", "fresh-import"),
                env_pair("LEAN_RS_LONG_SESSION_IMPORTS", "1"),
                env_pair("LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY", "1"),
                env_pair("LEAN_RS_LONG_SESSION_MAX_RSS_KIB", &LOCAL_RSS_CAP_KIB.to_string()),
            ],
        )?,
        record_samply(
            "worker-cycling",
            &profiling_example("memory_cycling"),
            &[
                env_pair("LEAN_RS_WORKER_MEMORY_IMPORTS", "6"),
                env_pair("LEAN_RS_WORKER_MEMORY_MAX_IMPORTS", "1"),
                env_pair("LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB", &LOCAL_RSS_CAP_KIB.to_string()),
            ],
        )?,
    ])
}

fn record_samply(workload: &str, binary: &Path, env: &[EnvPair]) -> Result<ProfileArtifact, Box<dyn Error>> {
    let output_path = results_dir().join(format!("{workload}.json.gz"));
    let mut command = Command::new("samply");
    command.current_dir(workspace_root()).args([
        "record",
        "--save-only",
        "--output",
        &output_path.display().to_string(),
        "--profile-name",
        workload,
        "--",
        &binary.display().to_string(),
    ]);
    let mut combined_env = default_env();
    combined_env.extend_from_slice(env);
    apply_env(&mut command, &combined_env);
    let output = command.output()?;
    let stdout_path = write_raw(&format!("samply-{workload}"), "stdout", &output.stdout)?;
    let stderr_path = write_raw(&format!("samply-{workload}"), "stderr", &output.stderr)?;
    let status = if output.status.success() {
        analyze_profile(&output_path).unwrap_or_else(|err| {
            format!("profile captured; symbol analysis failed: {err}; open in Firefox Profiler for native Lean frames")
        })
    } else {
        format!(
            "samply failed; stdout={} stderr={}",
            stdout_path.display(),
            stderr_path.display()
        )
    };
    Ok(ProfileArtifact {
        workload: workload.to_owned(),
        path: output_path.display().to_string(),
        status,
    })
}

fn analyze_profile(path: &Path) -> Result<String, Box<dyn Error>> {
    let script = workspace_root()
        .join("profiling")
        .join("scripts")
        .join("analyze_samply_symbols.py");
    if !script.is_file() {
        return Ok("profile captured; analyzer script unavailable; open in Firefox Profiler".to_owned());
    }
    let output = Command::new("python3").arg(script).arg(path).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string().into())
    }
}

fn run_binary(name: &str, binary: &Path, args: &[&str], env: Vec<EnvPair>) -> Result<WorkloadRun, Box<dyn Error>> {
    run_command_path(name, binary, args, env)
}

fn run_command(name: &str, program: &str, args: &[&str], env: Vec<EnvPair>) -> Result<WorkloadRun, Box<dyn Error>> {
    let path = PathBuf::from(program);
    run_command_path(name, &path, args, env)
}

fn run_command_path(
    name: &str,
    program: &Path,
    args: &[&str],
    env: Vec<EnvPair>,
) -> Result<WorkloadRun, Box<dyn Error>> {
    let mut command = Command::new(program);
    command.current_dir(workspace_root()).args(args);
    apply_env(&mut command, &env);

    let start = Instant::now();
    let output = command.output()?;
    let wall_time_ms = start.elapsed().as_secs_f64() * 1_000.0;

    let stdout_path = write_raw(name, "stdout", &output.stdout)?;
    let stderr_path = write_raw(name, "stderr", &output.stderr)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed = parse_key_values(&stdout, &stderr);

    Ok(WorkloadRun {
        name: name.to_owned(),
        command: command_display(program, args),
        env,
        exit_success: output.status.success(),
        exit_code: output.status.code(),
        wall_time_ms,
        status: find_last_value(&parsed.key_values, "status").or_else(|| {
            if output.status.success() {
                Some("ok".to_owned())
            } else {
                None
            }
        }),
        peak_rss_kib: parsed.peak_rss_kib,
        checkpoints: parsed.checkpoints,
        import_stats: parsed.import_stats,
        derived_work: parsed.derived_work,
        timings: parsed.timings,
        admissions: parsed.admissions,
        session_reuse: parsed.session_reuse,
        replacements: parsed.replacements,
        key_values: parsed.key_values,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
    })
}

fn apply_env(command: &mut Command, env: &[EnvPair]) {
    for pair in env {
        command.env(&pair.key, &pair.value);
    }
}

fn write_raw(name: &str, stream: &str, bytes: &[u8]) -> Result<PathBuf, Box<dyn Error>> {
    let path = results_dir().join(format!("{name}.{stream}.txt"));
    fs::write(&path, bytes)?;
    Ok(path)
}

fn parse_key_values(output: &str, stderr: &str) -> ParsedOutput {
    let mut key_values = Vec::new();
    let mut checkpoints = Vec::new();
    let mut import_stats = Vec::new();
    let mut derived_work = Vec::new();
    let mut timings = Vec::new();
    let mut admissions = Vec::new();
    let mut session_reuse = Vec::new();
    let mut replacements = Vec::new();
    let mut peak_rss_kib = None;

    for line in output.lines() {
        let mut line_pairs = Vec::new();
        for token in line.split_whitespace() {
            if let Some((key, value)) = token.split_once('=') {
                line_pairs.push((key.to_owned(), value.to_owned()));
                key_values.push(KeyValue {
                    key: key.to_owned(),
                    value: value.to_owned(),
                });
            }
        }

        let checkpoint = line_pairs
            .iter()
            .find(|(key, _value)| key == "checkpoint")
            .map(|(_key, value)| value.clone());
        let rss = line_pairs
            .iter()
            .find(|(key, _value)| is_observed_rss_key(key))
            .and_then(|(_key, value)| value.parse::<u64>().ok());

        if let Some(rss_kib) = rss {
            peak_rss_kib = Some(peak_rss_kib.map_or(rss_kib, |current: u64| current.max(rss_kib)));
            if let Some(stage) = checkpoint {
                checkpoints.push(RssCheckpoint { stage, rss_kib });
            }
        }

        if let Some(stats) = parse_import_stats(&line_pairs) {
            import_stats.push(stats);
        }
        if let Some(sample) = parse_derived_work(&line_pairs) {
            derived_work.push(sample);
        }
        if let Some(sample) = parse_timing(&line_pairs) {
            timings.push(sample);
        }
        if let Some(sample) = parse_admission(&line_pairs) {
            admissions.push(sample);
        }
        if let Some(sample) = parse_session_reuse(&line_pairs) {
            session_reuse.push(sample);
        }
        if let Some(sample) = parse_replacement(&line_pairs) {
            replacements.push(sample);
        }
    }

    if stderr.contains("lazy discriminator import initialization") {
        if derived_work.len() == 1 {
            if let Some(sample) = derived_work.first_mut() {
                sample.lazy_discr_tree_import_initialization_observed = true;
            }
        } else {
            derived_work.push(DerivedWorkSample {
                label: "lean_profiler_stderr".to_owned(),
                iteration: None,
                source_range_lookups: 0,
                docstring_lookups: 0,
                raw_type_renderings: 0,
                pretty_prints: 0,
                proof_search_fact_collections: 0,
                simp_extension_lookups: 0,
                parser_elaborator_runs: 0,
                module_snapshot_builds: 0,
                lazy_discr_tree_import_initialization_observed: true,
            });
        }
    }

    ParsedOutput {
        key_values,
        checkpoints,
        import_stats,
        derived_work,
        timings,
        admissions,
        session_reuse,
        replacements,
        peak_rss_kib,
    }
}

fn parse_import_stats(pairs: &[(String, String)]) -> Option<ImportStatsSample> {
    let label = value(pairs, "import_stats")
        .or_else(|| value(pairs, "bracketed_import_stats"))?
        .to_owned();
    let imported_bytes = parse_u64(pairs, "imported_bytes")?;
    let compacted_region_bytes = parse_u64(pairs, "compacted_region_bytes").unwrap_or(imported_bytes);
    Some(ImportStatsSample {
        label,
        iteration: value(pairs, "iteration").and_then(|value| value.parse::<u64>().ok()),
        profile_mode: value(pairs, "profile_mode").unwrap_or("unknown").to_owned(),
        direct_imports: value(pairs, "direct_imports")
            .unwrap_or_default()
            .split(',')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect(),
        effective_modules: parse_u64(pairs, "effective_modules")?,
        compacted_regions: parse_u64(pairs, "compacted_regions")?,
        memory_mapped_regions: parse_u64(pairs, "memory_mapped_regions")?,
        compacted_region_bytes,
        memory_mapped_region_bytes: parse_u64(pairs, "memory_mapped_region_bytes"),
        non_memory_mapped_region_bytes: parse_u64(pairs, "non_memory_mapped_region_bytes"),
        imported_bytes,
        imported_constants: parse_u64(pairs, "imported_constants")?,
        extension_count: parse_u64(pairs, "extension_count")?,
        total_imported_extension_entries: parse_u64(pairs, "total_imported_extension_entries")?,
        import_level: value(pairs, "import_level").unwrap_or("unknown").to_owned(),
        import_all: parse_bool(pairs, "import_all")?,
        load_exts: parse_bool(pairs, "load_exts")?,
        free_regions_ran: value(pairs, "free_regions_ran").and_then(|value| value.parse().ok()),
    })
}

fn parse_derived_work(pairs: &[(String, String)]) -> Option<DerivedWorkSample> {
    Some(DerivedWorkSample {
        label: value(pairs, "query_derived_work")?.to_owned(),
        iteration: value(pairs, "iteration").and_then(|value| value.parse::<u64>().ok()),
        source_range_lookups: parse_u64(pairs, "source_range_lookups")?,
        docstring_lookups: parse_u64(pairs, "docstring_lookups")?,
        raw_type_renderings: parse_u64(pairs, "raw_type_renderings")?,
        pretty_prints: parse_u64(pairs, "pretty_prints")?,
        proof_search_fact_collections: parse_u64(pairs, "proof_search_fact_collections")?,
        simp_extension_lookups: parse_u64(pairs, "simp_extension_lookups")?,
        parser_elaborator_runs: parse_u64(pairs, "parser_elaborator_runs")?,
        module_snapshot_builds: parse_u64(pairs, "module_snapshot_builds")?,
        lazy_discr_tree_import_initialization_observed: parse_bool(
            pairs,
            "lazy_discr_tree_import_initialization_observed",
        )?,
    })
}

fn parse_timing(pairs: &[(String, String)]) -> Option<TimingSample> {
    let label = value(pairs, "session_open_timing")
        .or_else(|| value(pairs, "pool_request_timing"))?
        .to_owned();
    Some(TimingSample {
        label,
        iteration: value(pairs, "iteration").and_then(|value| value.parse::<u64>().ok()),
        kind: value(pairs, "kind").unwrap_or("unknown").to_owned(),
        elapsed_ms: value(pairs, "elapsed_ms")?.parse::<f64>().ok()?,
        rss_kib: parse_u64(pairs, "rss_kib"),
        workers: parse_u64(pairs, "workers"),
        total_child_rss_kib: parse_u64(pairs, "total_child_rss_kib"),
        worker_restarts: parse_u64(pairs, "worker_restarts"),
        max_import_restarts: parse_u64(pairs, "max_import_restarts"),
        policy_restarts: parse_u64(pairs, "policy_restarts"),
    })
}

fn parse_admission(pairs: &[(String, String)]) -> Option<AdmissionSample> {
    let label = value(pairs, "admission")?.to_owned();
    let refusal_reason =
        value(pairs, "refusal_reason").and_then(|value| if value == "none" { None } else { Some(value.to_owned()) });
    Some(AdmissionSample {
        label,
        iteration: value(pairs, "iteration").and_then(|value| value.parse::<u64>().ok()),
        kind: value(pairs, "kind").unwrap_or("unknown").to_owned(),
        cold_open_attempts: parse_u64(pairs, "cold_open_attempts")?,
        cold_open_admitted: parse_u64(pairs, "cold_open_admitted")?,
        cold_open_refusals: parse_u64(pairs, "cold_open_refusals")?,
        import_like_requests: parse_u64(pairs, "import_like_requests")?,
        import_like_admitted: parse_u64(pairs, "import_like_admitted"),
        concurrent_cold_opens_observed: parse_u64(pairs, "concurrent_cold_opens_observed")?,
        rss_before_admission_kib: parse_u64(pairs, "rss_before_admission_kib"),
        rss_after_open_kib: parse_u64(pairs, "rss_after_open_kib"),
        refusal_reason,
    })
}

fn parse_session_reuse(pairs: &[(String, String)]) -> Option<SessionReuseSample> {
    let label = value(pairs, "session_reuse")?.to_owned();
    let last_miss_reason =
        value(pairs, "last_miss_reason").and_then(|value| if value == "none" { None } else { Some(value.to_owned()) });
    Some(SessionReuseSample {
        label,
        iteration: value(pairs, "iteration").and_then(|value| value.parse::<u64>().ok()),
        layer: value(pairs, "layer").unwrap_or("unknown").to_owned(),
        key_hits: parse_u64(pairs, "key_hits")?,
        key_misses: parse_u64(pairs, "key_misses")?,
        distinct_keys_seen: parse_u64(pairs, "distinct_keys_seen")?,
        fresh_imports_avoided: parse_u64(pairs, "fresh_imports_avoided")?,
        miss_empty_pool: parse_u64(pairs, "miss_empty_pool")?,
        miss_reuse_disabled: parse_u64(pairs, "miss_reuse_disabled").unwrap_or(0),
        miss_no_matching_key: parse_u64(pairs, "miss_no_matching_key")?,
        last_miss_reason,
    })
}

fn parse_replacement(pairs: &[(String, String)]) -> Option<ReplacementSample> {
    let label = value(pairs, "replacement")?.to_owned();
    let replacement_reason = value(pairs, "replacement_reason")
        .and_then(|value| if value == "none" { None } else { Some(value.to_owned()) });
    let replacement_budget_status = value(pairs, "replacement_budget_status")
        .and_then(|value| if value == "none" { None } else { Some(value.to_owned()) });
    let skipped_reason =
        value(pairs, "skipped_reason").and_then(|value| if value == "none" { None } else { Some(value.to_owned()) });
    Some(ReplacementSample {
        label,
        iteration: value(pairs, "iteration").and_then(|value| value.parse::<u64>().ok()),
        kind: value(pairs, "kind").unwrap_or("unknown").to_owned(),
        replacement_attempts: parse_u64(pairs, "replacement_attempts")?,
        replacement_successes: parse_u64(pairs, "replacement_successes")?,
        replacement_failures: parse_u64(pairs, "replacement_failures")?,
        replacement_budget_admitted: parse_u64(pairs, "replacement_budget_admitted")?,
        replacement_budget_skipped: parse_u64(pairs, "replacement_budget_skipped")?,
        spawn_handshake_ms: parse_f64(pairs, "spawn_handshake_ms"),
        capability_load_ms: parse_f64(pairs, "capability_load_ms"),
        session_open_import_ms: parse_f64(pairs, "session_open_import_ms"),
        first_command_ms: parse_f64(pairs, "first_command_ms"),
        warm_command_ms: parse_f64(pairs, "warm_command_ms"),
        replacement_total_ms: parse_f64(pairs, "replacement_total_ms"),
        replacement_reason,
        replacement_budget_status,
        skipped_reason,
    })
}

fn value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(candidate, _value)| candidate == key)
        .map(|(_key, value)| value.as_str())
}

fn parse_u64(pairs: &[(String, String)], key: &str) -> Option<u64> {
    value(pairs, key)?.parse().ok()
}

fn parse_f64(pairs: &[(String, String)], key: &str) -> Option<f64> {
    value(pairs, key)?.parse().ok()
}

fn parse_bool(pairs: &[(String, String)], key: &str) -> Option<bool> {
    value(pairs, key)?.parse().ok()
}

fn is_observed_rss_key(key: &str) -> bool {
    matches!(
        key,
        "rss_kib" | "total_child_rss_kib" | "parent_rss_start_kib" | "parent_rss_end_kib"
    )
}

fn find_last_value(values: &[KeyValue], key: &str) -> Option<String> {
    values
        .iter()
        .rev()
        .find(|value| value.key == key)
        .map(|value| value.value.clone())
}

fn command_display(program: &Path, args: &[&str]) -> String {
    let mut parts = vec![program.display().to_string()];
    parts.extend(args.iter().map(|arg| (*arg).to_owned()));
    parts.join(" ")
}

fn default_env() -> Vec<EnvPair> {
    let mut env = vec![env_pair("LEAN_RS_NUM_THREADS", "1")];
    if let Ok(limit) = std::env::var("LEAN_RS_LEAN_MAX_MEMORY_KIB")
        && !limit.is_empty()
    {
        env.push(env_pair("LEAN_RS_LEAN_MAX_MEMORY_KIB", &limit));
    }
    env
}

fn env_pair(key: &str, value: &str) -> EnvPair {
    EnvPair {
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

struct ParsedOutput {
    key_values: Vec<KeyValue>,
    checkpoints: Vec<RssCheckpoint>,
    import_stats: Vec<ImportStatsSample>,
    derived_work: Vec<DerivedWorkSample>,
    timings: Vec<TimingSample>,
    admissions: Vec<AdmissionSample>,
    session_reuse: Vec<SessionReuseSample>,
    replacements: Vec<ReplacementSample>,
    peak_rss_kib: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::parse_key_values;

    #[test]
    fn parses_import_stats_lines() {
        let parsed = parse_key_values(
            "import_stats=import_matrix iteration=2 profile_mode=exported-public direct_imports=Init,Lean effective_modules=12 compacted_regions=12 memory_mapped_regions=11 compacted_region_bytes=4096 memory_mapped_region_bytes=3072 non_memory_mapped_region_bytes=1024 imported_bytes=4096 imported_constants=500 extension_count=3 total_imported_extension_entries=7 import_level=exported import_all=false load_exts=true",
            "",
        );
        assert_eq!(parsed.import_stats.len(), 1);
        let stats = &parsed.import_stats[0];
        assert_eq!(stats.label, "import_matrix");
        assert_eq!(stats.iteration, Some(2));
        assert_eq!(stats.profile_mode, "exported-public");
        assert_eq!(stats.direct_imports, ["Init", "Lean"]);
        assert_eq!(stats.effective_modules, 12);
        assert_eq!(stats.compacted_region_bytes, 4096);
        assert_eq!(stats.memory_mapped_region_bytes, Some(3072));
        assert_eq!(stats.non_memory_mapped_region_bytes, Some(1024));
        assert_eq!(stats.imported_bytes, stats.compacted_region_bytes);
        assert_eq!(stats.import_level, "exported");
        assert!(!stats.import_all);
        assert!(stats.load_exts);
        assert_eq!(stats.free_regions_ran, None);
    }

    #[test]
    fn parses_legacy_import_stats_lines_with_imported_bytes_alias() {
        let parsed = parse_key_values(
            "import_stats=legacy iteration=1 profile_mode=private direct_imports=Init effective_modules=1 compacted_regions=1 memory_mapped_regions=0 imported_bytes=256 imported_constants=2 extension_count=0 total_imported_extension_entries=0 import_level=private import_all=false load_exts=true",
            "",
        );
        let stats = &parsed.import_stats[0];
        assert_eq!(stats.compacted_region_bytes, 256);
        assert_eq!(stats.memory_mapped_region_bytes, None);
        assert_eq!(stats.non_memory_mapped_region_bytes, None);
    }

    #[test]
    fn parses_bracketed_import_stats_lines() {
        let parsed = parse_key_values(
            "bracketed_import_stats=bracketed_lightweight iteration=1 profile_mode=bracketed-private-no-exts direct_imports=LeanRsFixture.Handles effective_modules=9 compacted_regions=10 memory_mapped_regions=0 compacted_region_bytes=8192 memory_mapped_region_bytes=0 non_memory_mapped_region_bytes=8192 imported_bytes=8192 imported_constants=12 extension_count=3 total_imported_extension_entries=4 import_level=private import_all=false load_exts=false free_regions_ran=true",
            "",
        );
        assert_eq!(parsed.import_stats.len(), 1);
        let stats = &parsed.import_stats[0];
        assert_eq!(stats.label, "bracketed_lightweight");
        assert_eq!(stats.profile_mode, "bracketed-private-no-exts");
        assert_eq!(stats.compacted_region_bytes, 8192);
        assert_eq!(stats.memory_mapped_region_bytes, Some(0));
        assert_eq!(stats.non_memory_mapped_region_bytes, Some(8192));
        assert!(!stats.load_exts);
        assert_eq!(stats.free_regions_ran, Some(true));
    }

    #[test]
    fn parses_worker_timing_lines() {
        let parsed = parse_key_values(
            "session_open_timing=worker_cycling iteration=2 kind=warm-same-child elapsed_ms=12.500 max_imports_per_child=2 max_rss_kib=1572864 rss_kib=700000",
            "",
        );
        assert_eq!(parsed.timings.len(), 1);
        let timing = &parsed.timings[0];
        assert_eq!(timing.label, "worker_cycling");
        assert_eq!(timing.iteration, Some(2));
        assert_eq!(timing.kind, "warm-same-child");
        assert_eq!(timing.elapsed_ms, 12.5);
        assert_eq!(timing.rss_kib, Some(700000));
    }

    #[test]
    fn parses_admission_lines() {
        let parsed = parse_key_values(
            "admission=worker_session_open iteration=2 kind=cold cold_open_attempts=1 cold_open_admitted=1 cold_open_refusals=0 import_like_requests=1 import_like_admitted=1 concurrent_cold_opens_observed=0 rss_before_admission_kib=100 rss_after_open_kib=700000 refusal_reason=none",
            "",
        );
        assert_eq!(parsed.admissions.len(), 1);
        let admission = &parsed.admissions[0];
        assert_eq!(admission.label, "worker_session_open");
        assert_eq!(admission.iteration, Some(2));
        assert_eq!(admission.kind, "cold");
        assert_eq!(admission.cold_open_attempts, 1);
        assert_eq!(admission.cold_open_admitted, 1);
        assert_eq!(admission.cold_open_refusals, 0);
        assert_eq!(admission.import_like_requests, 1);
        assert_eq!(admission.import_like_admitted, Some(1));
        assert_eq!(admission.concurrent_cold_opens_observed, 0);
        assert_eq!(admission.rss_before_admission_kib, Some(100));
        assert_eq!(admission.rss_after_open_kib, Some(700000));
        assert_eq!(admission.refusal_reason, None);
    }

    #[test]
    fn parses_session_reuse_lines() {
        let parsed = parse_key_values(
            "session_reuse=pooled-reuse iteration=2 layer=host key_hits=1 key_misses=1 distinct_keys_seen=1 fresh_imports_avoided=1 miss_empty_pool=1 miss_reuse_disabled=0 miss_no_matching_key=0 last_miss_reason=none",
            "",
        );
        assert_eq!(parsed.session_reuse.len(), 1);
        let sample = &parsed.session_reuse[0];
        assert_eq!(sample.label, "pooled-reuse");
        assert_eq!(sample.iteration, Some(2));
        assert_eq!(sample.layer, "host");
        assert_eq!(sample.key_hits, 1);
        assert_eq!(sample.key_misses, 1);
        assert_eq!(sample.distinct_keys_seen, 1);
        assert_eq!(sample.fresh_imports_avoided, 1);
        assert_eq!(sample.miss_empty_pool, 1);
        assert_eq!(sample.miss_reuse_disabled, 0);
        assert_eq!(sample.miss_no_matching_key, 0);
        assert_eq!(sample.last_miss_reason, None);
    }

    #[test]
    fn parses_replacement_lines() {
        let parsed = parse_key_values(
            "replacement=worker_session_open iteration=2 kind=cold replacement_attempts=1 replacement_successes=1 replacement_failures=0 replacement_budget_admitted=1 replacement_budget_skipped=0 spawn_handshake_ms=12.500 capability_load_ms=0.000 session_open_import_ms=90.250 first_command_ms=unavailable warm_command_ms=3.750 replacement_total_ms=15.000 replacement_reason=max_imports replacement_budget_status=synchronous-no-overlap skipped_reason=none",
            "",
        );
        assert_eq!(parsed.replacements.len(), 1);
        let sample = &parsed.replacements[0];
        assert_eq!(sample.label, "worker_session_open");
        assert_eq!(sample.iteration, Some(2));
        assert_eq!(sample.kind, "cold");
        assert_eq!(sample.replacement_attempts, 1);
        assert_eq!(sample.replacement_successes, 1);
        assert_eq!(sample.replacement_failures, 0);
        assert_eq!(sample.replacement_budget_admitted, 1);
        assert_eq!(sample.replacement_budget_skipped, 0);
        assert_eq!(sample.spawn_handshake_ms, Some(12.5));
        assert_eq!(sample.capability_load_ms, Some(0.0));
        assert_eq!(sample.session_open_import_ms, Some(90.25));
        assert_eq!(sample.first_command_ms, None);
        assert_eq!(sample.warm_command_ms, Some(3.75));
        assert_eq!(sample.replacement_total_ms, Some(15.0));
        assert_eq!(sample.replacement_reason.as_deref(), Some("max_imports"));
        assert_eq!(
            sample.replacement_budget_status.as_deref(),
            Some("synchronous-no-overlap")
        );
        assert_eq!(sample.skipped_reason, None);
    }

    #[test]
    fn parses_derived_work_lines() {
        let parsed = parse_key_values(
            "query_derived_work=proof_search_inspection iteration=1 source_range_lookups=1 docstring_lookups=0 raw_type_renderings=0 pretty_prints=1 proof_search_fact_collections=1 simp_extension_lookups=1 parser_elaborator_runs=0 module_snapshot_builds=0 lazy_discr_tree_import_initialization_observed=false",
            "",
        );
        assert_eq!(parsed.derived_work.len(), 1);
        let sample = &parsed.derived_work[0];
        assert_eq!(sample.label, "proof_search_inspection");
        assert_eq!(sample.iteration, Some(1));
        assert_eq!(sample.pretty_prints, 1);
        assert_eq!(sample.proof_search_fact_collections, 1);
        assert!(!sample.lazy_discr_tree_import_initialization_observed);
    }

    #[test]
    fn maps_lazy_discr_tree_stderr_span_to_single_derived_phase() {
        let parsed = parse_key_values(
            "query_derived_work=lazy_probe iteration=1 source_range_lookups=0 docstring_lookups=0 raw_type_renderings=0 pretty_prints=0 proof_search_fact_collections=0 simp_extension_lookups=0 parser_elaborator_runs=1 module_snapshot_builds=1 lazy_discr_tree_import_initialization_observed=false",
            "profiler: lazy discriminator import initialization 10ms",
        );
        assert_eq!(parsed.derived_work.len(), 1);
        assert!(parsed.derived_work[0].lazy_discr_tree_import_initialization_observed);
    }
}
