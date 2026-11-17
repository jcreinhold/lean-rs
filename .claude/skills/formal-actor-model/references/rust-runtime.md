# Rust Runtime

Keep state, receivers, lifecycle flags, and receive loops private. Expose concrete handles and typed outcomes. Choose an
owning task, thread, or process that fits the existing runtime; an actor boundary does not require Tokio.

In lean-rs, inspect `crates/lean-rs-worker-parent/src/supervisor.rs` and `pool.rs` before proposing a new scheduler.
Their reader threads and bounded event channels are implementation details of the worker contract.

## Message API

A typed message enum can carry a reply channel. For example, in a Tokio-based design (schematic types):

```rust
enum WorkerMsg {
    DoWork {
        input: Work,
        reply: tokio::sync::oneshot::Sender<Result<WorkDone, WorkerError>>,
    },
    Stop {
        reason: StopReason,
    },
}
```

Expose domain operations on a concrete handle. Request/reply operations await a typed result; fire-and-forget operations
still report whether admission succeeded. Do not expose the sender unless callers are meant to construct the protocol.
Add erased handles, registries, or generic traits only when actual callers need them.

## Mailboxes and backpressure

- Use bounded queues by default. Justify unbounded storage with a memory bound and overload policy.
- Define whether full queues wait, reject, drop, or route to dead letters.
- Expose capacity as a parameter only when callers need to choose it.
- Measure queue depth and admission wait time; admission is not proof of processing.

## Failure and cancellation

Represent failures through typed outcomes: stopped, unavailable, cancelled, panic, worker exit, timeout, or restart
exhaustion, as appropriate to the boundary.

- Specify whether dropping the last handle stops the owner.
- Return an error for a closed mailbox.
- Catch Rust panics at the owner boundary when the contract promises reporting or restart; this does not recover a
  process from a Lean fatal exit.
- Give cancellation a bounded shutdown path. Use a forced-stop fallback only when supported by the owner.

Use [supervision and failure](supervision-and-failure.md) for restart strategies, limits, retries, and shutdown order.
The supervisor owns recovery; each actor should not restart itself.

## Thread safety

State belongs to its owner. A handle may be `Send` or `Sync` only when its contents and operations make that sound.
Audit raw pointers and every unsafe thread-safety implementation. For foreign callbacks, enqueue only data permitted by
the thread contract; Lean-derived handles remain thread-bound. See [Lean/Rust FFI](lean-rust-ffi.md).

## Observability

Expose logical identity, lifecycle, restart generation, overload counters, and bounded error summaries. Structured
runtime events can support tests and model observations. Task IDs, channel types, OS thread IDs, and pointer addresses
may aid debugging but are not stable actor semantics.
