//! Probe for worker proof-candidate attempt latency and partial outcomes.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p lean-rs-worker-child --example proof_candidate_probe
//! ```
//!
//! The probe exercises the real worker/session path against the checked-in
//! Lean fixture. It prints line-oriented facts rather than Criterion summaries
//! because the interesting contract includes ordered statuses, JSON size, and
//! child RSS samples in addition to wall-clock latency.

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerConfig, LeanWorkerElabOptions, LeanWorkerOutputBudgets, LeanWorkerProofAttemptRequest,
    LeanWorkerProofAttemptResult, LeanWorkerProofAttemptStatus, LeanWorkerProofCandidate, LeanWorkerProofEditTarget,
    LeanWorkerProofPositionSelector, LeanWorkerSession, LeanWorkerSessionConfig,
};

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn worker_binary() -> PathBuf {
    let workspace = workspace_root();
    let release = workspace.join("target").join("release").join("lean-rs-worker-child");
    if release.is_file() {
        release
    } else {
        workspace.join("target").join("debug").join("lean-rs-worker-child")
    }
}

fn fixture_root() -> PathBuf {
    workspace_root().join("fixtures").join("lean")
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary()).request_timeout(Duration::from_secs(20))
}

fn session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsHostShims.Elaboration"],
    )
}

fn source() -> String {
    "import Lean\n\ntheorem t : True := by\n  skip\n/-- following docstring must remain parseable -/\ntheorem u : True := by\n  trivial\n".to_owned()
}

fn candidate_pool() -> Vec<LeanWorkerProofCandidate> {
    let expensive = "run_tac\n  let rec loop : Nat -> Lean.Meta.MetaM Unit\n    | 0 => pure ()\n    | n + 1 => loop n\n  loop 1000000";
    vec![
        LeanWorkerProofCandidate {
            id: "success".to_owned(),
            text: "trivial".to_owned(),
        },
        LeanWorkerProofCandidate {
            id: "failure".to_owned(),
            text: "exact definitely_missing_identifier".to_owned(),
        },
        LeanWorkerProofCandidate {
            id: "expensive".to_owned(),
            text: expensive.to_owned(),
        },
    ]
}

fn candidates(count: usize) -> Vec<LeanWorkerProofCandidate> {
    candidate_pool()
        .into_iter()
        .cycle()
        .take(count)
        .enumerate()
        .map(|(idx, mut candidate)| {
            candidate.id = format!("{}-{idx}", candidate.id);
            candidate
        })
        .collect()
}

fn request(count: usize) -> LeanWorkerProofAttemptRequest {
    request_with_budgets(count, LeanWorkerOutputBudgets::default())
}

fn request_with_budgets(count: usize, budgets: LeanWorkerOutputBudgets) -> LeanWorkerProofAttemptRequest {
    LeanWorkerProofAttemptRequest {
        source: source(),
        edit: LeanWorkerProofEditTarget::Declaration {
            name: "t".to_owned(),
            position: LeanWorkerProofPositionSelector::default(),
        },
        candidates: candidates(count),
        budgets,
    }
}

#[derive(Default)]
struct StatusCounts {
    closed: usize,
    progressed: usize,
    failed: usize,
    timeout: usize,
    budget: usize,
    not_attempted: usize,
    unsupported: usize,
    rows: usize,
}

fn status_counts(result: &LeanWorkerProofAttemptResult) -> StatusCounts {
    let mut counts = StatusCounts::default();
    let (LeanWorkerProofAttemptResult::Ok { result, .. } | LeanWorkerProofAttemptResult::MissingImports { result, .. }) =
        result
    else {
        return counts;
    };
    counts.rows = result.candidates.len();
    for row in &result.candidates {
        match row.status {
            LeanWorkerProofAttemptStatus::Closed => counts.closed = counts.closed.saturating_add(1),
            LeanWorkerProofAttemptStatus::Progressed => counts.progressed = counts.progressed.saturating_add(1),
            LeanWorkerProofAttemptStatus::Failed => counts.failed = counts.failed.saturating_add(1),
            LeanWorkerProofAttemptStatus::Timeout => counts.timeout = counts.timeout.saturating_add(1),
            LeanWorkerProofAttemptStatus::BudgetExceeded => counts.budget = counts.budget.saturating_add(1),
            LeanWorkerProofAttemptStatus::NotAttempted => {
                counts.not_attempted = counts.not_attempted.saturating_add(1);
            }
            LeanWorkerProofAttemptStatus::Unsupported => counts.unsupported = counts.unsupported.saturating_add(1),
            _ => {}
        }
    }
    counts
}

