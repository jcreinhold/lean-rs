# Worker Process Boundary

Use `lean-rs-host` for in-process theorem-prover work. Use the worker crates when the application must survive a Lean
fatal exit, enforce a request deadline, or reset process-global memory. The worker child runs `lean-rs-host`; the parent
owns supervision, framed IPC, cancellation, timeouts, row streaming, restart policy, and diagnostics.

The child is a supervised serial process. The local pool provides synchronous leases with bounded admission waits.
The runtime contract is in [`30-worker-runtime-semantics.md`](30-worker-runtime-semantics.md).

## Failure containment

Lean internal panics, generated `unreachable`, `std::exit`, `abort`, and foreign unwinds may terminate the process rather
than return an error. An in-process session cannot promise recovery after those failures. The worker parent observes
the child's exit and returns a typed failure while the application survives. Inside the child, the
[in-process panic contract](06-panic-containment.md) still applies.

Rust callback panics remain a `lean-rs` responsibility: its callback registry catches them before the C boundary. The
worker transports process-safe values; callback handles and runtime-bound Lean objects stay in the child.

## Memory reset

Dropping sessions or calling `SessionPool::drain()` releases Rust-owned environment references. Lean may retain memory
in imported modules, compacted regions, interned names, allocator state, and other process-global structures. Only
process exit resets that state; see the [recorded memory investigation](../safety/long-session-memory.md).

Worker policies can cycle a child on request count, import-like request count, idle time, sampled RSS, explicit request,
or in-flight cancellation. RSS sampling is best effort. Cleanup must confirm the owned child has exited before a
replacement can be treated as usable; an unconfirmed replacement retains its ownership and capacity fence.

## Layer ownership

| Layer | Responsibility |
| --- | --- |
| `lean-rs` | Runtime-bound handles, exported calls, ABI conversion, sealed same-process callbacks |
| `lean-rs-host` | Imports, elaboration, kernel checks, bounded meta operations, declaration and source queries |
| Worker crates | Process lifecycle, framing, watchdogs, copied results, streaming, memory cycling |
| Downstream application | Command semantics, schemas, persistence, cache validity, reporting |

Supervision belongs above `lean-rs-host`: adding pipes, child cleanup, or restart bookkeeping to `LeanSession` would
burden every in-process caller. It also belongs in this library rather than in each downstream application, because
those applications need the same exit, timeout, restart, and memory-reset rules.

See the [session adapter](17-worker-session-adapter.md), [capability layer](19-worker-capability-layer.md),
[local pool](20-worker-pool.md), and [worker recipe](../recipes/worker-capability-runner.md#wire-and-raw-rows).
