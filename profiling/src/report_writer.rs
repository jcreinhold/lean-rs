use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::results_dir;
use crate::report_schema::{
    AdmissionSample, BaselineMode, KeyValue, PerformanceReport, SessionReuseSample, TimingSample, WorkloadRun,
};

/// Write JSON and Markdown artifacts for a collected profiling report.
///
/// # Errors
///
/// Returns an error if the results directory cannot be created, the report cannot be serialized,
/// or either artifact cannot be written.
pub fn write_report(report: &PerformanceReport) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let dir = results_dir();
    fs::create_dir_all(&dir)?;

    let json_path = dir.join(format!("{}.json", report.baseline_mode.artifact_stem()));
    let markdown_path = dir.join(format!("{}.md", report.baseline_mode.report_stem()));

    let json = serde_json::to_string_pretty(report)?;
    fs::write(&json_path, json)?;
    fs::write(&markdown_path, render_markdown(report))?;
    Ok((json_path, markdown_path))
}

/// Regenerate a Markdown report from an existing JSON artifact.
///
/// # Errors
///
/// Returns an error if the JSON artifact cannot be read or parsed, or if the Markdown artifact
/// cannot be written.
pub fn regenerate(mode: BaselineMode) -> Result<PathBuf, Box<dyn Error>> {
    let json_path = results_dir().join(format!("{}.json", mode.artifact_stem()));
    let text = fs::read_to_string(&json_path)?;
    let report: PerformanceReport = serde_json::from_str(&text)?;
    let markdown_path = results_dir().join(format!("{}.md", mode.report_stem()));
    fs::write(&markdown_path, render_markdown(&report))?;
    Ok(markdown_path)
}