fn run_attempt(
    session: &mut LeanWorkerSession<'_>,
    count: usize,
) -> Result<(LeanWorkerProofAttemptResult, Duration, usize), Box<dyn std::error::Error>> {
    run_attempt_request(session, &request(count))
}

fn run_attempt_request(
    session: &mut LeanWorkerSession<'_>,
    request: &LeanWorkerProofAttemptRequest,
) -> Result<(LeanWorkerProofAttemptResult, Duration, usize), Box<dyn std::error::Error>> {
    let options = LeanWorkerElabOptions::new()
        .file_label("/probe/proof-candidates.lean")
        .heartbeat_limit(20_000)
        .diagnostic_byte_limit(16_384);
    let started = Instant::now();
    let result = session.attempt_proof(request, &options, None, None)?;
    let elapsed = started.elapsed();
    let bytes = serde_json::to_vec(&result)?.len();
    Ok((result, elapsed, bytes))
}

fn print_result(label: &str, count: usize, result: &LeanWorkerProofAttemptResult, elapsed: Duration, bytes: usize) {
    let counts = status_counts(result);
    println!(
        "mode={label} candidates={count} rows={} elapsed_ms={:.3} response_bytes={bytes} closed={} progressed={} failed={} timeout={} budget_exceeded={} not_attempted={} unsupported={}",
        counts.rows,
        elapsed.as_secs_f64() * 1000.0,
        counts.closed,
        counts.progressed,
        counts.failed,
        counts.timeout,
        counts.budget,
        counts.not_attempted,
        counts.unsupported,
    );
}

fn run_cold(count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut worker = LeanWorker::spawn(&worker_config())?;
    let rss_before = worker.rss_kib();
    {
        let mut session = worker.open_session(&session_config(), None, None)?;
        let (result, elapsed, bytes) = run_attempt(&mut session, count)?;
        print_result("cold", count, &result, elapsed, bytes);
    }
    println!(
        "mode=cold candidates={count} child_rss_before_kib={rss_before:?} child_rss_after_kib={:?}",
        worker.rss_kib()
    );
    worker.shutdown()?;
    Ok(())
}

fn run_warm(counts: &[usize]) -> Result<(), Box<dyn std::error::Error>> {
    let mut worker = LeanWorker::spawn(&worker_config())?;
    let rss_before = worker.rss_kib();
    {
        let mut session = worker.open_session(&session_config(), None, None)?;
        for &count in counts {
            let (result, elapsed, bytes) = run_attempt(&mut session, count)?;
            print_result("warm", count, &result, elapsed, bytes);
        }
        let budgeted = request_with_budgets(
            3,
            LeanWorkerOutputBudgets {
                per_field_bytes: 16,
                total_bytes: 1,
            },
        );
        let (result, elapsed, bytes) = run_attempt_request(&mut session, &budgeted)?;
        print_result("warm_budget", 3, &result, elapsed, bytes);
    }
    println!(
        "mode=warm child_rss_before_kib={rss_before:?} child_rss_after_kib={:?}",
        worker.rss_kib()
    );
    worker.shutdown()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::build_lake_target_quiet(&fixture_root(), "LeanRsFixture")?;
    for count in [1, 3, 10] {
        run_cold(count)?;
    }
    run_warm(&[1, 3, 10])?;
    Ok(())
}
