//! Session-level benchmarks for `lean_rs::host`: bulk environment
//! query, bulk introspection, elaboration, bounded `MetaM` (`whnf`), and `SessionPool`
//! reuse. Each workload names the user-visible code path it represents.
//!
//! All four workloads share one process and one `LeanRuntime` — Lean's
//! initialiser is `OnceLock`-cached and re-running it inside `b.iter`
//! would measure only the cached fast-path. Per-bench session and pool
//! construction happens once outside `b.iter`, so the recorded numbers
//! reflect the steady-state hot path, not import cost.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, clippy::unwrap_used)]

use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use lean_rs::LeanRuntime;
use lean_rs_host::meta::LeanMetaOptions;
use lean_rs_host::meta::whnf;
use lean_rs_host::{LeanCapabilities, LeanElabOptions, LeanHost, SessionPool};

// -- fixture bring-up ---------------------------------------------------

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn fixture_host() -> LeanHost<'static> {
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("Lake project opens")
}

// -- query_declarations_bulk --------------------------------------------
//
// Workload: `host::session::query_declarations_bulk/N` —
// `LeanSession::query_declarations_bulk` (crates/lean-rs/src/host/session.rs:704)
// over N pre-resolvable fixture declaration names. Measures the bulk
// dispatch: one FFI call regardless of N, plus N internal
// `name_from_string` conversions and N `Obj` decodes. The session is
// re-used across iterations — only the bulk call is in the measurement.

const HANDLES_NAMES: [&str; 16] = [
    "LeanRsFixture.Handles.nameAnonymous",
    "LeanRsFixture.Handles.nameMkStr",
    "LeanRsFixture.Handles.nameMkNum",
    "LeanRsFixture.Handles.nameToString",
    "LeanRsFixture.Handles.nameBeq",
    "LeanRsFixture.Handles.levelZero",
    "LeanRsFixture.Handles.levelSucc",
    "LeanRsFixture.Handles.exprConstNat",
    "LeanRsFixture.Handles.nameAnonymous",
    "LeanRsFixture.Handles.nameMkStr",
    "LeanRsFixture.Handles.nameMkNum",
    "LeanRsFixture.Handles.nameToString",
    "LeanRsFixture.Handles.nameBeq",
    "LeanRsFixture.Handles.levelZero",
    "LeanRsFixture.Handles.levelSucc",
    "LeanRsFixture.Handles.exprConstNat",
];

fn bench_query_declarations_bulk(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let mut session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("Handles imports cleanly");

    let mut group = c.benchmark_group("host::session::query_declarations_bulk");
    for &n in &[1_usize, 4, 8, 16] {
        let names = &HANDLES_NAMES[..n];
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &names, |b, names| {
            b.iter(|| {
                let decls = session
                    .query_declarations_bulk(black_box(names), None, None)
                    .expect("bulk query");
                black_box(decls);
            });
        });
    }
    group.finish();
}

// -- declaration_*_bulk -------------------------------------------------
//
// Workload: `host::session::declaration_*_bulk_vs_loop/5000` — each
// method over 5k declaration names, compared against the singular
// per-name loop with the same output allocation shape. The batch uses
// repeated fixture names so the measurement isolates FFI round-trips and
// result decoding instead of import or environment construction.

const INTROSPECTION_NAME: &str = "LeanRsFixture.Handles.nameAnonymous";

fn introspection_names_5k() -> Vec<&'static str> {
    vec![INTROSPECTION_NAME; 5_000]
}

fn bench_declaration_type_bulk_vs_loop(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let names = introspection_names_5k();
    let mut bulk_session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("Handles imports cleanly");
    let mut loop_session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("Handles imports cleanly");

    let mut group = c.benchmark_group("host::session::declaration_type_bulk_vs_loop");
    group.throughput(Throughput::Elements(names.len() as u64));
    group.sample_size(10);
    group.bench_function("bulk_5000", |b| {
        b.iter(|| {
            let types = bulk_session
                .declaration_type_bulk(black_box(names.as_slice()), None, None)
                .expect("bulk type query");
            black_box(types);
        });
    });
    group.bench_function("loop_5000", |b| {
        b.iter(|| {
            let types: Vec<_> = names
                .iter()
                .map(|name| {
                    loop_session
                        .declaration_type(black_box(*name), None)
                        .expect("type query")
                })
                .collect();
            black_box(types);
        });
    });
    group.finish();
}

fn bench_declaration_kind_bulk_vs_loop(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let names = introspection_names_5k();
    let mut bulk_session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("Handles imports cleanly");
    let mut loop_session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("Handles imports cleanly");

    let mut group = c.benchmark_group("host::session::declaration_kind_bulk_vs_loop");
    group.throughput(Throughput::Elements(names.len() as u64));
    group.sample_size(10);
    group.bench_function("bulk_5000", |b| {
        b.iter(|| {
            let kinds = bulk_session
                .declaration_kind_bulk(black_box(names.as_slice()), None, None)
                .expect("bulk kind query");
            black_box(kinds);
        });
    });
    group.bench_function("loop_5000", |b| {
        b.iter(|| {
            let kinds: Vec<_> = names
                .iter()
                .map(|name| {
                    loop_session
                        .declaration_kind(black_box(*name), None)
                        .expect("kind query")
                })
                .collect();
            black_box(kinds);
        });
    });
    group.finish();
}

