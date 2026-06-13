# Production-Scale Worker Release Contract

The release claim for the worker crates as a local worker-pool foundation for mathlib-scale worker-class workloads, and
the boundaries that must stay true.

## Release Claim

`LeanWorkerPool` is the supported local orchestration boundary for mathlib-scale worker-class workloads. The normal path
is:

```text
LeanWorkerImportPlanner -> LeanWorkerPool -> LeanWorkerSessionLease
  -> typed command -> live rows -> terminal summary -> pool stats
```

The pool owns local admission, session leases, memory-aware assignment, restart policy, backpressure, observability, and
failure isolation. Callers do not choose child pids, worker ids, pipes, protocol frames, restart sequencing,
row-buffering mechanics, or a queue position.

The pool sits above `LeanWorkerCapabilityBuilder`. The builder knows how to open one capability-backed worker session.
The pool decides when compatible work can reuse a warm session, when a new local child may be spawned, and when policy
requires a cycle before more work is admitted.

## What Callers Still Own

The worker crates do not own downstream semantics. Downstream crates still define:

- command names and exported Lean functions;
- request, row, and terminal-summary serde types;
- row schemas and schema versions;
- cache validity and persistence;
- ranking, reporting, and source provenance;
- user-facing CLI or service policy.

This is why the downstream worker fixture uses command-like names but does not define downstream rows or cache policy.

## Data Plane

Worker throughput is handled through worker IPC, raw-JSON typed decoding, bounded row delivery, and measured
batching/data-plane decisions. It is not handled through cross-process callback handles.

The worker keeps per-row frames and the raw-JSON typed decode path. The measured broader worker workloads did not
justify public row batch sinks, binary row APIs, MessagePack, CBOR, Postcard, or a public protocol frame surface.
`LeanWorkerDataRow` is the schema-less escape hatch; `LeanWorkerStreamingCommand` is the normal downstream streaming
surface.

## Callback Payload Decision

`lean-rs` callbacks are same-process interop mechanisms. They are useful for trusted extensions that intentionally run
in the same process as Lean. They are not the scale path for worker-class tools.

Supported callback payloads are the sealed payloads already implemented: `LeanProgressTick` and `LeanStringEvent`. Byte
callbacks are not exposed until a concrete same-process binary callback caller appears. Object callbacks are not exposed
until a soundness proof produces a scoped API. Neither byte nor object callbacks are needed for worker row ergonomics.

## Evidence

Named workloads back the scale claim:

- pool API and lease tests cover reuse, death, cancellation, timeout, metadata mismatch, memory-policy invalidation, and
  typed command execution;
- memory scheduling workloads record fixture import reuse, a deterministic mathlib-style fallback import set, repeated
  cycle/reuse, parent RSS, and child RSS or explicit RSS-unavailable status;
- row payload benches compare JSON tree rows, raw JSON, simulated batch frames, simulated binary envelopes, MessagePack,
  and CBOR;
- Lean-side helper tests cover streaming envelopes, chunked streams, diagnostics, progress, terminal metadata, and error
  conversion;
- the mathlib-scale fixture exercises planner, pool, lease, typed command, diagnostics, terminal summaries,
  cancellation, fatal-exit recovery, cycling, row throughput, and RSS sampling;
- pool observability tests cover snapshots and bounded backpressure;
- the downstream worker fixture exercises generic `version`, `doctor`, `extract`, `features`, `index`, and `probe`
  commands without importing downstream schemas.

Numbers are machine-local. Any performance claim must name the workload, command, platform, row counts, throughput, RSS
status, and caveats.

## Non-Goals

Remote workers are future work. The local pool should avoid public APIs that would make a remote backend impossible, but
this release supports only local child processes.

The worker crates do not implement `lean-dup`, define downstream row schemas, add worker pools across machines, or
expose new callback payload types.
