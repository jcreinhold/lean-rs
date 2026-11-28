---
name: formal-actor-model
description: Use for designing, specifying, implementing, or reviewing actor-model concurrency in Lean, Rust, or Lean/Rust FFI systems. Trigger for actor runtimes, supervised workers, mailboxes, message-passing APIs, bounded queues, failure/restart semantics, fairness/liveness claims, Lean transition-system models of concurrent systems, Rust Tokio actor patterns, or unsafe FFI boundaries between Lean and Rust.
---

# Formal Actor Model

An actor owns private state, processes messages serially, and exposes a narrow handle. Specify delivery, ordering,
capacity, failure, and ownership before choosing runtime mechanisms.

For work in this repository, read the worker and supervisor invariants in
[docs/architecture.md](../../../docs/architecture.md) first. The references below are design guidance, not a claim that
lean-rs implements a generic actor framework or a Tokio runtime.

## References

Load the files that answer the current question:

- [Literature map](references/literature-map.md): primary sources and the claims each supports.
- [Actor semantics](references/actor-semantics.md): configurations, transitions, traces, open systems, and fairness.
- [Lean modeling](references/lean-modeling.md): safety invariants, executable models, and proof boundaries.
- [Rust runtime](references/rust-runtime.md): handles, typed messages, bounded queues, cancellation, and observability.
- [Lean/Rust FFI](references/lean-rust-ffi.md): ownership, initialization, callbacks, and opaque handles.
- [Supervision and failure](references/supervision-and-failure.md): restart limits, worker death, retries, and shutdown.

## Workflow

1. Identify the private state, message vocabulary, reply protocol, and lifecycle owner. Explain why an actor fits the
   workload before adding a receive loop or framework.
2. State delivery, ordering, capacity, backpressure, cancellation, and restart guarantees. Distinguish accepted, queued,
   processed, and replied. Default to at-most-once delivery; stronger guarantees need an implemented protocol.
3. Model the boundary with configurations, events, and a transition relation. State fairness assumptions explicitly.
   Keep implementation details out of the model unless callers can observe them.
4. Implement narrow handles over the existing task, thread, or process owner. Use bounded mailboxes by default; justify
   unbounded storage with a memory argument and overload policy. Do not introduce generic traits before concrete callers
   need them.
5. Connect implementation observations to model transitions where a refinement claim is required. Prove or test the
   relevant invariants, overload, shutdown, restart limits, and recovery. An executable model alone does not establish
   that the Rust implementation refines it.

## Review

| Risk | Required check |
| --- | --- |
| State shared outside its owner | Callers use domain operations rather than receivers, locks, or mutable actor state. |
| Queue overload | Capacity, reject/wait/drop behavior, and observability match the stated contract. |
| Delivery or fairness overclaim | Acknowledgements, retry/deduplication, ordering, and scheduler assumptions support the claim. |
| Blocking or reentrancy | Foreign calls and waits do not hold locks or create an unhandled synchronous actor cycle. |
| Failure or stale handles | Panic, timeout, worker death, restart generation, and exhaustion have typed outcomes. |
| Shutdown or callbacks after destruction | Drain/drop policy is explicit; unregistering and lifetime checks prevent stale callbacks. |
| FFI ownership or thread leakage | Every crossing has an owner and thread contract; Lean-derived handles stay on their owning thread. |

Keep raw symbols and unsafe implementation in the repository's established boundary. Use its safe initialization,
conversion, and callback APIs. Foreign callbacks may enqueue owned Rust data allowed by the thread contract; enqueuing a
Lean object does not make it safe to move between threads.
