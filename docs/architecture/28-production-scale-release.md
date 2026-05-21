# Production-Scale Worker Release Contract

The release claim for `lean-rs-worker` as a local worker-pool foundation for
mathlib-scale worker-class workloads, and the boundaries that must stay true.

## Release Claim

`LeanWorkerPool` is the supported local orchestration boundary for
mathlib-scale worker-class workloads. The normal path is:

```text
LeanWorkerImportPlanner -> LeanWorkerPool -> LeanWorkerSessionLease
  -> typed command -> live rows -> terminal summary -> pool stats
```

The pool owns local scheduling, session leases, memory-aware admission,
restart policy, backpressure, observability, and failure isolation. Callers do
not choose child pids, worker ids, pipes, protocol frames, restart sequencing,
or row-buffering mechanics.

The pool sits above `LeanWorkerCapabilityBuilder`. The builder knows how to
open one capability-backed worker session. The pool decides when compatible
work can reuse a warm session, when a new local child may be spawned, and when
policy requires a cycle before more work is admitted.

## What Callers Still Own

`lean-rs-worker` does not own downstream semantics. Downstream crates still
define:

- command names and exported Lean functions;
- request, row, and terminal-summary serde types;
- row schemas and schema versions;
- cache validity and persistence;
- ranking, reporting, and source provenance;
- user-facing CLI or service policy.

This is why the `lean-dup` readiness proof uses command-like fixture names but
does not define `lean-dup` rows or cache policy.

## Data Plane

Worker throughput is handled through worker IPC, raw-JSON typed decoding,
bounded row delivery, and measured batching/data-plane decisions. It is not
handled through cross-process callback handles.

The worker keeps per-row frames and the raw-JSON typed decode path. The
measured broader worker workloads did not justify public row batch sinks,
binary row APIs, MessagePack, CBOR, Postcard, or a public protocol frame
surface. `LeanWorkerDataRow` is the schema-less escape hatch;
`LeanWorkerStreamingCommand` is the normal downstream streaming surface.

## Callback Payload Decision

L1 callbacks are same-process interop mechanisms in `lean-rs`. They are useful
for trusted extensions that intentionally run in the same process as Lean. They
are not the scale path for worker-class tools.

Supported callback payloads are the sealed payloads already implemented:
`LeanProgressTick` and `LeanStringEvent`. Byte callbacks are not exposed
until a concrete same-process binary callback caller appears. Object
callbacks are not exposed until a soundness proof produces a scoped API.
Neither byte nor object callbacks are needed for worker row ergonomics.

## Evidence

The scale claim is backed by named workloads, not by intent:

- pool API and lease tests cover reuse, death, cancellation, timeout, metadata
  mismatch, memory-policy invalidation, and typed command execution;
- memory scheduling workloads record fixture import reuse, mathlib-shaped
  fallback imports, repeated cycle/reuse, parent RSS, and child RSS or explicit
  RSS-unavailable status;
- row payload benches compare JSON tree rows, raw JSON, simulated batch frames,
  simulated binary envelopes, MessagePack, and CBOR;
- Lean-side helper tests cover streaming envelopes, chunked streams,
  diagnostics, progress, terminal metadata, and error conversion;
- the mathlib-scale fixture exercises planner, pool, lease, typed command,
  diagnostics, terminal summaries, cancellation, fatal-exit recovery, cycling,
  row throughput, and RSS sampling;
- pool observability tests cover snapshots and bounded backpressure;
- the readiness proof exercises generic `version`, `doctor`, `extract`,
  `features`, `index`, and `probe` command shapes without importing downstream
  schemas.

Numbers are machine-local. Any performance claim must name the workload,
command, platform, row counts, throughput, RSS status, and caveats.

## Non-Goals

Remote workers are future work. The local pool should avoid public APIs that
would make a remote backend impossible, but this release supports only local
child processes.

`lean-rs-worker` does not implement `lean-dup`, define downstream row
schemas, add worker pools across machines, or expose new callback payload
types.
