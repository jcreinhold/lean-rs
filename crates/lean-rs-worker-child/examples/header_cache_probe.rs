//! Bounded header-cache probe over an existing project; does not build project modules.
#![allow(clippy::print_stdout)]
use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerConfig, LeanWorkerElabOptions, LeanWorkerModuleQuerySelector, LeanWorkerOutputBudgets,
    LeanWorkerRestartPolicy, LeanWorkerSessionConfig,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let [_, worker_path, project_path, source_path] = args.as_slice() else {
        return Err("usage: header_cache_probe WORKER PROJECT SOURCE".into());
    };
    let source = std::fs::read_to_string(source_path)?;
    let imports: Vec<String> = source
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let mut token = words.next()?;
            if token == "public" {
                token = words.next()?;
            }
            if token != "import" {
                return None;
            }
            let mut name = words.next()?;
            if name == "all" {
                name = words.next()?;
            }
            Some(name.to_owned())
        })
        .collect();
    let config = LeanWorkerConfig::new(PathBuf::from(worker_path))
        .request_timeout(Duration::from_secs(90))
        .env("LEAN_RS_NUM_THREADS", "1")
        .env("LEAN_RS_LEAN_MAX_MEMORY_KIB", "14680064")
        .restart_policy(LeanWorkerRestartPolicy::default().max_import_residue_bytes(4 * 1024 * 1024 * 1024));
    let sc = LeanWorkerSessionConfig::shims_only(project_path, imports);
    let mut worker = LeanWorker::spawn(&config)?;
    let opts = LeanWorkerElabOptions {
        file_label: source_path.clone(),
        ..Default::default()
    };
    for i in 0..3 {
        let start = Instant::now();
        {
            let mut session = worker.open_session(&sc, None, None)?;
            session.clear_module_snapshot_cache(None, None)?;
            let result = session.process_module_query_batch(
                &source,
                &[LeanWorkerModuleQuerySelector::Diagnostics {
                    id: "diagnostics".into(),
                }],
                &LeanWorkerOutputBudgets::default(),
                &opts,
                None,
                None,
            )?;
            let summary = format!("{result:?}");
            println!("call={i} result={}", summary.chars().take(900).collect::<String>());
        }
        println!(
            "call={i} elapsed_ms={} rss_kib={:?} lifecycle={:?}",
            start.elapsed().as_millis(),
            worker.rss_kib(),
            worker.lifecycle_snapshot()
        );
    }
    worker.shutdown()?;
    Ok(())
}
