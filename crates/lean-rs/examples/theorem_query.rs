//! Query the imported environment for declarations and contrast a
//! definition (`Nat.add`) against a theorem (`Nat.add_zero`).
//!
//! Run with: `cargo run -p lean-rs --example theorem_query`.
//! See `crates/lean-rs/examples/README.md` for expected output.

#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use lean_rs::{LeanHost, LeanResult, LeanRuntime};

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
        // Print span entry as well as events; lean-rs's instrumentation
        // is span-shaped, so without `FmtSpan::NEW` a debug/trace
        // `RUST_LOG` would show nothing.
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
        .try_init();
}

fn run() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;

    // `LeanRsFixture.Handles` is the smallest fixture module that
    // demonstrates a Lake-built import; the Lean prelude (where
    // `Nat.add` and `Nat.add_zero` live) is imported transitively.
    let mut session = caps.session(&["LeanRsFixture.Handles"])?;

    // The environment carries the full Lean prelude plus the
    // fixture's own declarations. Many thousands of entries even for
    // a small project — print only the count, then ask about names
    // the caller already knows.
    let names = session.list_declarations()?;
    println!("total_declarations={}", names.len());

    // Contrast a definition and a theorem by name.
    // `declaration_kind` returns the Lean-rendered tag —
    // `"definition"`, `"theorem"`, `"axiom"`, etc. —
    // `declaration_name` round-trips a name through Lean's pretty
    // printer (diagnostic only, not a semantic key).
    for name in ["Nat.add", "Nat.add_zero"] {
        let kind = session.declaration_kind(name)?;
        let rendered = session.declaration_name(name)?;
        println!("{name}: kind={kind} rendered={rendered}");
    }

    Ok(())
}

fn lake_project_root() -> PathBuf {
    // Resolve `fixtures/lean` relative to this crate's manifest so the
    // example runs from the workspace root via `cargo run --example`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
