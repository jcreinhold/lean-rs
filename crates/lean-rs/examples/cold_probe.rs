//! One-shot cold-path performance probe.
//!
//! Runs **once per process** and prints per-stage wall-clock for the
//! three cold paths Criterion cannot honestly measure: runtime init,
//! library open, and module initializer. Run it repeatedly, in a shell
//! loop or with `hyperfine`, to see the variance.
//!
//! Output format: one `name=<workload> elapsed_us=<u64>` line per
//! stage, suitable for `grep`/`awk` post-processing.
//!
//! Why not a Criterion bench: `LeanRuntime::init` is `OnceLock`-cached
//! (`crates/lean-rs/src/runtime/init.rs`) and module initialisers are
//! guarded by Lean-side `_G_initialized` flags. Repeated invocation
//! inside `b.iter` measures the cached fast-path, not the cost the
//! subprocess names. Subprocess-per-sample would conflate Lean init with
//! `fork+execve+ld` cost.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used, clippy::print_stdout)]

#[path = "support/scalar_fixture.rs"]
mod scalar_fixture;
use scalar_fixture::ScalarFixture;
use std::time::Instant;

use lean_rs::LeanRuntime;

fn report(name: &str, elapsed_us: u128) {
    println!("name={name} elapsed_us={elapsed_us}");
}

fn main() {
    // The library borrows the fixture, so both live until the process exits.
    let fixture = Box::leak(Box::new(ScalarFixture::admit().expect("admit fixed profiling fixture")));

    // Stage 1: runtime init. First-call cost: `lean_initialize`,
    // `lean_io_mark_end_initialization`, OnceLock seeding.
    let t = Instant::now();
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    report("runtime_init_cold", t.elapsed().as_micros());

    // Stage 2: library open. dlopen of the Lake-built fixture dylib
    // plus the symbol-table walk that classifies function vs.
    // nullary-constant globals.
    let t = Instant::now();
    let library = Box::leak(Box::new(fixture.open_owned(runtime).expect("fixture dylib opens")));
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
