//! What does one *real* import cost, and which reported quantity predicts it?
//!
//! Every published figure about import cost in this workspace was measured on
//! `fixtures/lean`, whose modules all import `Lean` and therefore share ~99% of
//! their closure. That makes the fixture useless for the question a byte-denominated
//! recycle policy has to answer: how much unreclaimable residue does one import add,
//! and does any field of `ImportStats` predict it?
//!
//! This probe imports a list of real import profiles, one per line, against a real
//! Lake project, and prints every `ImportStats` field alongside the process memory
//! delta each import produced. It samples two memory figures deliberately:
//!
//! - **RSS**, which counts clean file-backed pages. Two `importModules` calls get
//!   distinct `CompactedRegion` mappings even for modules they share, and two
//!   mappings of one file share physical page-cache pages but count twice in RSS.
//!   So RSS may be measuring *mappings* rather than memory.
//! - **`phys_footprint`** (macOS `RUSAGE_INFO_V2`), which excludes clean file-backed
//!   pages and is what the OS actually kills on. On Linux, `/proc/self/smaps_rollup`
//!   `Private_Dirty` plays the same role.
//!
//! A large ratio between the two says the RSS-denominated figures elsewhere in this
//! repo are inflated.
//!
//! ```sh
//! LEAN_RS_IMPORT_PROBE_ROOT=/path/to/lake/project \
//! LEAN_RS_IMPORT_PROBE_PROFILES=/path/to/profiles.txt \
//!     cargo run --release -p lean-rs-host --example import_residue_probe
//! ```
//!
//! `profiles.txt` holds one profile per line; imports within a line are separated by
//! whitespace or commas. Blank lines and `#` comments are skipped. Each line is
//! imported once, in order, into one process — which is the point: the question is
//! what the *n*-th import adds, not what the first one costs.

#![allow(clippy::expect_used, clippy::panic, clippy::print_stdout)]

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use lean_rs::LeanRuntime;
use lean_rs_host::{LeanHost, LeanImportStats};

fn main() -> ExitCode {
    let Some(root) = std::env::var_os("LEAN_RS_IMPORT_PROBE_ROOT").map(PathBuf::from) else {
        eprintln!("set LEAN_RS_IMPORT_PROBE_ROOT to a Lake project root");
        return ExitCode::FAILURE;
    };
    let Some(profiles_path) = std::env::var_os("LEAN_RS_IMPORT_PROBE_PROFILES").map(PathBuf::from) else {
        eprintln!("set LEAN_RS_IMPORT_PROBE_PROFILES to a file holding one import profile per line");
        return ExitCode::FAILURE;
    };
    let profiles = match std::fs::read_to_string(&profiles_path) {
        Ok(text) => parse_profiles(&text),
        Err(err) => {
            eprintln!("cannot read {}: {err}", profiles_path.display());
            return ExitCode::FAILURE;
        }
    };
    if profiles.is_empty() {
        eprintln!("{} contained no profiles", profiles_path.display());
        return ExitCode::FAILURE;
    }

    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let host = LeanHost::from_lake_project(runtime, &root).expect("host opens the project");
    // Shims-only: the probe must not depend on the project exposing a `:shared`
    // facet, and a capability dylib would add its own initializers to the picture.
    let caps = host.load_shims_only().expect("shim-only capabilities load");

    println!(
        "probe_start root={} profiles={} rss_kib={} footprint_kib={}",
        root.display(),
        profiles.len(),
        rss_kib(),
        phys_footprint_kib().map_or_else(|| "unavailable".to_owned(), |kib| kib.to_string()),
    );

    // Every session is held for the whole run by default. Dropping them should not
    // reclaim anything — `freeRegions` is unsound under `loadExts := true` — but
    // "should not" is the claim a pool's capacity is sized on, so
    // `LEAN_RS_IMPORT_PROBE_DROP=1` measures the same workload holding nothing.
    // The difference between the two final footprints is the marginal cost of one
    // held environment, which is what pool capacity trades against a re-import.
    let drop_each = std::env::var_os("LEAN_RS_IMPORT_PROBE_DROP").is_some();
    let mut held = Vec::with_capacity(profiles.len());
    let mut previous_rss = rss_kib();
    let mut previous_footprint = phys_footprint_kib();

    for (index, profile) in profiles.iter().enumerate() {
        let borrowed: Vec<&str> = profile.iter().map(String::as_str).collect();
        let session = match caps.session(&borrowed, None, None) {
            Ok(session) => session,
            Err(err) => {
                println!("import index={index} status=failed error={err}");
                continue;
            }
        };
        let rss = rss_kib();
        let footprint = phys_footprint_kib();
        report(
            index,
            session.import_stats(),
            rss,
            previous_rss,
            footprint,
            previous_footprint,
        );
        previous_rss = rss;
        previous_footprint = footprint;
        if drop_each {
            drop(session);
        } else {
            held.push(session);
        }
    }

    println!(
        "probe_end drop_each={drop_each} held_sessions={} rss_kib={} footprint_kib={}",
        held.len(),
        rss_kib(),
        phys_footprint_kib().map_or_else(|| "unavailable".to_owned(), |kib| kib.to_string()),
    );
    ExitCode::SUCCESS
}

