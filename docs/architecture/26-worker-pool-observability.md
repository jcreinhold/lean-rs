# Worker Pool Observability

The operating view for large local worker runs. The pool reports cheap snapshots and applies bounded row-delivery
backpressure without exposing worker internals.

## Boundary

`LeanWorkerPoolSnapshot` is the public summary type. It reports aggregate pool state:

- configured and active worker counts;
- warm lease count and queue-depth field;
- request/import counters;
- restart reasons;
- best-effort child RSS samples;
- streaming request success/failure counters;
- delivered row count and raw payload bytes;
- accumulated stream elapsed time;
- backpressure wait and failure counters.

A snapshot intentionally does not expose child pids, local worker ids, pipe handles, protocol frames, child stderr, or
the selected warm worker. Those are supervisor and pool mechanics. Callers can sample the snapshot during a large run,
log it, or use it to choose their own cancellation policy without learning how the child process is wired.

The current pool does not implement a mailbox queue. `queue_depth` is always `0`; it remains in the snapshot as a stable
operational field. `queue_wait_timeout` measures bounded synchronous admission waiting for a full pool, not time spent
in a reserved queue slot. See [`30-worker-runtime-semantics.md`](30-worker-runtime-semantics.md) for the full worker
runtime contract.

`LeanWorkerSessionLease::snapshot` provides the same aggregate shape for the leased worker while the lease is active. It
is a sampling hook, not an identity handle. A stale lease remains stale after timeout, cancellation, crash, explicit
cycle, memory policy restart, or metadata mismatch.

## Backpressure

Worker row delivery uses a bounded internal event buffer between the supervisor reader thread and the request owner.
When the request owner or row sink is slow, the reader blocks after the internal capacity is reached. That blocking
propagates naturally to the child through the pipe instead of allowing unbounded parent memory growth.

Rows are never dropped for committed streams. A row delivered to the sink is still tentative until the terminal success
response arrives. Sustained backpressure can be stopped by request timeout or cooperative cancellation; the request then
returns a typed worker error and the affected session is invalidated according to the normal worker lifecycle rules.

Backpressure counters are observability, not an event bus:

- `backpressure_waits` counts reader-side waits on the bounded event buffer;
- `backpressure_failures` counts failed streaming requests that had already observed backpressure;
- `data_rows_delivered` counts rows successfully handed to the data sink;
- terminal summaries remain the source of committed row counts.

Diagnostics, progress events, data rows, and pool stats stay separate channels. Progress is control/observability; it is
not a row stream. Diagnostics describe capability or worker issues; they are not downstream data.

## RSS And Throughput

RSS sampling remains best effort. Unsupported platforms record unavailable samples rather than pretending that the pool
can enforce a budget from missing data. A memory reset still means cycling the child process. In-process host cache
draining does not reset Lean process-global RSS.

The mathlib-scale probe prints pool snapshot fields during normal typed-command execution and a slow-sink workload:

```sh
cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
cargo run -p lean-rs-worker-child --example mathlib_scale_probe
```

The `slow_sink` line records parent RSS before/after, child RSS, delivered rows, payload bytes, and backpressure waits.
Use those numbers as workload evidence; do not describe memory or throughput behavior as bounded without a recorded run.

## Rejected Designs

**Public worker identity metrics.** Rejected. Child pids and worker ids make local process details part of the API and
would complicate a future remote or alternate backend.

**Unbounded row queues.** Rejected. They make slow sinks a parent-memory leak under mathlib-scale streams.

**Dropped rows.** Rejected. Worker streams have commit-after-success semantics; dropping rows would make terminal
success ambiguous for downstream schemas.

**Metrics framework integration.** Rejected for this layer. The pool exposes a cheap snapshot. Applications adapt it to
logs, tracing, Prometheus, or another metrics backend outside the worker crates.
