//! Process an application-attested Lean module query and print its bounded result.

#![allow(clippy::print_stdout)]

use lean_rs::LeanResult;
use lean_rs::host_bridge::{HostModuleQuery, TrustedModuleSource};
use lean_rs_host::LeanSession;
use lean_rs_host::host::process::{ModuleQuery, ModuleQueryOutcome, ModuleQueryResult};

/// # Errors
/// Returns the session dispatch error if the admitted query fails.
pub fn run<'lean>(
    session: &mut LeanSession<'lean, '_>,
    request: &TrustedModuleSource<'lean, '_, HostModuleQuery>,
) -> LeanResult<()> {
    print_outcome(session.process_module_query(request, None)?);
    Ok(())
}

/// # Errors
/// Returns a diagnostic for an unknown selector or a missing or invalid argument.
pub fn parse_query<I>(kind: &str, mut args: I) -> Result<ModuleQuery, String>
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
