# Repo Measurement Surfaces

Kan already has useful measurement support. Use it before inventing new infrastructure.

## Criterion Benches

### Kernel Core

- `crates/kernel/core/benches/normalization_bench.rs` Focus: beta reduction, normalization, nested lets, simple value
    checks.
- `crates/kernel/core/benches/traversal_bench.rs` Focus: iterative traversal and term erasure under deep structures.
- `crates/kernel/core/benches/registry_cache_bench.rs` Focus: cold vs cached `DefRegistry` lookup.
- `crates/kernel/core/benches/pattern_compilation_bench.rs` Focus: case-tree and constructor-pattern matching behavior.

### Kernel Eval / Typecheck

- `crates/kernel/core-eval/benches/eval_bench.rs` Focus: eval-and-quote and quote-only paths.
- `crates/kernel/core-typecheck/benches/typecheck_bench.rs` Focus: usage info and context checkpoint operations.

### Frontend Inference

- `crates/frontend/typecheck-infer/benches/typecheck_bench.rs` Focus: synthesis and checking microbenches.
- `crates/frontend/typecheck-infer/benches/unification_bench.rs` Focus: unification, constraint queue, meta creation,
    and definitions-map pressure.
- `crates/frontend/typecheck-infer/benches/optimization_bench.rs` Focus: combined typechecker hot paths.
- `crates/frontend/typecheck-infer/benches/regression_bench.rs` Focus: regression guards for equality, closure
    instantiation, env lookup, neutral conversion.
- `crates/frontend/typecheck-infer/benches/dhat_profile.rs` Focus: heap profiling for unification-heavy paths.

### Pipeline / Runtime / Backend

- `crates/pipeline/build/benches/pipeline_bench.rs` Focus: stdlib frontend throughput, stage comparison, scaling,
    single-file latency.
- `crates/execution/interpreter/benches/interpreter.rs` Focus: runtime interpreter arithmetic, closure application, env
    lookup.
- `crates/backend/llvm-backend/benches/performance.rs` Focus: LLVM backend compile-time behavior, not end-to-end runtime
    of generated code.

## Profiling Crate

The `profiling/` workspace crate is the shared profiling surface for broader compiler workloads.

- `profiling/src/bin/profile_frontend.rs` Frontend-only stdlib compile flamegraph and pprof capture.
- `profiling/src/bin/profile_full_build.rs` Full fixture build orchestration.
- `profiling/src/bin/profile_interactive.rs` Single-file editor-latency style workload.
- `profiling/src/bin/profile_parser.rs` Parse-only workload.
- `profiling/src/bin/collect_baseline_quick.rs` Timing-oriented baseline.
- `profiling/src/bin/collect_baseline_full.rs` Timing plus allocation-aware baseline via DHAT.

Supporting scripts:

- `profiling/scripts/profile.sh`
- `profiling/scripts/profile_with_samply.sh`
- `profiling/scripts/analyze_profile.sh`

Outputs go to `profiling_results/`.

## Existing Profiling Hooks In Code

- `crates/pipeline/build/src/pipeline/compiler.rs`
- `crates/pipeline/build/src/compilation_pipeline.rs`
- `crates/frontend/typecheck-infer/src/unification/dispatch.rs`
- `crates/frontend/typecheck-infer/src/constraints/solving.rs`
- `crates/kernel/core-eval/src/engine/trampoline.rs`
- `crates/kernel/core-eval/src/semantics/store.rs`

Search patterns:

```bash
rg -n "profiling|with_profiling_observer|on_stage_exit|ProfilerGuard|dhat|criterion_group" crates profiling
rg --files crates | rg '/benches/'
```

## What Is Missing Or Fragmented

The repo has many good local benches, but they are fragmented by crate and level.

- Microbenches are stronger than throughput benchmarks today.
- The shared profiling crate is stronger than `crates/tools/cli/src/profile.rs`; the CLI file is only build-profile
    selection, not a performance workflow.
- Some hot areas still rely on comments or one-off benches rather than a shared regression suite.

Implication:

- Start with the closest existing bench.
- If the change might affect overall compile latency, also run `pipeline_bench` or a `kan-profiling` workload.
- If a hot path lacks a stable reproducer, add one in the nearest crate bench first, then consider whether the profiling
    crate needs a new shared workload.

## When To Add Measurement Support

Add or expand a bench or profiling surface when:

- the suspected hot path has no stable reproducer
- the only current bench measures the wrong thing, such as setup instead of steady-state work
- a change touches a pass boundary, cache, or invalidation policy that microbenches cannot cover
- reviewers would otherwise have no credible way to detect regressions later
