# Performance Workflow

Use this order. Do not skip ahead.

## 1. Define The Workload

Pick the workload that matches the user-visible complaint.

- Parser or syntax-only suspicion: use parser-only or parse-stage workloads.
- Typechecker, evaluator, normalization, or unification suspicion: start with the nearest crate bench, then confirm with
    a broader frontend workload if the change may affect compile latency.
- End-to-end compile throughput suspicion: use `crates/pipeline/build/benches/pipeline_bench.rs` or the `profiling/`
    binaries first.
- Runtime or interpreter suspicion: use `crates/execution/interpreter/benches/interpreter.rs` or backend benches.

Prefer existing workloads over inventing new ones. If nothing credible exists, add one before changing code.

## 2. Establish A Baseline

Use optimized builds for performance claims.

```bash
cargo bench -p kan-core --bench normalization_bench
cargo bench -p kan-typecheck-infer --bench unification_bench
cargo bench -p kan-build --bench pipeline_bench
```

For broader compiler profiling, Kan already has a shared profiling crate:

```bash
export RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes"

cargo run --profile profiling -p kan-profiling --bin collect_baseline_quick
cargo run --profile profiling -p kan-profiling --bin collect_baseline_full

cargo run --profile profiling -p kan-profiling --bin profile_frontend
cargo run --profile profiling -p kan-profiling --bin profile_full_build
cargo run --profile profiling -p kan-profiling --bin profile_interactive
cargo run --profile profiling -p kan-profiling --bin profile_parser
```

Use `collect_baseline_quick` for iteration and `collect_baseline_full` when allocation pressure matters.

## 3. Localize The Bottleneck

Match tool to symptom.

- CPU time hot path: Criterion, `profile_*`, `./profiling/scripts/profile.sh`, `cargo flamegraph`, or
    `./profiling/scripts/profile_with_samply.sh`.
- Allocation rate or retained heap suspicion: `collect_baseline_full` or
    `crates/frontend/typecheck-infer/benches/dhat_profile.rs`.
- Cache/layout suspicion: inspect sizes, pointer chasing, key choice, and hot/cold field mix after profiling points
    there.
- Compile-time code size suspicion in codegen-heavy crates: consider `cargo llvm-lines` only after runtime or throughput
    profiles point at LLVM/codegen work.

Useful commands:

```bash
./profiling/scripts/profile.sh frontend
./profiling/scripts/profile.sh full-build
./profiling/scripts/profile_with_samply.sh interactive

cargo bench -p kan-typecheck-infer --bench unification_bench -- --save-baseline before
cargo bench -p kan-typecheck-infer --bench unification_bench -- --baseline before
cargo bench -p kan-typecheck-infer --bench unification_bench -- --profile-time 10
```

Use `--profile-time` when attaching a profiler to Criterion benches so Criterion's own sampling logic does not dominate
the capture.

## 4. Choose The Intervention Level

Use this order unless the data clearly says otherwise.

1. Remove work: algorithm, invalidation, batching, deduplication, cache scope.
1. Fix representation: indexed handles, interning, side tables, borrow vs own, environment representation.
1. Fix allocation strategy: arena, scratch reuse, exact capacity, fewer transient vectors or maps.
1. Fix layout and locality: contiguous storage, smaller hot structs, hot/cold split, integer keys, branch shape.
1. Fix hashing or lookup strategy.
1. Only then consider low-level tuning, build flags, or backend-specific work.

## 5. Re-Measure

After the code change:

- Re-run the exact benchmark or profile used for the baseline.
- Re-run one broader workload that could regress elsewhere.
- If the change affects ownership, allocation, or caching, check both time and memory.

Evidence should include:

- workload name
- command
- before and after numbers
- whether caches were warm or cold
- whether the result is microbench-only or confirmed end-to-end

## 6. Stop Conditions

Stop and collect better data when:

- the measured change is small enough to be lost in noise
- the benchmark does not match the real workload
- the only visible win comes from changing setup code, not the hot path
- you can only show a microbench win for a change that obviously shifts work elsewhere
- you suspect the bottleneck moved but have not re-profiled

## Failure Smells

- "This should be faster" with no workload.
- Reporting only percent speedup with no raw numbers.
- Measuring debug builds.
- Benchmarking term construction when the complaint is normalization, conversion, or unification.
- Declaring victory from one bench while pipeline throughput or memory gets worse.
- Changing data structure, allocator, or hasher without characterizing keys, sizes, or lifetime.
