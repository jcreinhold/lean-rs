//! Run a bounded `MetaM` service against an elaborated Lean term and
//! handle every status the service can return.
//!
//! `LeanSession::run_meta` dispatches to a pre-resolved capability
//! shim (`infer_type`, `whnf`, `heartbeat_burn`) under
//! caller-supplied heartbeat / diagnostic budgets and returns a
//! `LeanMetaResponse<Resp>` whose four variants cover every outcome:
//! a typed payload on success, structured diagnostics on every kind
//! of failure.
//!
//! Run with: `cargo run -p lean-rs --example meta_query`.
//! See `crates/lean-rs/examples/README.md` for expected output.

#![allow(clippy::print_stdout, clippy::expect_used)]

use std::path::PathBuf;
use std::process::ExitCode;

use lean_rs::{LeanResult, LeanRuntime};
use lean_rs_host::meta::{LeanMetaOptions, LeanMetaResponse, infer_type};
use lean_rs_host::{LeanElabOptions, LeanHost};

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

#[allow(
    clippy::unwrap_in_result,
    reason = "elaboration of `(Nat.succ 0 : Nat)` against the prelude is a fixture invariant; an inner Err here means the toolchain itself has diverged"
)]
fn run() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;

    // `Meta` carries the `MetaM` shims `run_meta` dispatches to;
    // `Elaboration` carries the elaborator we use to build a
    // `LeanExpr` to feed `infer_type`.
    let mut session = caps.session(&["LeanRsHostShims.Meta", "LeanRsHostShims.Elaboration"], None, None)?;

    // Build an expression to query. `Nat.succ 0` elaborates against
    // the prelude without needing any extra imports. A failure here
    // would mean the active Lean prelude has diverged from what the
    // example was written against; `.expect` makes that diagnostic
    // surface directly.
    let elab_opts = LeanElabOptions::new();
    let expr = session
        .elaborate("(Nat.succ 0 : Nat)", None, &elab_opts, None)?
        .expect("(Nat.succ 0 : Nat) elaborates against the prelude");

    // Dispatch `infer_type` with default options (the published
    // heartbeat ceiling, the published diagnostic byte limit). The
    // typed return is `LeanMetaResponse<LeanExpr<'lean>>`.
    let meta_opts = LeanMetaOptions::new();
    let response = session.run_meta(&infer_type(), expr, &meta_opts, None)?;

    match response {
        LeanMetaResponse::Ok(_inferred_expr) => {
            // `LeanExpr` is opaque on purpose — the host stack never
            // pretty-prints proof terms. The `Ok` variant proves the
            // service ran cleanly; downstream callers usually feed
            // the inferred handle into further `MetaM` work or back
            // into `summarize_evidence` via a kernel-check round.
            println!("status=Ok service=infer_type");
        }
        LeanMetaResponse::Failed(failure) => {
            println!("status=Failed: {failure}");
        }
        LeanMetaResponse::TimeoutOrHeartbeat(failure) => {
            println!("status=TimeoutOrHeartbeat: {failure}");
        }
        LeanMetaResponse::Unsupported(failure) => {
            // Reached when the loaded capability lacks the requested
            // `MetaM` shim, or when the Lean side returned
            // `unsupported` for the request shape.
            println!("status=Unsupported: {failure}");
        }
        other => {
            println!("status=<non-exhaustive>: {other:?}");
        }
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
