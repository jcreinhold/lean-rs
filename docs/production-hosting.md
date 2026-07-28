# Production Rust Hosting

Use worker children for long-running hosts. The production shape is:

```rust
use lean_rs_worker_parent::{
    LeanWorkerCapabilityBuilder, LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerRestartPolicy,
};

// What a child cannot reclaim, in bytes. Size it from the machine, not from a
// module count: the same *number* of imports costs 9.6 GiB on one workload and
// 16.0 GiB on another.
const IMPORT_RESIDUE_BUDGET_BYTES: u64 = 9 * 1024 * 1024 * 1024;
const CHILD_RSS_KIB: u64 = 1_572_864;

let pool_config = LeanWorkerPoolConfig::new(1)
    .max_total_child_rss_kib(CHILD_RSS_KIB)
    .per_worker_rss_ceiling_kib(CHILD_RSS_KIB);

let builder = LeanWorkerCapabilityBuilder::new(
    lake_project_root,
    "my_package",
    "MyCapability",
    ["MyCapability"],
)
.worker_executable(worker_child_binary)
// How many warm import profiles one child holds. Independent of the restart
// bound on purpose: derive one from the other and the pool never evicts,
// because the parent recycles the child first.
.session_pool_capacity(8)
.restart_policy(
    LeanWorkerRestartPolicy::default()
        .max_import_residue_bytes(IMPORT_RESIDUE_BUDGET_BYTES)
        // A backstop, not the bound: it catches a child whose import stats come
        // back zeroed, which is the only way the byte accounting can go blind.
        .max_imports(32),
);

let mut pool = LeanWorkerPool::new(pool_config);
let mut lease = pool.acquire_lease(builder)?;

// Run related work through the same warm lease. For proof-state/module-query
// workflows, prefer the bounded batch API over many tiny session requests.
let outcome = lease.process_module_query_batch(source, &selectors, &budgets, &options, None, None)?;
```

Set the worker count, import residue budget, session pool capacity, restart policy, and batching strategy together.
Changing only one of them can reintroduce the failure mode the worker boundary is meant to prevent.

The RSS budgets above are a coarse outer guard, not the cycling policy. RSS counts clean mapped `.olean` pages that cost
nothing to reclaim, and under memory pressure it *falls* while the unreclaimable total keeps growing — see
[`docs/safety/long-session-memory.md`](safety/long-session-memory.md), which opens with the measurements. Bound the
residue; leave RSS as a ceiling you do not expect to reach.

## Why This Shape

Rust cannot reclaim fully loaded Lean import state in-process. Full host sessions use `loadExts := true`; after that,
Lean environment extensions may retain references into compacted `.olean` regions, so `Environment.freeRegions` is not a
safe cleanup tool. The reset boundary for those sessions is process exit.

Rust can still control the operational shape:

| Control | Production pattern |
| --- | --- |
| Admission | Refuse cold work before it starts when the pool, import count, or RSS budget is exhausted. |
| Reuse | Reuse warm sessions keyed by canonical roots, ordered imports, import profile, metadata expectation, and toolchain facts. |
| Scheduling | Keep local worker count bounded and size the total child RSS budget with `max_workers`. |
| Batching | Keep one warm lease open for related module-query work and preserve item-level results. |
| Cycling | Restart worker children once a generation has retained more than a byte budget of import residue. |
| Reporting | Surface `ResourceExhausted` facts instead of returning empty or misleading Lean results. |

The checked-in local defaults use one worker, one fresh full-session import per child, and a 1,572,864 KiB child RSS
budget. Treat those numbers as local operating defaults, not portable safety constants. Larger hosts should measure with
their own cap and set `max_total_child_rss_kib = max_workers * per_worker_rss_ceiling_kib` when they intentionally allow
multiple local children.

## Handling Resource Exhaustion

Resource failures are caller-visible facts, not Lean query failures. A minimal worker-side classifier is:

```rust
fn handle_worker_error(err: lean_rs_worker_parent::LeanWorkerError) -> lean_rs_worker_parent::LeanWorkerError {
    if let Some(facts) = err.resource_exhausted_facts() {
        if facts.work_entered_child {
            eprintln!(
                "Lean work started but was interrupted or degraded by a resource guard: cause={}",
                facts.cause
            );
        } else {
            eprintln!(
                "resource admission refused before Lean work entered the child: cause={}",
                facts.cause
            );
        }
    }
    err
}
```

Treat `work_entered_child=false` as a hard admission refusal. Retry only after changing the budget, choosing an
equivalent warm session key, or cycling according to policy. Treat `work_entered_child=true` as interrupted or degraded
work; module-query batches keep per-selector failures such as budget exhaustion in the returned batch facts.

Same-process `SessionPool` refusals use `LeanDiagnosticCode::ResourceExhausted` and attach `ResourceExhaustedFacts` to
the host failure. The same rule applies: do not convert resource exhaustion into `not_found`, an empty result, or a
generic protocol error.

## Diagnostic Same-Process Hosts

Same-process host sessions remain useful for embedded tools, examples, and focused diagnostics. They are not the default
pattern for unbounded long-running import-heavy services.

Use `SessionPool` with `SessionPoolMemoryPolicy` when the host must stay in-process, and configure a fresh-import and
RSS budget before cache misses can call `Lean.importModules`. `SessionPoolMemoryPolicy::disabled()` and
`LeanWorkerRestartPolicy::disabled()` are for short-lived tests, benchmarks, or hosts that enforce an external process
memory boundary.

Bracketed lightweight queries are the only in-process path that can call `Environment.freeRegions`, and only because
they import with `loadExts := false` and return fully Rust-owned serialized data before the bracket exits. They are not
a replacement for normal full sessions.

## Profiling Commands

Validate policy changes with the existing bounded workloads:

```sh
LEAN_RS_WORKER_MEMORY_IMPORTS=6 \
LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=1 \
LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB=1572864 \
./profiling/scripts/profile_memory.sh worker-cycling

LEAN_RS_POOL_MEMORY_MAX_WORKERS=1 \
LEAN_RS_POOL_MEMORY_TOTAL_RSS_KIB=1572864 \
LEAN_RS_POOL_MEMORY_PER_WORKER_RSS_KIB=1572864 \
LEAN_RS_POOL_MEMORY_MAX_IMPORTS=1 \
./profiling/scripts/profile_memory.sh pool-memory
```

The worker and pool reports include `import_stats=...`, `admission=...`, `session_reuse=...`, `replacement=...`, and
`batch=...` rows. Those rows explain, separately, what was imported, whether cold work was admitted, whether a warm key
was reused, how much synchronous replacement cost, and whether repeated module-query work stayed on one warm lease.

The same-process long-session workload is diagnostic:

```sh
LEAN_RS_LONG_SESSION_MAX_RSS_KIB=1572864 \
./profiling/scripts/profile_memory.sh long-session
```

Run it to understand retention and attribution, not as a production soak test.
