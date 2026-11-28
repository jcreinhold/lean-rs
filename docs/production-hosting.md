# Production Rust Hosting

Use bounded worker children for long-running import-heavy services. Process exit reclaims Lean's process-lifetime import
state; dropping sessions or clearing caches does not. Measure the workload, then configure the worker count, retained
import-byte budget, warm-session capacity, RSS ceilings, and request limits together.

```rust
use std::time::Duration;

use lean_rs_worker_parent::{LeanWorkerHostHandleBuilder, LeanWorkerRestartPolicy};

// Set every budget from measurements on the deployment machine.
let mut host = LeanWorkerHostHandleBuilder::shims_only(lake_project_root, Vec::<String>::new())
    .worker_executable(worker_child_binary)
    .session_pool_capacity(8)
    .restart_policy(
        LeanWorkerRestartPolicy::default()
            .max_import_residue_bytes(import_residue_budget_bytes)
            .max_imports(32)
            .max_rss_kib(child_rss_ceiling_kib),
    )
    .rss_hard_limit(child_rss_kill_kib, Duration::from_millis(250))
    .open()?;

let outcome =
    host.process_module_query_batch_with_imports(imports, source, &selectors, &budgets, &options, None, None)?;
```

The capacity, import-count and sample-interval values illustrate separate controls; they are not portable defaults. A
builder given no imports opens no session until the first call names them.

## Choose the bounds

| Control | Purpose |
| --- | --- |
| Worker count | The number of host handles the application starts bounds local concurrency and resident Lean runtimes. |
| `max_import_residue_bytes` | Cycle before further import work after confirmed imports reach the retained-byte budget. |
| `max_imports` | Backstop when import-byte accounting is absent or wrong. |
| `session_pool_capacity` | Bound the number of warm import profiles retained in a child, independently of the restart budget. |
| `max_rss_kib` | Cycle before a request when the sampled child RSS has reached the ceiling. |
| `rss_hard_limit` | Kill and replace the child during a request when sampled RSS reaches the limit. |
| Request timeout | Discard a child generation that fails to complete a request in time. |
| Module-query batches | Reuse a warm session for related queries while preserving item-level outcomes. |

Prefer the residue-byte policy to an import count: equal numbers of imports can retain very different amounts of data.
The counter uses non-memory-mapped compacted-region bytes, including accounted header imports. RSS also counts clean
mapped `.olean` pages and can fall under pressure while retained private bytes grow. The
[memory evidence](safety/long-session-memory.md) explains these measurements.

These are checks at lifecycle boundaries, not hard allocation limits. A generation can complete one import larger than
its residue budget; the next import check cycles it. RSS sampling can be unavailable. Monitor accounting and unavailable
sample counters rather than treating a configured ceiling as proof of a bounded peak.

With several children, the application owns the total: the worker count times the per-child RSS ceiling is a starting
budget when all children may run at once. The profiling scripts use a local 1,572,864 KiB cap, one worker, and one fresh
full-session import per generation. Those are diagnostic workload settings, not library defaults or portable safety
constants.

## Reuse and resource failures

Warm session keys include canonical roots, ordered imports, import profile, metadata expectation, toolchain facts, and
manifest identity where applicable. Keep related work on one host handle and use bounded module-query batches to reduce
session and request churn. Batching does not reclaim Lean memory or replace cycling.

Resource failures remain distinct from Lean query results:

```rust
if let Some(facts) = err.resource_exhausted_facts() {
    eprintln!(
        "resource guard: cause={} work_entered_child={}",
        facts.cause, facts.work_entered_child
    );
}
```

`work_entered_child=false` reports refusal before child work began. Retry after changing the budget, selecting an
equivalent warm key, or cycling according to policy. A true value reports interrupted or degraded work. Module-query
batches can preserve per-selector budget failures in their returned facts. Do not translate either kind into `not_found`
or an empty successful result.

## Same-process hosts

A same-process host has no import budget: each fresh import keeps its state until the process exits. Use one for
short-lived tools and tests, or behind a process boundary the host already has. Full sessions use `loadExts := true`, so
extension state can retain references into compacted `.olean` regions and `Environment.freeRegions` is unsafe.

The bracketed metadata query is a separate path: it imports with `loadExts := false`, serializes owned results, and
frees regions before returning. See [same-process controls](same-process-controls.md) for its restrictions.

## Measure policy changes

Use the [profiling commands](https://github.com/jcreinhold/lean-rs/blob/main/profiling/README.md) for bounded worker
cycling, import admission, and synchronous replacement timings. Use the same-process long-session probe to diagnose
retention, rather than as an unbounded service soak test. See [diagnostics](diagnostics.md) for resource facts and
lifecycle snapshots.
