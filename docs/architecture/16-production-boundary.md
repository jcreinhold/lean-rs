# Production Boundary

`lean-rs-host` is the fast in-process theorem-prover API. It is the right surface for trusted workloads where the caller
accepts Lean's process-scoped failure and memory contracts. It is not the containment boundary for production systems
that must survive Lean internal panics, aborts, foreign unwinds, or long-running import sweeps that retain
process-global memory.

The production boundary is a worker process. The worker crates own child processes, framed IPC, restart policy,
fatal-exit diagnostics, request timeouts, live row streaming, typed command facades, and memory cycling. `lean-rs-host`
remains the in-process API that the worker uses inside the child.

## Chosen Boundary

The crate split is:

- `lean-rs` owns typed Lean object handles, exported calls, and sealed callback payloads.
- `lean-rs-host` owns in-process sessions, imports, elaboration, kernel checks, declaration introspection, `MetaM`,
  pooling, cancellation, and progress.
- The worker crates own process supervision, worker protocol framing, restart policy, lifecycle, request watchdogs, row
  streaming, typed commands, memory cycling, and fatal-exit reporting.

This boundary keeps each layer at a different abstraction. `lean-rs-host` answers theorem-prover questions in one
process. The worker crates answer an operational question: how to run that host stack when the caller needs process
isolation or a hard memory reset.

## Why Process Isolation Is Required

Lean internal failures do not all return through an error channel. A kernel assertion, generated `unreachable`,
`LEAN_ABORT_ON_PANIC` path, `std::exit`, `abort`, or foreign unwind may terminate the process or cross a C ABI boundary
that Rust cannot recover from soundly. `LeanSession` therefore cannot promise a poisoned-but-droppable session after
those failures. The safe containment unit is the child process: the parent observes worker exit and decides whether to
restart.

The worker boundary does not change the in-process panic contract. Inside the child, the same rules from
[`06-panic-containment.md`](06-panic-containment.md) apply. The difference is that the parent process survives and
receives a typed worker-failure report instead of losing the application process.

## Why Memory Cycling Is Required

Long-running processes can retain Lean runtime memory even after Rust-owned session and pool values are dropped. Reused
imported environments are stable, but workloads that sweep fresh import sets can leave process-global residue in module
initialization state, imported environment data, compacted regions, interned names, allocator state, and other Lean
runtime structures.

`SessionPool::drain()` releases cached Rust-owned environment references. It is useful at idle boundaries, but it is not
an RSS reset. Only exiting the worker process resets Lean's process-global state. The worker crates therefore provide
process-cycling triggers for explicit cycles, request count, import-like request count, idle restart, measured RSS
ceiling, and in-flight worker-session cancellation. The measurement baseline remains
[`../safety/long-session-memory.md`](../safety/long-session-memory.md).

## Host Operations Across The Boundary

The worker session adapter is a narrow, process-safe subset of `LeanSession`. It returns copied diagnostics, kernel
statuses, and rendered declaration strings; it does not send runtime-bound `LeanExpr`, `LeanEvidence`,
`LeanDeclaration`, or `LeanName` handles to the parent. See
[`17-worker-session-adapter.md`](17-worker-session-adapter.md).

## Rejected Boundaries

**Embedding worker support in `lean-rs-host`.** Rejected. Process supervision, framed IPC, restart bookkeeping, crash
diagnostics, and memory cycling are not theorem-prover session operations. Putting them on `LeanSession` or
`LeanCapabilities` would make the in-process host layer shallow: every caller would see operational machinery even when
they only need fast trusted calls.

**Downstream-only orchestration.** Rejected. Leaving process supervision to each consumer would make every production
embedder rediscover the same rules for panic containment, child cleanup, request framing, crash classification, restart
cadence, and memory cycling. Those rules are part of the binding stack's operational contract and should be encoded
once.

## Callback Payloads Stay Below The Worker

Callback payloads remain an L1 `lean-rs` concern. `LeanCallbackPayload` is sealed, and supported payloads are added only
after the ABI, ownership, wrong-payload, stale-handle, reentrancy, and panic-boundary rules are proven. The worker
should use those payloads; it should not create an arbitrary cross-process callback type system.

Byte streaming and Lean-object events are future callback payload work, not worker-protocol shortcuts. The worker may
carry serialized effects over IPC, but the same deep-module rule applies: payload decoding and Lean object soundness
belong below the process supervisor.

## Consumer Guidance

Compose at the highest layer that matches the operational requirement. Direct `lean-rs` callbacks are for trusted
same-process ABI work. `lean-rs-host` is for trusted in-process theorem-prover sessions. The worker crates are for
worker-style tools where process lifecycle, IPC, fatal exits, request timeouts, row streaming, and memory reset should
be owned by the library rather than every downstream caller.

Use `lean-rs-host` directly when the process can trust the Lean workload, when latency is the primary concern, and when
process-level memory retention is acceptable.

Use the worker boundary when the application must continue after Lean exits, must classify fatal child exits, or must
reset memory after large import sweeps. The worker is not a replacement for `lean-rs-host`; it is a process boundary
around it. See [`../recipes/worker-process-boundary.md`](../recipes/worker-process-boundary.md) for the runnable
worker-streaming example.
