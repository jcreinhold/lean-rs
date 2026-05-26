//! Process a Lean source file with one bounded module query.
//!
//! Run with:
//!
//! ```text
//! cargo run -p lean-rs-host --example process_module_query -- <path> diagnostics
//! cargo run -p lean-rs-host --example process_module_query -- <path> type-at <line> <column>
//! cargo run -p lean-rs-host --example process_module_query -- <path> goal-at <line> <column>
//! cargo run -p lean-rs-host --example process_module_query -- <path> references <name>
//! ```

#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use lean_rs::{LeanError, LeanRuntime};
use lean_rs_host::host::process::{ModuleQuery, ModuleQueryOutcome, ModuleQueryResult};
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
        usage();
        return Ok(ExitCode::from(2));
    };
    let Some(query_arg) = args.next() else {
        usage();
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
    let query = match parse_query(&query_arg, args) {
        Ok(query) => query,
        Err(err) => {
            eprintln!("{err}");
            usage();
            return Ok(ExitCode::from(2));
        }
    };

    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;
    let mut session = caps.session(&["LeanRsHostShims.Elaboration"], None, None)?;
    let options = LeanElabOptions::new().file_label(&path.display().to_string());

    let outcome = session.process_module_query(&source, &query, &options, None)?;
    print_outcome(outcome);
    Ok(ExitCode::SUCCESS)
}

fn parse_query<I>(kind: &str, mut args: I) -> Result<ModuleQuery, String>
where
    I: Iterator<Item = String>,
{
    match kind {
        "diagnostics" => Ok(ModuleQuery::Diagnostics),
        "type-at" => {
            let line = parse_u32(args.next(), "line")?;
            let column = parse_u32(args.next(), "column")?;
            Ok(ModuleQuery::TypeAt { line, column })
        }
        "goal-at" => {
            let line = parse_u32(args.next(), "line")?;
            let column = parse_u32(args.next(), "column")?;
            Ok(ModuleQuery::GoalAt { line, column })
        }
        "references" => {
            let Some(name) = args.next() else {
                return Err("missing references name argument".to_string());
            };
            Ok(ModuleQuery::References { name })
        }
        _ => Err(format!("unknown query kind `{kind}`")),
    }
}

fn parse_u32(value: Option<String>, label: &str) -> Result<u32, String> {
    let Some(value) = value else {
        return Err(format!("missing {label} argument"));
    };
    value.parse().map_err(|_| format!("invalid {label} argument `{value}`"))
}

fn print_outcome(outcome: ModuleQueryOutcome) {
    match outcome {
        ModuleQueryOutcome::Ok { result, imports } => {
            println!("ok imports={imports:?}");
            print_result(result);
        }
        ModuleQueryOutcome::MissingImports {
            result,
            imports,
            missing,
        } => {
            println!("missing-imports imports={imports:?} missing={missing:?}");
            print_result(result);
        }
        ModuleQueryOutcome::HeaderParseFailed { diagnostics } => {
            println!(
                "header-parse-failed diagnostics={} truncated={}",
                diagnostics.diagnostics().len(),
                diagnostics.truncated(),
            );
        }
        ModuleQueryOutcome::Unsupported => println!("unsupported"),
    }
}

fn print_result(result: ModuleQueryResult) {
    match result {
        ModuleQueryResult::Diagnostics(diagnostics) => {
            println!(
                "diagnostics count={} truncated={}",
                diagnostics.diagnostics().len(),
                diagnostics.truncated(),
            );
        }
        ModuleQueryResult::TypeAt(result) => println!("type-at {result:?}"),
        ModuleQueryResult::GoalAt(result) => println!("goal-at {result:?}"),
        ModuleQueryResult::References(result) => {
            println!(
                "references count={} truncated={}",
                result.references.len(),
                result.truncated,
            );
        }
    }
}

fn usage() {
    eprintln!("usage: process_module_query <path> <diagnostics|type-at|goal-at|references> [args]");
}

fn lake_project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