fn render_markdown(report: &PerformanceReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# lean-rs Profiling Baseline");
    let _ = writeln!(out);
    let _ = writeln!(out, "- Mode: {:?}", report.baseline_mode);
    let _ = writeln!(out, "- Timestamp: {}", report.metadata.timestamp_utc);
    let _ = writeln!(out, "- Platform: {}", report.metadata.platform);
    let _ = writeln!(
        out,
        "- Git: {} ({})",
        report.metadata.git_commit, report.metadata.git_branch
    );
    let _ = writeln!(out, "- Lean: {}", report.metadata.lean_version);
    let _ = writeln!(out, "- Tooling: {}", report.metadata.tooling);
    let _ = writeln!(out);

    render_run_configuration(&mut out, report);
    render_worker_policy_summary(&mut out, report);

    let _ = writeln!(out, "## Workloads");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Workload | Status | Wall ms | Peak RSS KiB | Exit | Raw output |"
    );
    let _ = writeln!(out, "| --- | --- | ---: | ---: | --- | --- |");
    for run in &report.workloads {
        let status = run.status.as_deref().unwrap_or("unknown");
        let peak = run
            .peak_rss_kib
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
        let exit = if run.exit_success {
            "success".to_owned()
        } else {
            format!("failed {:?}", run.exit_code)
        };
        let _ = writeln!(
            out,
            "| {} | {} | {:.2} | {} | {} | [{}]({}) |",
            run.name,
            status,
            run.wall_time_ms,
            peak,
            exit,
            file_name(&run.stdout_path),
            run.stdout_path
        );
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "## RSS Checkpoints");
    for run in &report.workloads {
        if run.checkpoints.is_empty() {
            continue;
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "### {}", run.name);
        let _ = writeln!(out);
        let _ = writeln!(out, "| Stage | RSS KiB |");
        let _ = writeln!(out, "| --- | ---: |");
        for checkpoint in visible_checkpoints(&run.checkpoints) {
            let _ = writeln!(out, "| {} | {} |", checkpoint.stage, checkpoint.rss_kib);
        }
    }

    let import_workloads: Vec<_> = report
        .workloads
        .iter()
        .filter(|run| !run.import_stats.is_empty())
        .collect();
    if !import_workloads.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Lean Import Stats");
        for run in import_workloads {
            let _ = writeln!(out);
            let _ = writeln!(out, "### {}", run.name);
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "| Label | Iteration | Mode | Imports | importAll | Level | loadExts | freeRegions | Modules | Regions | mmap | Total bytes | mmap bytes | non-mmap bytes | RSS gap | Constants | Exts | Entries |"
            );
            let _ = writeln!(
                out,
                "| --- | ---: | --- | --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
            );
            for stats in visible_import_stats(&run.import_stats) {
                let iteration = stats
                    .iteration
                    .map_or_else(|| String::from("-"), |value| value.to_string());
                let imports = if stats.direct_imports.is_empty() {
                    String::from("-")
                } else {
                    stats.direct_imports.join("<br>")
                };
                let free_regions = stats
                    .free_regions_ran
                    .map_or_else(|| String::from("-"), |value| value.to_string());
                let compacted_region_bytes = compacted_region_bytes(stats);
                let rss_gap = import_rss_gap(run.peak_rss_kib, compacted_region_bytes);
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    stats.label,
                    iteration,
                    stats.profile_mode,
                    imports,
                    stats.import_all,
                    stats.import_level,
                    stats.load_exts,
                    free_regions,
                    stats.effective_modules,
                    stats.compacted_regions,
                    stats.memory_mapped_regions,
                    compacted_region_bytes,
                    format_optional_u64(stats.memory_mapped_region_bytes),
                    format_optional_u64(stats.non_memory_mapped_region_bytes),
                    rss_gap,
                    stats.imported_constants,
                    stats.extension_count,
                    stats.total_imported_extension_entries
                );
            }
        }
    }

    let derived_workloads: Vec<_> = report
        .workloads
        .iter()
        .filter(|run| !run.derived_work.is_empty())
        .collect();
    if !derived_workloads.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Lean Derived Work");
        for run in derived_workloads {
            let _ = writeln!(out);
            let _ = writeln!(out, "### {}", run.name);
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "| Label | Iteration | Source Ranges | Docstrings | Raw Types | Pretty Prints | Proof Facts | Simp Ext | Parser/Elab | Snapshots | Lazy Import Init |"
            );
            let _ = writeln!(
                out,
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
            );
            for sample in &run.derived_work {
                let iteration = sample
                    .iteration
                    .map_or_else(|| String::from("-"), |value| value.to_string());
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    sample.label,
                    iteration,
                    sample.source_range_lookups,
                    sample.docstring_lookups,
                    sample.raw_type_renderings,
                    sample.pretty_prints,
                    sample.proof_search_fact_collections,
                    sample.simp_extension_lookups,
                    sample.parser_elaborator_runs,
                    sample.module_snapshot_builds,
                    sample.lazy_discr_tree_import_initialization_observed
                );
            }
        }
    }

    let timing_workloads: Vec<_> = report.workloads.iter().filter(|run| !run.timings.is_empty()).collect();
    if !timing_workloads.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Worker Timings");
        for run in timing_workloads {
            let _ = writeln!(out);
            let _ = writeln!(out, "### {}", run.name);
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "| Label | Iteration | Kind | Elapsed ms | RSS KiB | Workers | Total child RSS KiB | Restarts | Max-import restarts | Policy restarts |"
            );
            let _ = writeln!(
                out,
                "| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
            );
            for timing in visible_timings(&run.timings) {
                let iteration = timing
                    .iteration
                    .map_or_else(|| String::from("-"), |value| value.to_string());
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {:.3} | {} | {} | {} | {} | {} | {} |",
                    timing.label,
                    iteration,
                    timing.kind,
                    timing.elapsed_ms,
                    format_optional_u64(timing.rss_kib),
                    format_optional_u64(timing.workers),
                    format_optional_u64(timing.total_child_rss_kib),
                    format_optional_u64(timing.worker_restarts),
                    format_optional_u64(timing.max_import_restarts),
                    format_optional_u64(timing.policy_restarts)
                );
            }
        }
    }

    let admission_workloads: Vec<_> = report
        .workloads
        .iter()
        .filter(|run| !run.admissions.is_empty())
        .collect();
    if !admission_workloads.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Import Admission");
        for run in admission_workloads {
            let _ = writeln!(out);
            let _ = writeln!(out, "### {}", run.name);
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "| Label | Iteration | Kind | Cold attempts | Cold admitted | Cold refusals | Import-like requests | Import-like admitted | Concurrent cold opens | RSS before KiB | RSS after KiB | Refusal |"
            );
            let _ = writeln!(
                out,
                "| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
            );
            for admission in visible_admissions(&run.admissions) {
                let iteration = admission
                    .iteration
                    .map_or_else(|| String::from("-"), |value| value.to_string());
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    admission.label,
                    iteration,
                    admission.kind,
                    admission.cold_open_attempts,
                    admission.cold_open_admitted,
                    admission.cold_open_refusals,
                    admission.import_like_requests,
                    format_optional_u64(admission.import_like_admitted),
                    admission.concurrent_cold_opens_observed,
                    format_optional_u64(admission.rss_before_admission_kib),
                    format_optional_u64(admission.rss_after_open_kib),
                    admission.refusal_reason.as_deref().unwrap_or("-")
                );
            }
        }
    }

    let reuse_workloads: Vec<_> = report
        .workloads
        .iter()
        .filter(|run| !run.session_reuse.is_empty())
        .collect();
    if !reuse_workloads.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Session Reuse Keys");
        for run in reuse_workloads {
            let _ = writeln!(out);
            let _ = writeln!(out, "### {}", run.name);
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "| Label | Iteration | Layer | Key hits | Key misses | Distinct keys | Fresh imports avoided | Empty misses | Reuse-disabled misses | No-match misses | Last miss |"
            );
            let _ = writeln!(
                out,
                "| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
            );
            for sample in visible_session_reuse(&run.session_reuse) {
                let iteration = sample
                    .iteration
                    .map_or_else(|| String::from("-"), |value| value.to_string());
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    sample.label,
                    iteration,
                    sample.layer,
                    sample.key_hits,
                    sample.key_misses,
                    sample.distinct_keys_seen,
                    sample.fresh_imports_avoided,
                    sample.miss_empty_pool,
                    sample.miss_reuse_disabled,
                    sample.miss_no_matching_key,
                    sample.last_miss_reason.as_deref().unwrap_or("-")
                );
            }
        }
    }

    if !report.profiles.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## CPU Profiles");
        let _ = writeln!(out);
        for profile in &report.profiles {
            let _ = writeln!(
                out,
                "- `{}`: [{}]({}) - {}",
                profile.workload, profile.path, profile.path, profile.status
            );
        }
    }

    if !report.notes.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Notes");
        let _ = writeln!(out);
        for note in &report.notes {
            let _ = writeln!(out, "- {note}");
        }
    }

    out
}

