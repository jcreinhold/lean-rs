---
name: formal-actor-model
description: Use for designing, specifying, implementing, or reviewing actor-model concurrency in Lean, Rust, or Lean/Rust FFI systems. Trigger for actor runtimes, supervised workers, mailboxes, message-passing APIs, bounded queues, failure/restart semantics, fairness/liveness claims, Lean transition-system models of concurrent systems, Rust Tokio actor patterns, or unsafe FFI boundaries between Lean and Rust.
---

# Formal Actor Model

Design actor systems as deep modules: each actor owns private state, processes messages serially, and exposes a narrow
address/handle interface. Treat scheduling, queueing, failure, and FFI ownership as explicit semantic decisions, not
incidental implementation details.

## Reference Selection

Load only the reference files needed for the task:

- `references/literature-map.md` for primary sources and which source supports which design claim.
- `references/actor-semantics.md` for the formal actor model: configurations, mailboxes, transitions, traces, fairness,
  and open systems.
- `references/lean-modeling.md` for Lean transition-system models, invariants, traces, executable models, and proof
  boundaries.
- `references/rust-runtime.md` for a Rust runtime design: handles, typed messages, bounded mailboxes, supervision,
  cancellation, and observability.
- `references/lean-rust-ffi.md` for opaque handles, Lean ABI ownership, callbacks, runtime/thread initialization, and
  safe boundary design.
- `references/supervision-and-failure.md` for OTP-style supervision, restart intensity, worker death, retries, and
  structured failure reporting.
- `references/pitfalls.md` before finalizing a design or review.

## Workflow

1. Name the actor boundary. Identify the private state, message vocabulary, reply protocol, lifecycle owner, and one
   reason this should be an actor rather than a pure function, mutex-protected object, stream, or one-shot task.

2. State semantic guarantees before coding. Specify delivery, ordering, mailbox capacity, backpressure, cancellation,
   restart policy, and whether fairness is enforced, assumed, or out of scope. Do not claim exactly-once, global FIFO,
   or fairness unless the runtime design actually provides it.

3. Separate model from runtime. In Lean, model actor systems as transition systems over configurations and traces. In
   Rust, implement a practical runtime with narrow handles and private queues. Connect them by a stated
   simulation/refinement relation when needed.

4. Push complexity behind the boundary. Public callers should send typed messages, await typed replies when needed, and
   observe structured statuses. They should not manage channels, locks, task handles, raw pointers, thread
   initialization, or scheduler details.

5. Validate the concurrency story. Prove or test single-message atomicity, mailbox bounds/backpressure, shutdown
   behavior, restart limits, no source of shared mutable state outside the actor, and recovery after actor failure.

## Design Defaults

- Use bounded mailboxes by default. Choose unbounded queues only with a measured memory argument and explicit overload
  behavior.
- Use at-most-once delivery as the default semantic claim. Build acknowledgements, retries, deduplication, and
  idempotence explicitly when stronger behavior is required.
- Preserve per-sender ordering only when the chosen channel/runtime provides it and no mediator invalidates it.
- Keep actor handles cheap to clone but semantically narrow: `send`, `call`, `stop`, and `status` are usually enough.
- Treat panics, worker exits, closed mailboxes, timeout, cancellation, and restart exhaustion as structured events.
- Keep `unsafe` and FFI in one private module with documented ownership and thread-safety contracts.

## Lean Guidance

- Define `Config`, `Event`, and a small-step relation before implementation details.
- Model actors by identity, local behavior, mailbox contents, and private state. Keep the scheduler/interleaving
  relation private behind `Step`.
- Prove safety invariants first: no duplicate in-flight reply IDs, no processing without a mailbox entry, bounded queue
  preservation, and state ownership.
- Treat liveness and fairness as assumptions unless the scheduler model enforces them.
- If extracting or testing an executable model, keep it a witness of the formal transition relation rather than the
  source of truth.

## Rust Guidance

- Prefer one task per actor with a private receiver loop and a public handle wrapping only sender capabilities.
- Represent messages as an enum or trait-object protocol only after choosing the type-safety tradeoff deliberately.
- Use `tokio::sync::mpsc::channel(n)` or an equivalent bounded queue for backpressure. Pair request/reply messages with
  `oneshot` responders.
- Supervise actors through an owner that starts them, observes exits, applies restart limits, and exposes health.
- Avoid exposing `JoinHandle`, `Receiver`, `Arc<Mutex<State>>`, raw pointers, or internal queue types in the public API.

## Lean/Rust FFI Guidance

- Use opaque handles with explicit create/destroy functions; do not expose Rust structs or Lean runtime objects directly
  across the ABI.
- Define ownership for every crossing: who owns the value now, who decrements/frees it, and whether the value is
  borrowed.
- Initialize the Lean runtime, modules, and Rust-created threads before calling into Lean.
- For callbacks from foreign threads, enqueue data into the owning runtime rather than mutating Rust or Lean state
  directly.
- Keep cross-boundary messages serialized or represented by stable opaque IDs unless both runtimes can enforce the same
  lifetime and thread-safety contract.

## Review Checklist

- Does the public API describe actor intent rather than implementation mechanics?
- Are delivery, ordering, capacity, restart, and cancellation guarantees explicit and testable?
- Is there a single owner for each actor's mutable state?
- Can a failed actor be restarted or reported without corrupting callers' mental model?
- Are all raw pointers, Lean objects, unsafe blocks, and thread initialization details private?
- Do tests or Lean proofs cover overload, shutdown, worker death, and post-failure recovery?
