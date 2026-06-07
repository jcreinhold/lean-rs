# Worker Pool

A single worker is the right unit for panic containment and process-global memory reset, but it is not enough for
mathlib-scale or multi-package workloads. Those workloads need parallelism, warm session reuse, memory admission, and
failure isolation across more than one child process.

The pool boundary keeps that operational machinery inside the worker crates. Downstream callers submit capability work
keyed by session requirements. The pool decides whether to reuse a warm worker, start a new child, cycle a stale child,
or delay work until policy permits it.

## Chosen Boundary

`LeanWorkerPool` is the local multi-worker orchestration boundary. It owns:

- worker child lifecycle;
- work queueing and admission;
- session leases and session-key matching;
- worker restart and session invalidation sequencing;
- memory-aware scheduling and RSS sampling policy;
- failure isolation between child processes;
- cancellation and timeout propagation into leased work;
- pool-level observability.

The pool does not replace `LeanWorkerCapabilityBuilder` or typed commands. It sits above them: the builder describes how
to open one capability-backed worker session, while the pool decides which local child should host that session for one
piece of work.

The public surface is a lease-first API:

- `LeanWorkerPool` owns a bounded set of local capability workers;
- `LeanWorkerPoolConfig` exposes a fixed `max_workers` limit;
- `LeanWorkerSessionKey` records the worker reuse facts;
- `LeanWorkerSessionLease` runs typed JSON and streaming commands without exposing `LeanWorkerSession` as the primary
  pool API.

A lease becomes invalid after timeout, cancellation, child fatal exit, explicit cycle, or capability metadata mismatch.
The caller acquires a fresh lease for follow-up work. That rule keeps session invalidation explicit without making
callers learn which child process or warm worker was selected.

The same pool boundary carries local memory-aware scheduling:

- `max_total_child_rss_kib` rejects a new distinct worker when known total child RSS already reaches the configured
  budget;
- `per_worker_rss_ceiling_kib` cycles a warm worker before assigning more work when its sampled RSS reaches the
  configured ceiling;
- `idle_cycle_after` cycles an idle worker before stale leased work continues;
- `queue_wait_timeout` bounds synchronous admission waits for a full pool.

RSS sampling is best effort and platform-specific. Unsupported samples are recorded as unavailable; the pool does not
claim a budget decision from missing RSS data. Memory-driven cycles are reported as policy restarts and invalidate stale
leases before downstream command execution.

Pool-level observability and bounded row-delivery backpressure also live at this boundary:

- `LeanWorkerPoolSnapshot` summarizes worker counts, warm leases, queue depth, restart reasons, child RSS samples,
  stream request outcomes, delivered row counts, payload bytes, stream elapsed time, and backpressure counters;
- `LeanWorkerSessionLease::snapshot` samples the leased worker without exposing child identity;
- row delivery uses a bounded internal event buffer, so a slow sink blocks the request path instead of growing memory
  without bound;
- rows are never dropped for committed streams, and delivered rows remain tentative until terminal success.

Snapshots are operational summaries, not protocol traces. They do not expose worker ids, child pids, pipe handles,
protocol frames, or which warm worker was selected.

## Designs Considered

**Single worker only.** Rejected as the scale foundation. It preserves a clean process boundary, but it serializes
independent module groups. A downstream tool that wants to use available cores would have to build its own worker
fanout, queueing, and restart policy.

**Caller-managed worker fanout.** Rejected. It would push child counts, restart timing, session reuse, memory ceilings,
lease invalidation, and failure classification into every downstream tool. That duplicates the same production rules
that the worker crates already owns for one worker.

**`LeanWorkerPool`.** Chosen. The pool is a deeper module because it hides orchestration decisions that every local
production consumer would otherwise reimplement. The public surface should describe capability work and policy intent,
not child process mechanics.

Remote workers are out of the current arc. The local pool should not expose APIs that assume a worker is identified by a
child pid or a local pipe, because that would make a future remote backend harder. The first supported backend is local
child processes.