fn render_run_configuration(out: &mut String, report: &PerformanceReport) {
    let _ = writeln!(out, "## Run Configuration");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Git `{}` on `{}`, platform `{}`, Lean `{}`.",
        report.metadata.git_commit, report.metadata.git_branch, report.metadata.platform, report.metadata.lean_version
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| Workload | Command | Environment | Stdout | Stderr |");
    let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
    for run in &report.workloads {
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | [{}]({}) | [{}]({}) |",
            run.name,
            run.command,
            format_env(&run.env),
            file_name(&run.stdout_path),
            run.stdout_path,
            file_name(&run.stderr_path),
            run.stderr_path
        );
    }
    if !report.profiles.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "| Profile Artifact | Path | Status |");
        let _ = writeln!(out, "| --- | --- | --- |");
        for profile in &report.profiles {
            let path = if profile.path.is_empty() {
                String::from("-")
            } else {
                format!("[{}]({})", file_name(&profile.path), profile.path)
            };
            let _ = writeln!(out, "| {} | {} | {} |", profile.workload, path, profile.status);
        }
    }
    let _ = writeln!(out);
}

fn render_worker_policy_summary(out: &mut String, report: &PerformanceReport) {
    let worker_one = report
        .workloads
        .iter()
        .find(|run| run.name == "worker-cycling-max-imports-1");
    let Some(worker_one) = worker_one else {
        return;
    };

    let worker_two = report
        .workloads
        .iter()
        .find(|run| run.name == "worker-cycling-max-imports-2");
    let cap_kib = key_value(&worker_one.key_values, "max_rss_kib")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1_572_864);
    let widen_threshold = cap_kib * 70 / 100;
    let recommended_max_imports = if worker_two
        .and_then(|run| run.peak_rss_kib)
        .is_some_and(|peak| peak <= cap_kib)
    {
        2
    } else {
        1
    };
    let pool = report.workloads.iter().find(|run| run.name == "pool-memory");
    let max_workers = pool
        .and_then(|run| key_value(&run.key_values, "max_workers"))
        .unwrap_or("1");
    let per_worker_rss = pool
        .and_then(|run| key_value(&run.key_values, "per_worker_rss_ceiling_kib"))
        .unwrap_or_else(|| key_value(&worker_one.key_values, "max_rss_kib").unwrap_or("1572864"));
    let total_child_rss = pool
        .and_then(|run| key_value(&run.key_values, "max_total_child_rss_kib"))
        .unwrap_or(per_worker_rss);

    let _ = writeln!(out, "## Worker Policy Summary");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Local recommendation under the {} KiB RSS budget: `LeanWorkerRestartPolicy::memory_bounded({}, {})` with `LeanWorkerPoolConfig::new({}).per_worker_rss_ceiling_kib({}).max_total_child_rss_kib({})`.",
        cap_kib, recommended_max_imports, cap_kib, max_workers, per_worker_rss, total_child_rss
    );
    let _ = writeln!(
        out,
        "The `max_imports=2` candidate is collected only when the `max_imports=1` run stays at or below the 70% widening threshold ({} KiB).",
        widen_threshold
    );
    if let Some(worker_two) = worker_two
        && worker_two.peak_rss_kib.is_some_and(|peak| peak > cap_kib)
    {
        let _ = writeln!(
            out,
            "`max_imports=2` was measured but is not recommended for this local cap because its peak RSS exceeded the budget."
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Candidate | Status | Peak RSS KiB | Cold open ms | Warm open ms | Restarts |"
    );
    let _ = writeln!(out, "| --- | --- | ---: | ---: | ---: | ---: |");
    render_worker_candidate_row(out, worker_one, "max_imports=1");
    if let Some(worker_two) = worker_two {
        render_worker_candidate_row(out, worker_two, "max_imports=2");
    } else {
        let _ = writeln!(out, "| max_imports=2 | skipped | - | - | - | - |");
    }
    let _ = writeln!(out);
}

