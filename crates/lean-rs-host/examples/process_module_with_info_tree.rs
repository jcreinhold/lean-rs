//! Process a Lean source file (header + body) and print a summary.
//!
//! Unlike `process_file` (which feeds a body-only snippet through
//! `process_with_info_tree`), this example uses
//! `LeanSession::process_module_with_info_tree` — it parses the header
//! first and resumes elaboration of the body from the parser state,
//! so position coordinates land in the original file's line/column
//! system.
//!
//! Run with: `cargo run -p lean-rs-host --example process_module_with_info_tree -- <path>`.

#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use lean_rs::{LeanError, LeanRuntime};
use lean_rs_host::host::process::ProcessModuleOutcome;
use lean_rs_host::{LeanElabOptions, LeanHost};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("[{}] {err}", err.code());
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, LeanError> {
    let mut args = std::env::args().skip(1);
    let Some(path_arg) = args.next() else {
        eprintln!("usage: process_module_with_info_tree <path-to-lean-file>");
        return Ok(ExitCode::from(2));
    };
    let path = PathBuf::from(path_arg);
    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("failed to read {}: {err}", path.display());
            return Ok(ExitCode::from(2));
        }
    };

    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;

    let mut session = caps.session(&["LeanRsHostShims.Elaboration"], None, None)?;

    let options = LeanElabOptions::new().file_label(&path.display().to_string());

    println!(
        "process_module_with_info_tree {} ({} bytes)",
        path.display(),
        source.len()
    );
    let outcome = session.process_module_with_info_tree(&source, &options, None)?;

    match outcome {
        ProcessModuleOutcome::Ok { file, imports } => {
            print_counts(&file, &imports, &[]);
            Ok(ExitCode::SUCCESS)
        }
        ProcessModuleOutcome::MissingImports { file, imports, missing } => {
            print_counts(&file, &imports, &missing);
            // Soft failure — still exit 0 because the body did
            // elaborate against whatever the env carried.
            Ok(ExitCode::SUCCESS)
        }
        ProcessModuleOutcome::HeaderParseFailed { diagnostics } => {
            eprintln!(
                "  header parse failed: {} diagnostic(s){}",
                diagnostics.diagnostics().len(),
                if diagnostics.truncated() { " (truncated)" } else { "" },
            );
            for d in diagnostics.diagnostics() {
                eprintln!("    [{:?}] {}", d.severity(), d.message());
            }
            Ok(ExitCode::from(3))
        }
        ProcessModuleOutcome::Unsupported => {
            eprintln!("capability dylib does not export `lean_rs_host_process_module_with_info_tree`");
            Ok(ExitCode::from(2))
        }
        other => {
            eprintln!("unexpected non-exhaustive outcome: {other:?}");
            Ok(ExitCode::from(3))
        }
    }
}

fn print_counts(file: &lean_rs_host::host::process::ProcessedFile, imports: &[String], missing: &[String]) {
    println!(
        "  commands: {}  terms: {}  tactics: {}  names: {}  diagnostics: {}  truncated: {}",
        file.commands.len(),
        file.terms.len(),
        file.tactics.len(),
        file.names.len(),
        file.diagnostics.diagnostics().len(),
        file.diagnostics.truncated(),
    );
    println!("  imports: {imports:?}");
    if !missing.is_empty() {
        println!("  missing: {missing:?}");
    }
    if let Some(first) = file.tactics.first() {
        println!(
            "  first tactic at {}:{}-{}:{}",
            first.start_line, first.start_column, first.end_line, first.end_column,
        );
    }
}

fn lake_project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
