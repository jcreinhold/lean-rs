//! Submit a small theorem to Lean's elaborator + kernel, re-validate
//! the resulting evidence, and project a bounded summary.
//!
//! `LeanSession::kernel_check` is the host-stack entry point that
//! takes a source string, parses + elaborates + kernel-checks it, and
//! returns a typed `LeanKernelOutcome`. On `Checked`, the carried
//! `LeanEvidence` handle lets us re-validate (`check_evidence`) or
//! project a display-only `ProofSummary` (`summarize_evidence`).
//!
//! Run with: `cargo run -p lean-rs --example proof_check`.
//! See `crates/lean-rs/examples/README.md` for expected output.

#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::{EvidenceStatus, LeanElabOptions, LeanHost, LeanKernelOutcome};

fn main() -> ExitCode {
    install_tracing();
    match run() {
        Ok(()) => {
            println!("ok");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("[{}] {err}", err.code());
            ExitCode::FAILURE
        }
    }
}

fn install_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
        .try_init();
}

fn run() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;

    // `LeanRsHostShims.Elaboration` brings in the Lean shim that
    // `kernel_check` dispatches through; the Lean prelude is
    // imported transitively, so `1 + 1 = 2 := rfl` elaborates.
    let mut session = caps.session(&["LeanRsHostShims.Elaboration"])?;

    let source = "theorem demo_proof_check : 1 + 1 = 2 := rfl";
    let options = LeanElabOptions::new();

    println!("kernel_check source: {source}");
    let outcome = session.kernel_check(source, &options)?;

    // The outcome is a four-tag enum. Only `Checked` carries a
    // `LeanEvidence` handle for re-validation; the other three carry
    // structured diagnostics. `#[non_exhaustive]` allows for future
    // toolchain refinements — match the closed case and group the
    // rest.
    let evidence = match outcome {
        LeanKernelOutcome::Checked(evidence) => evidence,
        LeanKernelOutcome::Rejected(failure) => {
            eprintln!("kernel rejected the proof: {failure}");
            return Ok(());
        }
        LeanKernelOutcome::Unavailable(failure) => {
            eprintln!("kernel could not check (resource-bound): {failure}");
            return Ok(());
        }
        LeanKernelOutcome::Unsupported(failure) => {
            eprintln!("source not supported by `kernel_check`: {failure}");
            return Ok(());
        }
        other => {
            eprintln!("unexpected non-exhaustive outcome: {other:?}");
            return Ok(());
        }
    };

    // Re-validate the captured evidence against the (unchanged)
    // session environment. `check_evidence` runs the kernel fresh —
    // useful when you cached evidence and want to confirm it still
    // holds.
    let status = session.check_evidence(&evidence)?;
    assert_eq!(status, EvidenceStatus::Checked, "evidence must re-validate");
    println!("check_evidence: {status:?}");

    // Project the evidence into a bounded `ProofSummary` for
    // logging. The strings are display text, not semantic keys.
    let summary = session.summarize_evidence(&evidence)?;
    println!(
        "summary: name={} kind={} type={}",
        summary.declaration_name(),
        summary.kind(),
        summary.type_signature(),
    );

    Ok(())
}

fn lake_project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
