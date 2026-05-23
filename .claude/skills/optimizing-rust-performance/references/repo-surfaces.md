# Repo Measurement Surfaces

Your workspace likely already has useful measurement support. Use it before inventing new infrastructure.

## Look For Existing Criterion Benches

The conventions are stable across Rust workspaces. Look for:

- `benches/` directories under each crate (`crates/<name>/benches/*.rs`).
- A `[[bench]]` table in each crate's `Cargo.toml`.
- Names that hint at scope: `*_bench.rs`, `unification_bench.rs`, `pipeline_bench.rs`, `regression_bench.rs`,
  `dhat_profile.rs`.

The fastest way to enumerate benches is:

```bash
rg --files crates | rg '/benches/'
cargo bench --workspace --list 2>/dev/null
```

When narrowing to one bench, prefer the smallest crate that exercises the suspected hot path. End-to-end pipeline
benches are useful for confirmation, not for fast iteration.

## Look For A Shared Profiling Crate

Compiler-style workspaces often factor profiling into a dedicated crate (`profiling/`, `bench/`, `<name>-profiling/`)
with one or more binaries that drive realistic workloads end to end. Common shapes:

- `bin/profile_frontend.rs` — frontend-only flamegraph and pprof capture.
- `bin/profile_full_build.rs` — full fixture build orchestration.
- `bin/profile_interactive.rs` — single-file editor-latency style workload.
- `bin/profile_parser.rs` — parse-only workload.
- `bin/collect_baseline_quick.rs` — timing-oriented baseline.
- `bin/collect_baseline_full.rs` — timing plus allocation-aware baseline via DHAT.

Plus supporting shell scripts under `scripts/` for sample-based capture (`profile.sh`, `profile_with_samply.sh`,
`analyze_profile.sh`).

## Look For Existing Profiling Hooks In Code

Many workspaces install profiling spans, observers, or stage-exit hooks at the natural boundaries of the compiler.
Search for them before adding new ones:

```bash
rg -n "profiling|with_profiling_observer|on_stage_exit|ProfilerGuard|dhat|criterion_group" crates profiling
```

Common attach points: the pipeline driver, unification dispatch, constraint-solving loops, the evaluation engine, and
the term store.

## What Is Usually Missing Or Fragmented

The pattern across compiler-style workspaces:

- Microbenches are stronger than throughput benchmarks. Crates ship per-crate benches that exercise inner loops well but
  do not compose into an end-to-end story.
- Profiling crates, when they exist, are usually richer than per-crate `tools/cli` shims — the CLI shim is build-profile
  selection, not a performance workflow.
- Some hot areas rely on comments or one-off benches rather than a shared regression suite.

Implication:

- Start with the closest existing bench.
- If the change might affect overall compile latency, also run a broader pipeline or end-to-end workload.
- If a hot path lacks a stable reproducer, add one in the nearest crate bench first, then consider whether the shared
  profiling crate needs a new workload.

## When To Add Measurement Support

Add or expand a bench or profiling surface when:

- the suspected hot path has no stable reproducer
- the only current bench measures the wrong thing, such as setup instead of steady-state work
- a change touches a pass boundary, cache, or invalidation policy that microbenches cannot cover
- reviewers would otherwise have no credible way to detect regressions later