## Session Keys

Pool reuse is keyed by the facts that make a worker session safe and useful to reuse:

- capability Lake project root;
- import workspace root;
- capability package and target;
- import set;
- capability metadata expectations;
- Lean toolchain and `lean-rs` protocol fingerprint;
- restart-policy class.

The key is not a downstream cache key. Downstream crates still decide whether a row, index, probe, or report is
semantically valid. The pool key only answers a worker question: can this already-open child session run the next
compatible request without repeating setup or violating policy?

The import workspace root is key material because the current pool model holds one open session per worker entry, and a
Lean session's search path is fixed when it opens. The implemented design is one worker entry per compatible
`(capability, import workspace)` pair. That keeps same-workspace audits warm and prevents different workspaces from
aliasing one session.

The key records capability metadata expectations as opaque generic metadata facts. The worker crates can compare the
expected and actual metadata envelopes, but it does not interpret downstream command versions or decide cache
invalidation.

A future pool design could key workers only by capability identity and reopen sessions against different target
workspaces on the same loaded capability child. That would be a deeper change to the pool invariant and lifecycle model.
It should be built only if a named `lean-dup` audit-many-workspaces workload measures worker churn or dylib reload as a
bottleneck; until then, the current keyed-session model is the supported behavior.

## What The Pool Hides

Callers should not learn:

- worker ids or child pids;
- stdin/stdout pipes or frame order;
- child spawn and handshake sequencing;
- queue internals;
- stderr parsing or fatal-exit classification;
- restart timing;
- RSS sampling details;
- which session values are invalidated by a cycle;
- which warm worker is selected for a request.

Those details are volatile and operational. Keeping them private lets the worker crates change scheduling, memory
policy, batching, and future backend internals without rewriting downstream tools.

## What Callers Still Know

Callers still own the facts that are part of their domain:

- capability identity;
- typed command request, row, and summary schemas;
- request values;
- timeout and cancellation intent;
- row commit policy after terminal success;
- row semantics, cache validity, ranking, reporting, and source provenance.

This is the same boundary as the worker capability layer. The worker crates own process and transport behavior;
downstream crates own semantic commands and schemas. A `lean-dup` integration would map its own commands onto the pool,
but the worker crates should not grow first-party `extract`, `features`, `index`, or `probe` methods.

## Failure And Memory Model

A child crash, request timeout, cancellation-triggered cycle, explicit cycle, or policy restart invalidates the sessions
hosted by that child. The pool must report those outcomes distinctly so callers can distinguish downstream command
failure from infrastructure failure.

Only process exit resets Lean process-global runtime and import state. `SessionPool::drain()` can release Rust-owned
cached environments inside a child, but it is not an RSS reset. Pool memory policy therefore operates by admitting work,
selecting warm sessions carefully, and cycling child processes when measured policy says a reset is needed.

The pool memory policy is local-child policy, not a remote placement model. Callers may configure worker count and RSS
budgets, but they do not poll child RSS, inspect child pids, or decide which child to cycle.

## Related docs

- [`21-import-set-planning.md`](21-import-set-planning.md)—stable work batches that make pool reuse effective.
- [`22-worker-row-batching.md`](22-worker-row-batching.md),
  [`23-worker-data-plane-format.md`](23-worker-data-plane-format.md)—batching and data-plane format decisions.
- [`24-lean-side-worker-streaming.md`](24-lean-side-worker-streaming.md)—Lean-side streaming helpers.
- [`25-mathlib-scale-worker-fixture.md`](25-mathlib-scale-worker-fixture.md)—mathlib-scale fixture.
- [`26-worker-pool-observability.md`](26-worker-pool-observability.md)—pool observability.
- [`27-lean-dup-readiness.md`](27-lean-dup-readiness.md)—downstream worker replacement fixture.
- [`28-production-scale-release.md`](28-production-scale-release.md)—production-scale contract.
