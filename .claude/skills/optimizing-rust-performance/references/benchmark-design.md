# Benchmark Design And Regression Tracking

Kan needs both narrow and broad measurements. Use the smallest benchmark that answers the question, then expand scope if
the change can move cost elsewhere.

## Choose The Right Benchmark Shape

### Microbenchmark

Use when:

- you already know the suspected hot function or loop
- you are choosing between two local representations
- you need fast iteration before broader confirmation

Place it in the nearest crate bench file or add a new bench beside related ones.

### Scenario Benchmark

Use when:

- the hot path depends on realistic term shape, module shape, or cache state
- the complaint is editor latency, one-file compile cost, or a specific pipeline stage

Good existing surfaces:

- `single_file/list` in `crates/pipeline/build/benches/pipeline_bench.rs`
- `profile_interactive`
- regression benches in `crates/frontend/typecheck-infer/benches/regression_bench.rs`

### End-To-End Benchmark

Use when:

- the change crosses pass boundaries
- caching or invalidation changes
- a micro win might lose in total throughput

Good existing surfaces:

- `full_build` and `per_stage` in `crates/pipeline/build/benches/pipeline_bench.rs`
- `profile_full_build`
- `collect_baseline_quick` and `collect_baseline_full`

## Criterion Guidance

Use Criterion's comparison features instead of ad hoc timing loops.

Useful commands:

```bash
cargo bench -p kan-typecheck-infer --bench unification_bench -- --save-baseline before
cargo bench -p kan-typecheck-infer --bench unification_bench -- --baseline before
cargo bench -p kan-typecheck-infer --bench unification_bench -- --profile-time 10
```

Design rules:

- Benchmark the operation of interest, not just fixture construction.
- Put expensive, invariant setup outside `b.iter` when the workload under test is steady-state behavior.
- Keep setup inside `b.iter` only when setup cost is part of the real complaint.
- Use parameterized families, not one magic size.
- Use realistic term shapes, not only best-case atoms.
- Use `black_box` around inputs and results that the optimizer could otherwise erase.

## Allocation Benchmarks

Use heap profiling when allocation pressure is plausible.

Repo-specific options:

- `cargo run --profile profiling -p kan-profiling --bin collect_baseline_full`
- `cargo bench -p kan-typecheck-infer --bench dhat_profile --features dhat-heap`

Questions to answer:

- total allocations or bytes
- peak live heap
- whether fewer allocations increase retained memory
- whether allocation reduction actually improves wall time

## Interpreting Results

Be skeptical of small deltas.

- If a change is within noise, scale the workload or gather more samples.
- If a microbench improves but total compile time does not, the bottleneck moved or the microbench was too narrow.
- If time improves and memory worsens, report both.
- If memory improves and time worsens, report both.
- If only cold-cache behavior changes, say that clearly.

Do not over-claim:

- one run is not enough
- one size is not enough
- one machine-local profiler trace is not enough for a broad claim

## Common Bogus Conclusions

- "The new type is faster" when the benchmark mostly measured construction.
- "SmallVec helped" with no inline-size distribution.
- "FxHashMap helped" when the real win came from changing keys to integers.
- "This branch is cheaper" when the broader pipeline now recomputes more.
- "This microbench is green" when cached and uncached behavior differ and only one was measured.

## When To Add A New Bench Or Hook

Add measurement support when:

- you found a hot path with no stable reproducer
- an existing bench is too synthetic to catch the regression class
- reviewers would otherwise have to trust a performance claim on sight
- the repo has a profiling hook nearby but not at the granularity needed

Keep new support near the code it protects unless the workload is intentionally shared across the compiler.

## Review Checklist

For a performance-sensitive diff, ask:

- What exact workload was used?
- Is that workload representative?
- What were the before and after numbers?
- Did the author check a broader workload?
- Did they check both time and memory if the change could trade them off?
- Is there now a bench or profiling path that will catch future regressions?
