---
name: optimizing-rust-performance
description: 'Use for measurement-driven Rust performance work: throughput, allocations, memory footprint, cache locality, hot-path cloning, layout, hashing, pipeline latency, profiling, benches, regressions.'
---

# Optimizing Rust Performance

Use this skill for measurement-driven Rust performance work. The default job is not "make it faster." The job is:
pick a real workload, measure a baseline, localize the bottleneck, choose the right intervention level, change code, and
re-measure.

## Load The Right References

- Start with [`references/workflow.md`](references/workflow.md) for every task.
- Read [`references/repo-surfaces.md`](references/repo-surfaces.md) when choosing benches, profiling commands, or where
    to add new measurement support.
- Read [`references/hotspot-classes.md`](references/hotspot-classes.md) when the issue is in normalization, evaluation,
    type checking, unification, traversal, registry lookup, or pipeline throughput.
- Read [`references/intervention-patterns.md`](references/intervention-patterns.md) before changing ownership,
    allocation, layout, hashing, indexing, caching, or branch structure.
- Read [`references/benchmark-design.md`](references/benchmark-design.md) when adding benches, interpreting variance,
    checking regressions, or reviewing performance claims.
- Read [`references/external-lessons.md`](references/external-lessons.md) when you need design pressure from strong Rust
    codebases and tooling rather than repo-local habits.

## Workflow

1. Name the workload before touching code.
1. Reuse an existing bench or profiling workload if one exists close to the suspected hot path.
1. If measurement is missing, add the smallest credible bench or profiling hook first.
1. Localize whether the problem is time, allocation rate, memory footprint, cache/layout, or repeated recomputation.
1. Choose the lowest-risk intervention that matches the bottleneck class.
1. Re-measure the changed path and one broader workload that could regress.
1. Report numbers, workload, command, and remaining uncertainty.

## Hard Rules

- Do not optimize from intuition alone.
- Do not claim a win from a toy microbenchmark when the change affects pipeline throughput.
- Do not default to SIMD, parallelism, custom allocators, or `#[inline(always)]` before measuring and exhausting
    algorithm, representation, and allocation/layout changes.
- Do not switch data structures because they are fashionable. State the key shape, access pattern, mutation pattern, and
    expected lifetime first.
- Do not ignore memory to report a speedup, or ignore speed to report lower allocation count. Check both when the change
    could trade them off.
- Do not accept a performance-sensitive diff without asking what workload proves the win and what broader workload
    guards against regressions.

## Workspace-specific priorities

- Prefer fixing phase-local allocation, clone pressure, and redundant recomputation before micro-tuning instruction
    count.
- Treat normalization/evaluation, typechecker/unifier paths, traversal code, registry/metadata lookup, and pipeline pass
    boundaries as first-class performance surfaces in compiler-style workloads.
- Arena lifetime design matters. Distinguish phase-local data from data that must survive across caches or pass
    boundaries.
- Many existing benches are microbenches. Use them to localize, then confirm with a broader pipeline or end-to-end
    workload when the change can affect end-to-end throughput.

## Review Mode

When reviewing a performance-sensitive diff, focus on:

- Is there a representative benchmark or profile for the claimed win?
- Is the chosen intervention level appropriate for the bottleneck?
- Does the change shift cost to memory, compile time, invalidation complexity, or another pipeline stage?
- Does it preserve the right lifetime model: arena, borrowed, indexed, interned, cached, or owned?
- Is measurement support now good enough for the next person to detect regressions?
