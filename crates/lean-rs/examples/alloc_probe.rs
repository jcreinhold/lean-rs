//! dhat-instrumented allocation probe for ABI conversions.
//!
//! Mirrors the per-iteration body of the matching `hot_paths`
//! benchmarks for a fixed iteration count, with dhat installed as the
//! Rust global allocator. Writes `dhat-heap.json` on drop; inspect at
//! <https://nnethercote.github.io/dh_view/dh_view.html>.
//!
//! Lean's internal mimalloc (statically linked into `libleanrt.a`) is
//! a separate allocator — it does not flow through `#[global_allocator]`
//! and is invisible to this probe. The numbers captured here are
//! host-side Rust churn (`String::from_utf8`, `Vec` resize, ABI buffer
//! ownership transfers, error-message bounding) — the surface the
//! conversion-path interventions target.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p lean-rs --features dhat-heap --example alloc_probe -- string_identity_4096
//! cargo run --release -p lean-rs --features dhat-heap --example alloc_probe -- array_string_256
//! ```

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used, clippy::print_stderr)]

use std::path::PathBuf;

use lean_rs::LeanRuntime;
use lean_rs::module::LeanLibrary;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const ITERS: usize = 1000;

fn fixture_dylib_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = workspace
        .join("fixtures")
        .join("lean")
        .join(".lake")
        .join("build")
        .join("lib");
    let new_style = lib_dir.join(format!("liblean__rs__fixture_LeanRsFixture.{dylib_extension}"));
    let old_style = lib_dir.join(format!("libLeanRsFixture.{dylib_extension}"));
    if old_style.is_file() && !new_style.is_file() {
        old_style
    } else {
        new_style
    }
}

fn run_string_identity(library: &LeanLibrary<'_>, bytes: usize) {
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("module initialiser succeeds");
    let identity = module
        .exported::<(String,), String>("lean_rs_fixture_string_identity")
        .expect("lookup string_identity");
    let template = "a".repeat(bytes);
    for _ in 0..ITERS {
        let echoed = identity.call(template.clone()).expect("call");
        std::hint::black_box(echoed);
    }
}

fn run_array_string(library: &LeanLibrary<'_>, n: usize) {
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("module initialiser succeeds");
    let identity = module
        .exported::<(Vec<String>,), Vec<String>>("lean_rs_fixture_array_string_identity")
        .expect("lookup array_string_identity");
    let template: Vec<String> = (0..n).map(|i| format!("elt{i:04}")).collect();
    for _ in 0..ITERS {
        let echoed = identity.call(template.clone()).expect("call");
        std::hint::black_box(echoed);
    }
}

fn main() {
    // The dhat profiler is initialised *before* runtime/library setup
    // so the bring-up cost is included — this matches what a real
    // application sees on first request. Use the dhat viewer to
    // partition setup vs. steady-state by the recorded backtraces.
    let _profiler = dhat::Profiler::new_heap();

    let workload = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: alloc_probe <string_identity_16|string_identity_256|string_identity_4096|array_string_1|array_string_16|array_string_256>");
        std::process::exit(2);
    });

    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    let path = fixture_dylib_path();
    let library = LeanLibrary::open(runtime, &path).expect("fixture dylib opens");

    match workload.as_str() {
        "string_identity_16" => run_string_identity(&library, 16),
        "string_identity_256" => run_string_identity(&library, 256),
        "string_identity_4096" => run_string_identity(&library, 4096),
        "array_string_1" => run_array_string(&library, 1),
        "array_string_16" => run_array_string(&library, 16),
        "array_string_256" => run_array_string(&library, 256),
        other => {
            eprintln!("unknown workload: {other}");
            std::process::exit(2);
        }
    }

    eprintln!("workload={workload} iterations={ITERS} dhat-heap.json written on exit");
}