fn bench_declaration_name_bulk_vs_loop(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let names = introspection_names_5k();
    let mut bulk_session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("Handles imports cleanly");
    let mut loop_session = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("Handles imports cleanly");

    let mut group = c.benchmark_group("host::session::declaration_name_bulk_vs_loop");
    group.throughput(Throughput::Elements(names.len() as u64));
    group.sample_size(10);
    group.bench_function("bulk_5000", |b| {
        b.iter(|| {
            let rendered = bulk_session
                .declaration_name_bulk(black_box(names.as_slice()), None, None)
                .expect("bulk name query");
            black_box(rendered);
        });
    });
    group.bench_function("loop_5000", |b| {
        b.iter(|| {
            let rendered: Vec<_> = names
                .iter()
                .map(|name| {
                    loop_session
                        .declaration_name(black_box(*name), None)
                        .expect("name query")
                })
                .collect();
            black_box(rendered);
        });
    });
    group.finish();
}

// -- elaborate ----------------------------------------------------------
//
// Workload: `host::session::elaborate_small` — `LeanSession::elaborate`
// (crates/lean-rs/src/host/session.rs:471) on `(1 + 1 : Nat)`. The
// elaborator runs against the cached `Elaboration` environment; the
// measurement reflects warm steady-state cost. `elaborate` accumulates
// session state across calls, so the first call in a fresh session is
// systematically slower than the steady-state number recorded here —
// see `docs/performance/baseline.md` for the caveat.

fn bench_elaborate(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let mut session = caps
        .session(&["LeanRsHostShims.Elaboration"], None, None)
        .expect("Elaboration imports cleanly");
    let opts = LeanElabOptions::new();

    c.bench_function("host::session::elaborate_small", |b| {
        b.iter(|| {
            let outcome = session
                .elaborate(black_box("(1 + 1 : Nat)"), None, &opts, None)
                .expect("host stack reports no exception");
            let expr = outcome.expect("elaboration succeeds");
            black_box(expr);
        });
    });
}

// -- run_meta(whnf) -----------------------------------------------------
//
// Workload: `host::meta::run_meta_whnf` — `LeanSession::run_meta`
// (crates/lean-rs/src/host/session.rs:640) with the `whnf` service
// (crates/lean-rs/src/host/meta/service.rs:98) on the type of
// `Nat.zero`. The expression is built once in setup so the per-iter
// cost is `LeanExpr::clone` + meta dispatch + decode — not Expr
// construction.

fn bench_run_meta_whnf(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let mut session = caps
        .session(&["LeanRsHostShims.Meta"], None, None)
        .expect("Meta imports cleanly");
    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query for Nat.zero")
        .expect("Nat.zero has a type");
    let opts = LeanMetaOptions::new();
    let service = whnf();

    c.bench_function("host::meta::run_meta_whnf", |b| {
        b.iter(|| {
            let outcome = session
                .run_meta(black_box(&service), expr.clone(), black_box(&opts), None)
                .expect("host stack reports no exception");
            black_box(outcome);
        });
    });
}

// -- SessionPool reuse hit ----------------------------------------------
//
// Workload: `host::pool::session_reuse_hit` — `SessionPool::acquire`
// (crates/lean-rs/src/host/pool.rs:130+) on a warm pool. After a
// warm-up acquire+drop, the free list holds one entry; each measured
// iteration is a hot-cache LIFO pop, drop returns it. Imports are
// performed once outside the measurement so the recorded cost is the
// pool's bookkeeping + session re-binding, not Lean import work.

fn bench_session_pool_reuse_hit(c: &mut Criterion, runtime: &'static LeanRuntime, caps: &LeanCapabilities<'_, '_>) {
    let pool = SessionPool::with_capacity(runtime, 1);
    let imports = ["LeanRsFixture.Handles"];

    // Warm the pool: one acquire + drop seeds the free list with a
    // ready-to-reuse environment.
    drop(pool.acquire(caps, &imports, None, None).expect("warm-up acquire"));

    c.bench_function("host::pool::session_reuse_hit", |b| {
        b.iter(|| {
            let sess = pool
                .acquire(black_box(caps), black_box(&imports), None, None)
                .expect("acquire");
            black_box(&sess);
            drop(sess);
        });
    });
}

// -- harness ------------------------------------------------------------

fn criterion_benchmarks(c: &mut Criterion) {
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("capabilities load");

    bench_query_declarations_bulk(c, &caps);
    bench_declaration_type_bulk_vs_loop(c, &caps);
    bench_declaration_kind_bulk_vs_loop(c, &caps);
    bench_declaration_name_bulk_vs_loop(c, &caps);
    bench_elaborate(c, &caps);
    bench_run_meta_whnf(c, &caps);
    bench_session_pool_reuse_hit(c, runtime, &caps);
}

criterion_group!(benches, criterion_benchmarks);
criterion_main!(benches);
