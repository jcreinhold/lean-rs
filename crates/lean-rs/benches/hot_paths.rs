//! Steady-state benchmarks for the `lean_rs::module` + `lean_rs::abi`
//! hot paths.
//!
//! Each workload names the user-visible code path it measures:
//! benchmarks are workloads, not demonstrations. Cold paths (runtime
//! init, library open, module initializer) live in
//! `examples/cold_probe.rs` because they are once-per-process and
//! Criterion's repeated-sampling shape would measure the cached
//! fast-path instead.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, clippy::unwrap_used)]

use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use lean_rs::LeanRuntime;
use lean_rs::module::{LeanExported, LeanLibrary, LeanModule};

// -- fixture bring-up ---------------------------------------------------
//
// One library + module per bench-binary process: `LeanRuntime::init` is
// `OnceLock`-cached so the cost is paid once at first bench, then
// amortised across every workload group below. `LeanLibrary` and
// `LeanModule` live in this `static`-equivalent scope via lazy locals
// owned by `criterion_benchmarks`.

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

fn open_fixture() -> LeanLibrary<'static> {
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    let path = fixture_dylib_path();
    assert!(
        path.exists(),
        "fixture dylib not found at {} — run `cd fixtures/lean && lake build`",
        path.display(),
    );
    LeanLibrary::open(runtime, &path).expect("fixture dylib opens")
}

// -- scalar dispatch -----------------------------------------------------
//
// Workload: `module::scalar_dispatch_u32_add` — measures the cost of a
// fully-typed `LeanExported<(u32, u32), u32>::call` against
// `lean_rs_fixture_u32_add` (fixtures/lean/LeanRsFixture/Scalars.lean:25).
// This is the floor for any unboxed-scalar exported call: no Lean-side
// allocation, no Rust-side allocation, no IO wrapping. What's left is
// the cost of (a) the `IntoLean::into_c` no-op cast, (b) the indirect C
// call through libloading's resolved function pointer, (c) Lean's
// unboxed `UInt32 → UInt32 → UInt32` body, (d) `DecodeCallResult` no-op
// decode.

fn bench_scalar_dispatch(c: &mut Criterion, module: &LeanModule<'_, '_>) {
    let add: LeanExported<'_, '_, (u32, u32), u32> = module
        .exported::<(u32, u32), u32>("lean_rs_fixture_u32_add")
        .expect("lookup u32_add");

    c.bench_function("module::scalar_dispatch_u32_add", |b| {
        b.iter(|| {
            let result = add.call(black_box(7_u32), black_box(35_u32)).expect("call");
            black_box(result);
        });
    });
}

// -- string conversion ---------------------------------------------------
//
// Workload: `abi::string_roundtrip/<bytes>` — `LeanExported<(String,),
// String>::call` against `lean_rs_fixture_string_identity`
// (fixtures/lean/LeanRsFixture/Strings.lean:7). End-to-end cost of
// `String::into_lean` (one `lean_alloc_small` + memcpy) + the indirect
// C call + `String::try_from_lean` (one Rust `String::from_utf8` over
// the borrowed payload, which itself owns a fresh `Vec<u8>`).
//
// Input allocation is excluded from the measurement via
// `iter_batched(setup, …)` so the timing reflects the marshal +
// dispatch + decode, not `String::clone`.

fn bench_string_roundtrip(c: &mut Criterion, module: &LeanModule<'_, '_>) {
    let identity: LeanExported<'_, '_, (String,), String> = module
        .exported::<(String,), String>("lean_rs_fixture_string_identity")
        .expect("lookup string_identity");

    let mut group = c.benchmark_group("abi::string_roundtrip");
    for &bytes in &[16_usize, 256, 4096] {
        let template = "a".repeat(bytes);
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(BenchmarkId::from_parameter(bytes), &template, |b, source| {
            b.iter_batched(
                || source.clone(),
                |owned| {
                    let echoed = identity.call(black_box(owned)).expect("call");
                    black_box(echoed);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// -- array conversion ----------------------------------------------------
//
// Workload: `abi::array_string_roundtrip/<n>` — `LeanExported<(Vec<String>,),
// Vec<String>>::call` against `lean_rs_fixture_array_string_identity`
// (fixtures/lean/LeanRsFixture/Containers.lean:7). Covers the
// `Array T` ABI: per-element `String` encode, container-level
// `lean_alloc_ctor`/`Array.mk` allocation, and the symmetric decode
// path. Elements are 8-byte ASCII so the per-element string allocation
// is a single small-object alloc.
//
// `ByteArray` is intentionally excluded — its `from_bytes` / `to_vec`
// helpers (crates/lean-rs/src/abi/bytearray.rs:40, 69) are `pub(crate)`
// because `Vec<u8>` also names `Array UInt8` and overloading would
// pick a Lean shape by accident (see the module comment for the
// disambiguation rationale). The boxed-array path measured here covers
// the same allocation patterns at the public surface.

fn bench_array_string_roundtrip(c: &mut Criterion, module: &LeanModule<'_, '_>) {
    let identity: LeanExported<'_, '_, (Vec<String>,), Vec<String>> = module
        .exported::<(Vec<String>,), Vec<String>>("lean_rs_fixture_array_string_identity")
        .expect("lookup array_string_identity");

    let mut group = c.benchmark_group("abi::array_string_roundtrip");
    for &n in &[1_usize, 16, 256] {
        let template: Vec<String> = (0..n).map(|i| format!("elt{i:04}")).collect();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &template, |b, source| {
            b.iter_batched(
                || source.clone(),
                |owned| {
                    let echoed = identity.call(black_box(owned)).expect("call");
                    black_box(echoed);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// -- harness -------------------------------------------------------------

fn criterion_benchmarks(c: &mut Criterion) {
    let library = open_fixture();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("module initialiser succeeds");

    bench_scalar_dispatch(c, &module);
    bench_string_roundtrip(c, &module);
    bench_array_string_roundtrip(c, &module);
}

criterion_group!(benches, criterion_benchmarks);
criterion_main!(benches);