fn render_worker_candidate_row(out: &mut String, run: &WorkloadRun, label: &str) {
    let status = run.status.as_deref().unwrap_or("unknown");
    let peak = run
        .peak_rss_kib
        .map_or_else(|| String::from("-"), |value| value.to_string());
    let cold = first_timing_ms(&run.timings, "cold");
    let warm = first_timing_ms(&run.timings, "warm-same-child");
    let restarts = key_value(&run.key_values, "restarts").unwrap_or("-");
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} | {} |",
        label, status, peak, cold, warm, restarts
    );
}

fn first_timing_ms(timings: &[TimingSample], kind: &str) -> String {
    timings
        .iter()
        .find(|timing| timing.kind == kind)
        .map_or_else(|| String::from("-"), |timing| format!("{:.3}", timing.elapsed_ms))
}

fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(path)
}

fn format_env(env: &[crate::report_schema::EnvPair]) -> String {
    if env.is_empty() {
        return String::from("-");
    }
    env.iter()
        .map(|pair| format!("`{}={}`", pair.key, pair.value))
        .collect::<Vec<_>>()
        .join("<br>")
}

fn key_value<'a>(values: &'a [KeyValue], key: &str) -> Option<&'a str> {
    values
        .iter()
        .rev()
        .find(|value| value.key == key)
        .map(|value| value.value.as_str())
}

fn visible_checkpoints(
    checkpoints: &[crate::report_schema::RssCheckpoint],
) -> Vec<&crate::report_schema::RssCheckpoint> {
    const HEAD: usize = 20;
    const TAIL: usize = 5;
    if checkpoints.len() <= HEAD + TAIL {
        return checkpoints.iter().collect();
    }
    let mut visible: Vec<_> = checkpoints.iter().take(HEAD).collect();
    visible.extend(checkpoints.iter().skip(checkpoints.len().saturating_sub(TAIL)));
    visible
}

fn visible_import_stats(
    stats: &[crate::report_schema::ImportStatsSample],
) -> Vec<&crate::report_schema::ImportStatsSample> {
    const HEAD: usize = 20;
    const TAIL: usize = 5;
    if stats.len() <= HEAD + TAIL {
        return stats.iter().collect();
    }
    let mut visible: Vec<_> = stats.iter().take(HEAD).collect();
    visible.extend(stats.iter().skip(stats.len().saturating_sub(TAIL)));
    visible
}

fn visible_timings(timings: &[crate::report_schema::TimingSample]) -> Vec<&crate::report_schema::TimingSample> {
    const HEAD: usize = 20;
    const TAIL: usize = 5;
    if timings.len() <= HEAD + TAIL {
        return timings.iter().collect();
    }
    let mut visible: Vec<_> = timings.iter().take(HEAD).collect();
    visible.extend(timings.iter().skip(timings.len().saturating_sub(TAIL)));
    visible
}

fn visible_admissions(admissions: &[AdmissionSample]) -> Vec<&AdmissionSample> {
    const HEAD: usize = 20;
    const TAIL: usize = 5;
    if admissions.len() <= HEAD + TAIL {
        return admissions.iter().collect();
    }
    let mut visible: Vec<_> = admissions.iter().take(HEAD).collect();
    visible.extend(admissions.iter().skip(admissions.len().saturating_sub(TAIL)));
    visible
}