fn report(
    index: usize,
    stats: &LeanImportStats,
    rss: u64,
    previous_rss: u64,
    footprint: Option<u64>,
    previous_footprint: Option<u64>,
) {
    let delta_footprint = match (footprint, previous_footprint) {
        (Some(now), Some(before)) => now.saturating_sub(before).to_string(),
        _ => "unavailable".to_owned(),
    };
    println!(
        "import index={index} modules={} constants={} \
         compacted_region_bytes={} memory_mapped_region_bytes={} non_memory_mapped_region_bytes={} \
         compacted_region_count={} memory_mapped_region_count={} extension_entries={} \
         delta_rss_kib={} delta_footprint_kib={} rss_kib={} footprint_kib={} imports={}",
        stats.effective_module_count,
        stats.imported_constant_count,
        stats.compacted_region_bytes,
        stats.memory_mapped_region_bytes,
        stats.non_memory_mapped_region_bytes,
        stats.compacted_region_count,
        stats.memory_mapped_region_count,
        stats.total_imported_extension_entries,
        rss.saturating_sub(previous_rss),
        delta_footprint,
        rss,
        footprint.map_or_else(|| "unavailable".to_owned(), |kib| kib.to_string()),
        stats.direct_import_names.join(","),
    );
}

fn parse_profiles(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split([',', ' ', '\t'])
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .filter(|profile: &Vec<String>| !profile.is_empty())
        .collect()
}

fn rss_kib() -> u64 {
    Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Dirty, anonymous, and compressed bytes charged to this process — what jetsam
/// and the OOM killer actually count. Clean file-backed `.olean` mappings, which
/// dominate RSS here, are excluded.
#[cfg(target_os = "linux")]
fn phys_footprint_kib() -> Option<u64> {
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    rollup.lines().find_map(|line| {
        let rest = line.strip_prefix("Private_Dirty:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

/// macOS has no `smaps_rollup`; `footprint(1)` reports exactly what
/// `proc_pid_rusage(RUSAGE_INFO_V2).ri_phys_footprint` exposes, in its
/// "Auxiliary data" block, without needing an `unsafe` libc binding in an example.
#[cfg(target_os = "macos")]
fn phys_footprint_kib() -> Option<u64> {
    let output = Command::new("footprint")
        .args(["-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("phys_footprint:"))?;
    let mut tokens = line.split_whitespace();
    let value: f64 = tokens.next()?.replace(',', "").parse().ok()?;
    let scale = match tokens.next() {
        Some("KB") => 1.0,
        Some("MB") => 1024.0,
        Some("GB") => 1024.0 * 1024.0,
        Some("B") | None => 1.0 / 1024.0,
        Some(_) => return None,
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a KiB count derived from a human-readable size; truncation is the intent"
    )]
    Some((value * scale) as u64)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn phys_footprint_kib() -> Option<u64> {
    None
}
