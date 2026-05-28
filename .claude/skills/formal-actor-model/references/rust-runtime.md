# Rust Runtime

Use Rust to implement the actor runtime as a deep boundary. Callers should see handles and typed outcomes, not channels,
locks, join handles, or scheduler internals.

## Runtime Structure

Recommended default:

- `ActorHandle<M>`: cloneable sending capability.
- `ActorRef`: erased or enum-based handle when heterogeneous actors are needed.
- Private `ActorCell`: owns state, receiver, lifecycle flags, metrics, and supervisor link.
- Private receive loop: processes one message at a time.
- `Supervisor`: owns start/stop/restart policy and observes actor exits.
- `Registry`: optional mapping from logical actor IDs to handles; use only when lookup is a real requirement.

## Message API

Prefer a typed enum for each actor boundary:

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

Use `call` for request/reply and `send` for fire-and-forget:

```rust
impl WorkerHandle {
    pub async fn call(&self, input: Work) -> Result<WorkDone, ActorError>;
    pub async fn send_stop(&self, reason: StopReason) -> Result<(), ActorError>;
}
```

Do not expose `mpsc::Sender<WorkerMsg>` unless the caller is meant to participate in protocol construction.

## Mailboxes And Backpressure

- Use bounded `mpsc` by default.
- Make capacity a constructor parameter only if real callers need different values.
- Define overload behavior: await capacity, fail fast, drop oldest, drop newest, or route to dead letters.
- Instrument queue depth and send wait time.
- Avoid unbounded queues for convenience. They turn overload into memory growth and hide backpressure from callers.

## Failure And Cancellation

Represent actor outcomes explicitly:

```rust
enum ActorExit {
    Normal,
    Stopped(StopReason),
    Failed(ActorFailure),
    Cancelled,
    RestartLimitExceeded,
}
```

Rules:

- Dropping a handle should not silently abort an actor unless that is the declared lifecycle policy.
- A closed mailbox is a structured `ActorError::Stopped` or `ActorError::Unavailable`, not a panic.
- Panics should be caught at the task boundary if the runtime promises restart/reporting.
- Cancellation should have a bounded shutdown path and a forced-abort fallback if the runtime supports it.

## Supervision

Supervisor responsibilities:

- Start children in dependency order.
- Observe normal and abnormal exits.
- Apply restart strategy and intensity window.
- Escalate when restart limits are exceeded.
- Publish health/freshness after restart so callers know cached state may be stale.

Do not make each actor manually restart itself. Self-restart tangles lifecycle with business logic.

## Thread Safety

- Actor state should be owned by the actor task, not shared through `Arc<Mutex<State>>`.
- Handles must be `Send + Sync` only if their internals make that sound.
- Raw pointers are not `Send`/`Sync` by default; wrap them only with a documented safety proof.
- If a foreign callback enters Rust from another thread, immediately enqueue into the owning actor/runtime.

## Observability

Expose:

- actor ID/logical name;
- lifecycle status;
- restart generation;
- queue depth or overload counters;
- last error summary;
- structured event log for tests.

Do not expose task IDs, OS thread IDs, channel internals, or raw pointer addresses as stable semantics.