fn visible_session_reuse(samples: &[SessionReuseSample]) -> Vec<&SessionReuseSample> {
    const HEAD: usize = 20;
    const TAIL: usize = 5;
    if samples.len() <= HEAD + TAIL {
        return samples.iter().collect();
    }
    let mut visible: Vec<_> = samples.iter().take(HEAD).collect();
    visible.extend(samples.iter().skip(samples.len().saturating_sub(TAIL)));
    visible
}

fn format_optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| String::from("-"), |value| value.to_string())
}

fn import_rss_gap(peak_rss_kib: Option<u64>, compacted_region_bytes: u64) -> String {
    let Some(peak_rss_kib) = peak_rss_kib else {
        return String::from("-");
    };
    let peak_rss_bytes = i128::from(peak_rss_kib) * 1024;
    let gap = peak_rss_bytes - i128::from(compacted_region_bytes);
    gap.to_string()
}

fn compacted_region_bytes(stats: &crate::report_schema::ImportStatsSample) -> u64 {
    if stats.compacted_region_bytes == 0 {
        stats.imported_bytes
    } else {
        stats.compacted_region_bytes
    }
}

#[cfg(test)]
mod tests {
    use crate::report_schema::{
        AdmissionSample, BaselineMode, EnvPair, ImportStatsSample, PerformanceReport, ReportMetadata,
        SessionReuseSample, TimingSample, WorkloadRun,
    };

    use super::render_markdown;

    #[test]
    fn import_stats_table_renders_region_byte_split_and_rss_gap() {
        let report = PerformanceReport {
            metadata: ReportMetadata {
                timestamp_utc: "now".to_owned(),
                platform: "test".to_owned(),
                git_commit: "abc".to_owned(),
                git_branch: "main".to_owned(),
                lean_version: "test".to_owned(),
                tooling: "test".to_owned(),
            },
            baseline_mode: BaselineMode::Quick,
            workloads: vec![WorkloadRun {
                name: "long-session-test".to_owned(),
                command: "cmd".to_owned(),
                env: Vec::<EnvPair>::new(),
                exit_success: true,
                exit_code: Some(0),
                wall_time_ms: 1.0,
                status: Some("ok".to_owned()),
                peak_rss_kib: Some(8),
                checkpoints: Vec::new(),
                import_stats: vec![ImportStatsSample {
                    label: "import".to_owned(),
                    iteration: Some(1),
                    profile_mode: "private".to_owned(),
                    direct_imports: vec!["Init".to_owned()],
                    effective_modules: 1,
                    compacted_regions: 2,
                    memory_mapped_regions: 1,
                    compacted_region_bytes: 4096,
                    memory_mapped_region_bytes: Some(1024),
                    non_memory_mapped_region_bytes: Some(3072),
                    imported_bytes: 4096,
                    imported_constants: 3,
                    extension_count: 4,
                    total_imported_extension_entries: 5,
                    import_level: "private".to_owned(),
                    import_all: false,
                    load_exts: true,
                    free_regions_ran: None,
                }],
                derived_work: Vec::new(),
                timings: Vec::new(),
                admissions: Vec::new(),
                session_reuse: Vec::new(),
                key_values: Vec::new(),
                stdout_path: "stdout".to_owned(),
                stderr_path: "stderr".to_owned(),
            }],
            profiles: Vec::new(),
            notes: Vec::new(),
        };

        let markdown = render_markdown(&report);
        assert!(markdown.contains("mmap bytes"));
        assert!(markdown.contains("non-mmap bytes"));
        assert!(markdown.contains("RSS gap"));
        assert!(markdown.contains("| import | 1 | private | Init | false | private | true | - | 1 | 2 | 1 | 4096 | 1024 | 3072 | 4096 | 3 | 4 | 5 |"));
    }

