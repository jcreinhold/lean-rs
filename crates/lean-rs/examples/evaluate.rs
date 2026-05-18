//! Call a typed Lean export through the curated session surface and
//! print the result.
//!
//! `LeanSession::call_capability` is the transport-neutral escape hatch
//! for any `@[export]` Lean function that isn't one of the
//! session-fixed symbols (`query_declaration`, `elaborate`, …). The
//! call site spells the argument tuple type and the return decoder;
//! `lean-rs` resolves the dlsym address, dispatches, and returns the
//! decoded value.
//!
//! Run with: `cargo run -p lean-rs --example evaluate`.
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
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
        .try_init();
}

fn run() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, lake_project_root())?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;

    // call_capability is a session method; we still need a session to
    // dispatch through. The import list affects what's in the Lean
    // environment, not which dylib symbols are reachable, so a small
    // import keeps the cold path fast.
    let mut session = caps.session(&["LeanRsFixture.Strings"])?;

    let input = "hello, lean";

    // Pure round-trip on a boxed type:
    // `stringIdentity : String → String`. The Rust call spells the
    // argument tuple `(&str,)` and return type `String`; `lean-rs`
    // wires the typed FFI through borrowed-string marshalling (no
    // Rust-side copy on the way in).
    let echoed: String = session.call_capability::<(&str,), String>("lean_rs_fixture_string_identity", (input,))?;
    println!("string_identity({input:?}) = {echoed:?}");

    // Non-trivial computation on unboxed scalars:
    // `u32Add : UInt32 → UInt32 → UInt32`. Lake emits both
    // parameters and the return as unboxed C `uint32_t`, so the
    // `u32` `LeanAbi` impl carries the raw values across without
    // boxing.
    let sum: u32 = session.call_capability::<(u32, u32), u32>("lean_rs_fixture_u32_add", (1_000, 2_500))?;
    println!("u32_add(1000, 2500) = {sum}");

    Ok(())
}

fn lake_project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("fixtures/lean"),
        |workspace| workspace.join("fixtures").join("lean"),
    )
}
