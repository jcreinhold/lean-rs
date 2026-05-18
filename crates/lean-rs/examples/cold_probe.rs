//! One-shot cold-path probe for prompt 21's performance baseline.
//!
//! Runs **once per process** and prints per-stage wall-clock for the
//! three cold paths Criterion cannot honestly measure: runtime init,
//! library open, and module initializer. Repeated invocation (via a
//! shell loop or `hyperfine`) provides variance; see
//! `docs/performance/baseline.md` for the commands.
//!
//! Output format: one `name=<workload> elapsed_us=<u64>` line per
//! stage, suitable for `grep`/`awk` post-processing.
//!
//! Why not a Criterion bench: `LeanRuntime::init` is `OnceLock`-cached
//! (`crates/lean-rs/src/runtime/init.rs`) and module initialisers are
//! guarded by Lean-side `_G_initialized` flags. Repeated invocation
//! inside `b.iter` measures the cached fast-path, not the cost the
//! prompt names. Subprocess-per-sample would conflate Lean init with
//! `fork+execve+ld` cost.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used, clippy::print_stdout)]

use std::path::PathBuf;
use std::time::Instant;

use lean_rs::LeanRuntime;
use lean_rs::module::LeanLibrary;

fn fixture_dylib_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    workspace
        .join("fixtures")
        .join("lean")
        .join(".lake")
        .join("build")
        .join("lib")
        .join(format!("liblean__rs__fixture_LeanRsFixture.{dylib_extension}"))
}

fn report(name: &str, elapsed_us: u128) {
    println!("name={name} elapsed_us={elapsed_us}");
}

fn main() {
    let path = fixture_dylib_path();
    assert!(
        path.exists(),
        "fixture dylib not found at {} — run `cd fixtures/lean && lake build`",
        path.display(),
    );

    // Stage 1: runtime init. First-call cost: `lean_initialize`,
    // `lean_io_mark_end_initialization`, OnceLock seeding.
    let t = Instant::now();
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    report("runtime_init_cold", t.elapsed().as_micros());

    // Stage 2: library open. dlopen of the Lake-built fixture dylib
    // plus the symbol-table walk that classifies function vs.
    // nullary-constant globals.
    let t = Instant::now();
    let library = LeanLibrary::open(runtime, &path).expect("fixture dylib opens");
    report("library_open_cold", t.elapsed().as_micros());

    // Stage 3: module initializer. Invokes the C
    // `initialize_LeanRsFixture` constructor, which transitively
    // initialises every imported module.
    let t = Instant::now();
    let _module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("module initialiser succeeds");
    report("module_initialize_cold", t.elapsed().as_micros());
}
