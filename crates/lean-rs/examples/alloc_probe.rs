//! dhat-instrumented allocation probe for ABI conversions.
//!
//! Mirrors the per-iteration body of the matching `hot_paths`
//! benchmarks for a fixed iteration count, with dhat installed as the
//! Rust global allocator. Writes `dhat-heap.json` on drop; inspect at
//! <https://nnethercote.github.io/dh_view/dh_view.html>.
//!
//! Lean's internal mimalloc (statically linked into `libleanrt.a`) is
//! a separate allocator—it does not flow through `#[global_allocator]`
//! and is invisible to this probe. The numbers captured here are
//! host-side Rust churn (`String::from_utf8`, `Vec` resize, ABI buffer
//! ownership transfers, error-message bounding)—the surface the
//! conversion-path interventions target.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p lean-rs --example alloc_probe -- string_identity_4096
//! cargo run --release -p lean-rs --example alloc_probe -- array_string_256
//! ```

#![allow(
    unsafe_code,
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::print_stderr
)]

#[path = "support/scalar_fixture.rs"]
mod scalar_fixture;
use scalar_fixture::ScalarFixture;

use lean_rs::LeanRuntime;
use lean_rs::module::LeanLibrary;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const ITERS: usize = 1000;

fn run_string_identity(library: &LeanLibrary<'_>, bytes: usize) {
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("module initialiser succeeds");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let identity = unsafe { module.exported_unchecked::<(String,), String>("lean_rs_fixture_string_identity") }
        .expect("lookup string_identity");
    let template = "a".repeat(bytes);
    for _ in 0..ITERS {
        // SAFETY: the matched identity consumes the freshly cloned Rust value and returns the same Lean type.
        let echoed = unsafe { identity.call(template.clone()) }.expect("call");
        std::hint::black_box(echoed);
    }
}

fn run_array_string(library: &LeanLibrary<'_>, n: usize) {
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("module initialiser succeeds");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let identity =
        unsafe { module.exported_unchecked::<(Vec<String>,), Vec<String>>("lean_rs_fixture_array_string_identity") }
            .expect("lookup array_string_identity");
    let template: Vec<String> = (0..n).map(|i| format!("elt{i:04}")).collect();
    for _ in 0..ITERS {
        // SAFETY: the matched identity consumes the freshly cloned Rust value and returns the same Lean type.
        let echoed = unsafe { identity.call(template.clone()) }.expect("call");
        std::hint::black_box(echoed);
    }
}

fn main() {
    // The dhat profiler is initialised *before* runtime/library setup
    // so the bring-up cost is included—this matches what a real
    // application sees on first request. Use the dhat viewer to
    // partition setup vs. steady-state by the recorded backtraces.
    let _profiler = dhat::Profiler::new_heap();

    let workload = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: alloc_probe <string_identity_16|string_identity_256|string_identity_4096|array_string_1|array_string_16|array_string_256>");
        std::process::exit(2);
    });

    let fixture = ScalarFixture::admit().expect("admit fixed profiling fixture");
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    let library = fixture.open(runtime).expect("fixture dylib opens");

    match workload.as_str() {
        "string_identity_16" => run_string_identity(library, 16),
        "string_identity_256" => run_string_identity(library, 256),
        "string_identity_4096" => run_string_identity(library, 4096),
        "array_string_1" => run_array_string(library, 1),
        "array_string_16" => run_array_string(library, 16),
        "array_string_256" => run_array_string(library, 256),
        other => {
            eprintln!("unknown workload: {other}");
            std::process::exit(2);
        }
    }

    eprintln!("workload={workload} iterations={ITERS} dhat-heap.json written on exit");
}
