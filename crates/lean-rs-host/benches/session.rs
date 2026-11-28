//! Session-level benchmarks for `lean_rs::host`: bulk environment
//! query, bulk introspection, elaboration, bounded `MetaM` (`whnf`), and
//! cached module queries. Each workload names the user-visible code path
//! it represents.
//!
//! All workloads share one process and one `LeanRuntime`—Lean's
//! initialiser is `OnceLock`-cached and re-running it inside `b.iter`
//! would measure only the cached fast-path. Each bench builds its session
//! once, outside `b.iter`, so the recorded numbers reflect the
//! steady-state hot path, not import cost.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, clippy::unwrap_used)]

#[path = "../tests/support/fixed_fixture.rs"]
mod fixed_fixture;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use lean_rs::LeanRuntime;
use lean_rs_host::host::process::{ModuleQueryCachePolicy, ModuleQueryOutputBudgets, ModuleQuerySelector};
use lean_rs_host::meta::LeanMetaOptions;
use lean_rs_host::meta::whnf;
use lean_rs_host::{LeanCapabilities, LeanElabOptions, LeanHost, LeanProgressEvent, LeanProgressSink};

// -- fixture bring-up ---------------------------------------------------

fn fixture_host() -> LeanHost<'static> {
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");
    LeanHost::from_lake_project(runtime, fixed_fixture::fixture_root()).expect("Lake project opens")
}

// -- query_declarations_bulk --------------------------------------------
//
// Workload: `host::session::query_declarations_bulk/N`—
// `LeanSession::query_declarations_bulk` (crates/lean-rs/src/host/session.rs:704)
// over N pre-resolvable fixture declaration names. Measures the bulk
// dispatch: one FFI call regardless of N, plus N internal
// `name_from_string` conversions and N `Obj` decodes. The session is
// re-used across iterations—only the bulk call is in the measurement.

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
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
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

// -- progress callback delivery -----------------------------------------
//
// Workload: `host::session::query_declarations_bulk_progress/16`—the
// same bulk declaration query as above, but with a progress sink wired
// through the Lean callback trampoline. This is the callback-delivery
// baseline for future progress API changes.

#[derive(Default)]
struct CountingSink {
    events: AtomicU64,
}

impl LeanProgressSink for CountingSink {
    fn report(&self, event: LeanProgressEvent) {
        black_box(event);
        self.events.fetch_add(1, Ordering::Relaxed);
    }
}

fn bench_query_declarations_bulk_progress(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let mut session = caps
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
        .expect("Handles imports cleanly");
    let sink = CountingSink::default();
    let names = &HANDLES_NAMES[..16];

    let mut group = c.benchmark_group("host::session::query_declarations_bulk_progress");
    group.throughput(Throughput::Elements(names.len() as u64));
    group.bench_function("16", |b| {
        b.iter(|| {
            let before = sink.events.load(Ordering::Relaxed);
            let decls = session
                .query_declarations_bulk(black_box(names), None, Some(&sink))
                .expect("progress bulk query");
            black_box(decls);
            black_box(sink.events.load(Ordering::Relaxed).saturating_sub(before));
        });
    });
    group.finish();
}

// -- declaration_*_bulk -------------------------------------------------
//
// Workload: `host::session::declaration_*_bulk_vs_loop/5000`—each
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
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
        .expect("Handles imports cleanly");
    let mut loop_session = caps
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
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
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
        .expect("Handles imports cleanly");
    let mut loop_session = caps
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
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
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
        .expect("Handles imports cleanly");
    let mut loop_session = caps
        .session(&fixed_fixture::imports(caps, "LeanRsFixture.Handles"), None, None)
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
// Workload: `host::session::elaborate_small`—`LeanSession::elaborate`
// (crates/lean-rs/src/host/session.rs:471) on `(1 + 1 : Nat)`. The
// elaborator runs against the cached `Elaboration` environment; the
// measurement reflects warm steady-state cost. `elaborate` accumulates
// session state across calls, so the first call in a fresh session is
// systematically slower than the steady-state number recorded here—
// see `docs/performance/baseline.md` for the caveat.

