# Pitfalls

Read this before finalizing an actor design or review.

## Shallow Actor Wrappers

Bad sign: the actor API exposes `Sender`, `Receiver`, `JoinHandle`, `Arc<Mutex<State>>`, or a public `run_loop`.

Fix: expose a small handle with domain operations. Keep channel and task details private.

## Unbounded Mailboxes

Bad sign: unbounded queues are chosen because they are easy.

Why it fails: overload becomes memory growth, latency spikes, and eventual process death.

Fix: bounded capacity plus explicit overload behavior and metrics.

## False Delivery Claims

Bad sign: "message sent" is treated as "message processed".

Fix: distinguish accepted, queued, delivered, processed, acknowledged, and replied. Use business-level acknowledgements
for stronger guarantees.

## Hidden Blocking

Bad sign: an actor holds a lock while awaiting, calls foreign code while holding state, or blocks a runtime worker.

Fix: perform blocking work in a dedicated worker or process. Do not hold actor state across unknown blocking calls unless
the actor is explicitly unavailable during that time.

## Shared Mutable State Outside The Actor

Bad sign: actor state is an `Arc<Mutex<_>>` used by callers.

Fix: state belongs to the actor. Expose messages or query APIs.

## Raw Pointer Leakage

Bad sign: public APIs accept or return `*mut c_void`, `lean_object *`, or integer handles without lifecycle semantics.

Fix: wrap handles in private types, document ownership, and make destroy/status functions explicit.

## Callback Lifetime Bugs

Bad sign: a callback can run after its target object is dropped.

Fix: unregister before destroy, use generation tokens, and enqueue callbacks into an actor that checks liveness.

## Reentrant Actor Calls

Bad sign: a message handler synchronously calls back into itself or into a cycle of actors while holding state.

Fix: make reentrancy a deliberate protocol, or break cycles with queued messages and correlation IDs.

## Unverifiable Fairness Claims

Bad sign: a theorem says every message is eventually processed without a scheduler assumption.

Fix: state fairness assumptions explicitly or prove them from the scheduler implementation.

## Supervision Without Restart Limits

Bad sign: a supervisor restarts forever.

Fix: restart intensity, period, escalation, and observable unhealthy status.

## FFI Without Thread Initialization

Bad sign: Rust-created threads call Lean functions or touch Lean objects without documented runtime/thread initialization.

Fix: initialize the Lean runtime and each foreign thread according to Lean's FFI requirements before crossing the boundary.

## Over-General Frameworks

Bad sign: traits, plugins, middleware, dynamic dispatch, and generic mailboxes appear before there is a second actor type.

Fix: implement one concrete actor boundary. Generalize only after repeated shape is real.
