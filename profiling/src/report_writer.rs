use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::results_dir;
use crate::report_schema::{BaselineMode, PerformanceReport};

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

fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(path)
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
        BaselineMode, EnvPair, ImportStatsSample, PerformanceReport, ReportMetadata, WorkloadRun,
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
}
