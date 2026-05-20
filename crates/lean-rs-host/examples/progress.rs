//! Observe structured progress and cooperative cancellation on a host-session call.
//!
//! Run with: `cargo run -p lean-rs-host --example progress`.
//! See `crates/lean-rs-host/examples/README.md` for expected output.

#![allow(clippy::expect_used, clippy::panic, clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use lean_rs::{LeanDiagnosticCode, LeanResult, LeanRuntime};
use lean_rs_host::{LeanCancellationToken, LeanHost, LeanProgressEvent, LeanProgressSink};

struct PrintProgress;

impl LeanProgressSink for PrintProgress {
    fn report(&self, event: LeanProgressEvent) {
        let total = event.total.map_or_else(|| "-".to_owned(), |n| n.to_string());
        println!(
            "progress phase={} current={} total={} elapsed_us={}",
            event.phase,
            event.current,
            total,
            event.elapsed.as_micros()
        );
    }
}

struct CancelOnFirst<'a> {
    token: &'a LeanCancellationToken,
}

impl LeanProgressSink for CancelOnFirst<'_> {
    fn report(&self, event: LeanProgressEvent) {
        println!(
            "cancel_progress phase={} current={} total={}",
            event.phase,
            event.current,
            event.total.map_or_else(|| "-".to_owned(), |n| n.to_string())
        );
        if event.current >= 1 {
            self.token.cancel();
        }
    }
}

fn main() -> ExitCode {
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

fn run() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;
    let mut session = caps.session(&["LeanRsFixture.Handles"], None, None)?;

    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let progress = PrintProgress;
    let declarations = session.query_declarations_bulk(&names, None, Some(&progress))?;
    println!("queried_declarations={}", declarations.len());

    let token = LeanCancellationToken::new();
    let cancel_sink = CancelOnFirst { token: &token };
    match session.declaration_kind_bulk(&names, Some(&token), Some(&cancel_sink)) {
        Err(err) if err.code() == LeanDiagnosticCode::Cancelled => {
            println!("cancelled_code={}", err.code());
        }
        Err(err) => return Err(err),
        Ok(kinds) => panic!("expected cancellation, got {kinds:?}"),
    }

    Ok(())
}

fn lake_project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
