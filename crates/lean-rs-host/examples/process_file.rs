//! Project a tiny Lean source string into its info-tree projection
//! and print the four node counts.
//!
//! `LeanSession::process_with_info_tree` returns a `ProcessFileOutcome`
//! with two arms: `Processed` (carrying a `ProcessedFile`) and
//! `Unsupported` (when the loaded capability dylib does not export the
//! optional shim). Inspect `ProcessedFile::diagnostics` to detect
//! elaboration failures — there is no separate timeout arm because
//! `IO.processCommands` catches per-command exceptions and attaches
//! them to the message log.
//!
//! Run with: `cargo run -p lean-rs-host --example process_file`.

#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::host::process::ProcessFileOutcome;
use lean_rs_host::{LeanElabOptions, LeanHost};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("[{}] {err}", err.code());
            ExitCode::FAILURE
        }
    }
}

fn run() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;

    let mut session = caps.session(&["LeanRsHostShims.Elaboration"], None, None)?;

    let source = "def x := 1\ntheorem t : x = 1 := by rfl\n#check x";
    let options = LeanElabOptions::new();

    println!("process_with_info_tree source ({} bytes)", source.len());
    let outcome = session.process_with_info_tree(source, &options, None)?;

    match outcome {
        ProcessFileOutcome::Processed(processed) => {
            println!(
                "  commands: {}  terms: {}  tactics: {}  names: {}  diagnostics: {}  truncated: {}",
                processed.commands.len(),
                processed.terms.len(),
                processed.tactics.len(),
                processed.names.len(),
                processed.diagnostics.diagnostics().len(),
                processed.diagnostics.truncated(),
            );
            Ok(())
        }
        ProcessFileOutcome::Unsupported => {
            eprintln!("capability dylib does not export `lean_rs_host_process_with_info_tree`");
            std::process::exit(2);
        }
        other => {
            eprintln!("unexpected non-exhaustive outcome: {other:?}");
            std::process::exit(3);
        }
    }
}

fn lake_project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
