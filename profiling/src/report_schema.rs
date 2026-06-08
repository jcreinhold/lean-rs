use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineMode {
    Quick,
    Full,
}

impl BaselineMode {
    pub const fn artifact_stem(self) -> &'static str {
        match self {
            Self::Quick => "baseline_data_quick",
            Self::Full => "baseline_data_full",
        }
    }

    pub const fn report_stem(self) -> &'static str {
        match self {
            Self::Quick => "profiling-baseline-quick",
            Self::Full => "profiling-baseline-full",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceReport {
    pub metadata: ReportMetadata,
    pub baseline_mode: BaselineMode,
    pub workloads: Vec<WorkloadRun>,
    pub profiles: Vec<ProfileArtifact>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMetadata {
    pub timestamp_utc: String,
    pub platform: String,
    pub git_commit: String,
    pub git_branch: String,
    pub lean_version: String,
    pub tooling: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadRun {
    pub name: String,
    pub command: String,
    pub env: Vec<EnvPair>,
    pub exit_success: bool,
    pub exit_code: Option<i32>,
    pub wall_time_ms: f64,
    pub status: Option<String>,
    pub peak_rss_kib: Option<u64>,
    pub checkpoints: Vec<RssCheckpoint>,
    pub import_stats: Vec<ImportStatsSample>,
    pub derived_work: Vec<DerivedWorkSample>,
    pub key_values: Vec<KeyValue>,
    pub stdout_path: String,
    pub stderr_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvPair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RssCheckpoint {
    pub stage: String,
    pub rss_kib: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportStatsSample {
    pub label: String,
    pub iteration: Option<u64>,
    pub profile_mode: String,
    pub direct_imports: Vec<String>,
    pub effective_modules: u64,
    pub compacted_regions: u64,
    pub memory_mapped_regions: u64,
    pub imported_bytes: u64,
    pub imported_constants: u64,
    pub extension_count: u64,
    pub total_imported_extension_entries: u64,
    pub import_level: String,
    pub import_all: bool,
    pub load_exts: bool,
    pub free_regions_ran: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedWorkSample {
    pub label: String,
    pub iteration: Option<u64>,
    pub source_range_lookups: u64,
    pub docstring_lookups: u64,
    pub raw_type_renderings: u64,
    pub pretty_prints: u64,
    pub proof_search_fact_collections: u64,
    pub simp_extension_lookups: u64,
    pub parser_elaborator_runs: u64,
    pub module_snapshot_builds: u64,
    pub lazy_discr_tree_import_initialization_observed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileArtifact {
    pub workload: String,
    pub path: String,
    pub status: String,
}
