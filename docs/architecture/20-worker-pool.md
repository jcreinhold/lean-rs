# Worker Pool

`LeanWorkerPool` retains a bounded set of local capability workers and returns a `LeanWorkerSessionLease` for compatible
work. The builder describes one capability session; the pool owns warm reuse, cold admission, replacement, memory
policy, lease invalidation, and aggregate observations.

The API is synchronous: acquiring a lease mutably borrows the pool, and the lease borrows its selected entry. It has
no public mailbox, reserved queue slot, scheduler, fairness guarantee, or global FIFO order. Remote workers are outside
the supported backend. See [runtime semantics](30-worker-runtime-semantics.md) for the lifecycle model.

## Leases and reuse

A lease runs typed JSON and streaming commands without exposing child selection or pipes. Explicit `release` and drop
share an at-most-once capacity return path. Timeout, cancellation, fatal exit, explicit cycle, metadata mismatch, or a
policy replacement invalidates the lease. A lease also records its admitted worker generation; acquire a fresh lease
before follow-up work after invalidation.

`LeanWorkerSessionKey` contains:

- normalized capability project and import workspace roots;
- manifest path when configured, package, and library target;
- ordered imports and import profile;
- capability metadata and host-runtime expectations;
- build-baked `ToolchainFingerprint`;
- the coarse default/custom restart-policy class.

The key answers whether an open worker session can be reused. It is not a downstream result-cache key and does not
encode command semantics, row schemas, or every restart-policy knob. Protocol compatibility is checked by worker
handshake, not by a separate protocol-fingerprint field in this key.

One pool entry holds one open session with one fixed import search path. Different `(capability, import workspace)`
pairs therefore use different entries. A capability-only pool that reopens sessions across workspaces would change
this invariant and needs measured evidence of workspace churn before adoption.

## Admission and memory policy

`LeanWorkerPoolConfig::new(max_workers)` clamps the worker limit to at least one. Additional controls are:

| Setting | Behavior |
| --- | --- |
| `max_total_child_rss_kib` | Refuse a new distinct worker when known total child RSS has reached the budget |
| `per_worker_rss_ceiling_kib` | Cycle a worker before assigning more work when its sampled RSS reaches the ceiling |
| `idle_cycle_after` | Cycle an idle worker before stale leased work continues |
| `queue_wait_timeout` | Bound synchronous admission waiting when the pool is full; default is zero |

Coordinate these settings with each worker's restart policy. RSS sampling is best effort: unavailable samples are
reported as unavailable, not used to assert a memory budget. Admission compares observations; it does not reserve a
predicted future RSS allocation.

Only process exit resets Lean's process-global state. `SessionPool::drain()` releases cached in-process environments
but is not an RSS reset. A replacement whose exit cannot be confirmed retains cleanup ownership and prevents unsafe
reuse; it is not silently treated as an empty capacity slot.

## Observability and backpressure

`LeanWorkerPoolSnapshot` aggregates worker counts, warm leases, requests/imports, admission and reuse results, restart
reasons, best-effort child RSS, startup/import/command timing, and streaming outcomes. Stream fields include delivered
rows, raw payload bytes, elapsed time, and backpressure counters. `LeanWorkerSessionLease::snapshot` samples its entry
using the same shape while the pool is borrowed.

The snapshot's `queue_depth` is always `0`: the current pool has no mailbox queue. Snapshots do not expose worker ids,
child pids, pipes, frame order, or the selected warm worker. Applications adapt snapshots to their own logs or metrics
backend. The private `RuntimeTraceEvent` in `tests/worker_shutdown.rs` is a conformance-test vocabulary, not a public
event bus.

The bounded internal reader buffer blocks when the request owner or sink is slow, propagating pressure through the
child pipe. Rows are never dropped from committed streams, but delivered rows remain tentative until terminal success.
Timeout or cancellation can terminate a blocked request through the normal lifecycle path.

- `backpressure_waits` counts reader waits on the bounded buffer.
- `backpressure_failures` counts failed streams that had observed backpressure.
- `data_rows_delivered` counts successful sink deliveries; terminal summaries define committed counts.

Diagnostics, progress, data rows, and snapshots remain separate channels. A slow-sink probe prints parent/child RSS,
rows, bytes, and waits; see the [fixture commands](25-mathlib-scale-worker-fixture.md#commands).

## Validation evidence

The supported workload path is:

```text
LeanWorkerImportPlanner -> LeanWorkerPool -> LeanWorkerSessionLease
  -> typed command -> live rows -> terminal summary -> snapshot
```

The repository tests cover keyed reuse, affine release, death, timeout, cancellation, metadata mismatch, explicit and
policy cycling, memory admission, typed commands, and bounded backpressure. Relevant worker-child suites are `pool`,
`session_reuse`, `import_planning`, `observability_backpressure`, `mathlib_scale_fixture`, and `lean_dup_readiness`.
The parent `worker_shutdown` suite checks import-light lifecycle conformance.

The [scale and replacement fixtures](25-mathlib-scale-worker-fixture.md) use deterministic test schemas, including when
planning with a real mathlib module list. They establish operational coverage, not mathlib indexing semantics or a
portable throughput guarantee. [Streaming measurements](18-worker-data-streaming.md) justify the retained per-row,
raw-JSON path.

Performance reports must name the command, revision, platform, row/module counts, throughput, RSS availability, and
whether the workload used the deterministic fallback or `LEAN_RS_MATHLIB_ROOT`; use the probes in
[performance guidance](../performance.md). Downstream schemas, cache validity, ranking, reporting, and provenance stay
with the application.