    #[test]
    fn report_renders_worker_policy_summary_and_timings() {
        let report = PerformanceReport {
            metadata: ReportMetadata {
                timestamp_utc: "now".to_owned(),
                platform: "test-platform".to_owned(),
                git_commit: "abc".to_owned(),
                git_branch: "main".to_owned(),
                lean_version: "test-lean".to_owned(),
                tooling: "test".to_owned(),
            },
            baseline_mode: BaselineMode::Quick,
            workloads: vec![WorkloadRun {
                name: "worker-cycling-max-imports-1".to_owned(),
                command: "worker".to_owned(),
                env: vec![EnvPair {
                    key: "LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB".to_owned(),
                    value: "1572864".to_owned(),
                }],
                exit_success: true,
                exit_code: Some(0),
                wall_time_ms: 1.0,
                status: Some("ok".to_owned()),
                peak_rss_kib: Some(1000),
                checkpoints: Vec::new(),
                import_stats: Vec::new(),
                derived_work: Vec::new(),
                timings: vec![TimingSample {
                    label: "worker_cycling".to_owned(),
                    iteration: Some(1),
                    kind: "cold".to_owned(),
                    elapsed_ms: 10.0,
                    rss_kib: Some(1000),
                    workers: None,
                    total_child_rss_kib: None,
                    worker_restarts: None,
                    max_import_restarts: None,
                    policy_restarts: None,
                }],
                admissions: vec![AdmissionSample {
                    label: "worker_session_open".to_owned(),
                    iteration: Some(1),
                    kind: "cold".to_owned(),
                    cold_open_attempts: 1,
                    cold_open_admitted: 1,
                    cold_open_refusals: 0,
                    import_like_requests: 1,
                    import_like_admitted: Some(1),
                    concurrent_cold_opens_observed: 0,
                    rss_before_admission_kib: Some(100),
                    rss_after_open_kib: Some(1000),
                    refusal_reason: None,
                }],
                session_reuse: Vec::new(),
                key_values: vec![
                    crate::report_schema::KeyValue {
                        key: "max_rss_kib".to_owned(),
                        value: "1572864".to_owned(),
                    },
                    crate::report_schema::KeyValue {
                        key: "restarts".to_owned(),
                        value: "5".to_owned(),
                    },
                ],
                stdout_path: "stdout".to_owned(),
                stderr_path: "stderr".to_owned(),
            }],
            profiles: Vec::new(),
            notes: Vec::new(),
        };

        let markdown = render_markdown(&report);
        assert!(markdown.contains("## Run Configuration"));
        assert!(markdown.contains("## Worker Policy Summary"));
        assert!(markdown.contains("LeanWorkerRestartPolicy::memory_bounded(1, 1572864)"));
        assert!(markdown.contains("## Worker Timings"));
        assert!(markdown.contains("| worker_cycling | 1 | cold | 10.000 | 1000 |"));
        assert!(markdown.contains("## Import Admission"));
        assert!(markdown.contains("| worker_session_open | 1 | cold | 1 | 1 | 0 | 1 | 1 | 0 | 100 | 1000 | - |"));
    }

    #[test]
    fn report_renders_session_reuse_table() {
        let report = PerformanceReport {
            metadata: ReportMetadata {
                timestamp_utc: "now".to_owned(),
                platform: "test".to_owned(),
                git_commit: "abc".to_owned(),
                git_branch: "main".to_owned(),
                lean_version: "test".to_owned(),
                tooling: "test".to_owned(),
            },
            baseline_mode: BaselineMode::Quick,
            workloads: vec![WorkloadRun {
                name: "pool-memory".to_owned(),
                command: "cmd".to_owned(),
                env: Vec::<EnvPair>::new(),
                exit_success: true,
                exit_code: Some(0),
                wall_time_ms: 1.0,
                status: Some("ok".to_owned()),
                peak_rss_kib: None,
                checkpoints: Vec::new(),
                import_stats: Vec::new(),
                derived_work: Vec::new(),
                timings: Vec::new(),
                admissions: Vec::new(),
                session_reuse: vec![SessionReuseSample {
                    label: "bounded_reuse_no_cycle".to_owned(),
                    iteration: Some(2),
                    layer: "worker-pool".to_owned(),
                    key_hits: 1,
                    key_misses: 1,
                    distinct_keys_seen: 1,
                    fresh_imports_avoided: 1,
                    miss_empty_pool: 1,
                    miss_reuse_disabled: 0,
                    miss_no_matching_key: 0,
                    last_miss_reason: None,
                }],
                key_values: Vec::new(),
                stdout_path: "stdout".to_owned(),
                stderr_path: "stderr".to_owned(),
            }],
            profiles: Vec::new(),
            notes: Vec::new(),
        };

        let markdown = render_markdown(&report);
        assert!(markdown.contains("## Session Reuse Keys"));
        assert!(markdown.contains("Fresh imports avoided"));
        assert!(markdown.contains("| bounded_reuse_no_cycle | 2 | worker-pool | 1 | 1 | 1 | 1 | 1 | 0 | 0 | - |"));
    }
}