fn bench_elaborate(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let mut session = caps
        .session(&fixed_fixture::imports(caps, "LeanRsHostShims.Elaboration"), None, None)
        .expect("Elaboration imports cleanly");
    let term = fixed_fixture::term(caps, &session, "(1 + 1 : Nat)");

    c.bench_function("host::session::elaborate_small", |b| {
        b.iter(|| {
            let outcome = session
                .elaborate(black_box(&term), None)
                .expect("host stack reports no exception");
            let expr = outcome.expect("elaboration succeeds");
            black_box(expr);
        });
    });
}

// -- run_meta(whnf) -----------------------------------------------------
//
// Workload: `host::meta::run_meta_whnf`—`LeanSession::run_meta`
// (crates/lean-rs/src/host/session.rs:640) with the `whnf` service
// (crates/lean-rs/src/host/meta/service.rs:98) on the type of
// `Nat.zero`. The expression is built once in setup so the per-iter
// cost is `LeanExpr::clone` + meta dispatch + decode—not Expr
// construction.

fn bench_run_meta_whnf(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let mut session = caps
        .session(&fixed_fixture::imports(caps, "LeanRsHostShims.Meta"), None, None)
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

// -- module-query object decoding ---------------------------------------
//
// Workload: `host::process::module_query_batch_cached_decode`—
// `LeanSession::process_module_query_batch_cached` over a tiny module.
// The Lean result decodes host query envelopes plus cache facts, including
// scalar-tail `UInt64` timings, `UInt8` cache status, and constructor tags.

fn bench_module_query_batch_cached_decode(c: &mut Criterion, caps: &LeanCapabilities<'_, '_>) {
    let mut session = caps
        .session(&fixed_fixture::imports(caps, "LeanRsHostShims.InfoTree"), None, None)
        .expect("InfoTree imports cleanly");
    let source = "def benchValue : Nat := 1\n#check benchValue\n";
    let selectors = vec![
        ModuleQuerySelector::Diagnostics {
            id: "diagnostics".to_owned(),
        },
        ModuleQuerySelector::TypeAt {
            id: "type_at".to_owned(),
            line: 1,
            column: 20,
        },
    ];
    let budgets = ModuleQueryOutputBudgets::default();
    let options = LeanElabOptions::new();
    let policy = ModuleQueryCachePolicy {
        file_identity: "bench.lean".to_owned(),
        key: "module-query-batch-cached-decode".to_owned(),
        max_entries: 8,
        ttl_millis: 60_000,
        max_bytes: 1_048_576,
    };

    let (env, imports) = session.source_context();
    let batch = lean_rs::host_bridge::HostModuleBatch {
        source: source.into(),
        selectors,
        budgets,
    };
    let cached = lean_rs::host_bridge::HostCachedModuleBatch {
        batch,
        policy: format!(
            "{}\n{}\n{}\n{}\n{}",
            policy.file_identity, policy.key, policy.max_entries, policy.ttl_millis, policy.max_bytes
        ),
    };
    #[allow(unsafe_code, reason = "fixed benchmark case under qualified fixture custody")]
    // SAFETY: the request above contains one fixed declaration, two fixed projections
    // and bounded cache policy in this exact InfoTree environment with default options.
    let request = unsafe {
        caps.shim_capability().cached_module_batch_scope(
            env,
            imports,
            fixed_fixture::resolution(),
            cached,
            options.source_options(),
        )
    }
    .expect("attest fixed benchmark");

    c.bench_function("host::process::module_query_batch_cached_decode", |b| {
        b.iter(|| {
            let outcome = session
                .process_module_query_batch_cached(black_box(&request), None)
                .expect("cached module query");
            black_box(outcome);
        });
    });
}

// -- harness ------------------------------------------------------------

fn criterion_benchmarks(c: &mut Criterion) {
    let host = fixture_host();
    let caps = fixed_fixture::capabilities(&host).expect("capabilities load");

    bench_query_declarations_bulk(c, &caps);
    bench_query_declarations_bulk_progress(c, &caps);
    bench_declaration_type_bulk_vs_loop(c, &caps);
    bench_declaration_kind_bulk_vs_loop(c, &caps);
    bench_declaration_name_bulk_vs_loop(c, &caps);
    bench_elaborate(c, &caps);
    bench_run_meta_whnf(c, &caps);
    bench_module_query_batch_cached_decode(c, &caps);
}

criterion_group!(benches, criterion_benchmarks);
criterion_main!(benches);
