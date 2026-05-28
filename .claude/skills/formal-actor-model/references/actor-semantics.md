# Actor Semantics

This reference gives a compact semantic model suitable for Lean specifications and implementation reviews. It is not a
full actor calculus; it is the minimum useful model for designing a sound runtime boundary.

## Core Objects

- `ActorId`: stable logical address. It is not a thread ID, task handle, file descriptor, or raw pointer.
- `Message`: typed payload plus optional reply address/correlation ID.
- `Behavior`: private transition function for one actor. It consumes one message and local state, then produces a new
  local state plus effects.
- `Mailbox`: ordered or partially ordered multiset/queue of pending messages. Its capacity and ordering policy are part
  of the semantics.
- `Config`: global system snapshot containing actor table, mailboxes, in-flight sends, supervisor state, and external
  interface assumptions.
- `Event`: observable transition label such as `send`, `receive`, `spawn`, `stop`, `crash`, `restart`, `drop`,
  `timeout`, or `external`.

## Minimal Step Relation

Model the runtime with a small-step relation:

```lean
inductive Step : Config -> Event -> Config -> Prop
```

Good constructors are:

- `receive`: choose an enabled actor with a non-empty mailbox, dequeue according to mailbox policy, and run its behavior.
- `send`: enqueue or record attempted delivery; model full queues explicitly.
- `spawn`: allocate a fresh actor ID, initial state, behavior, and mailbox.
- `stop`: remove or mark an actor as stopped, handling queued messages by the chosen policy.
- `crash`: record abnormal termination and notify a supervisor.
- `restart`: apply supervisor strategy, reset state, and update restart counters.
- `external`: represent messages arriving from outside the modeled system.

Keep scheduler choice private in `Step`. Do not expose "run actor A now" as a public API unless the purpose is model
checking with an explicit scheduler.

## Semantic Guarantees To State

- Delivery: default to at-most-once. Stronger guarantees require explicit acknowledgement, retry, deduplication, and
  idempotent handling.
- Ordering: state whether FIFO is per mailbox, per sender-recipient pair, causal, priority-based, or unspecified.
- Capacity: state whether queues are bounded, rendezvous, dropping, rejecting, or unbounded.
- Atomicity: one message handler observes and updates the actor's private state without interleaving from another
  handler for the same actor.
- Failure: state whether actor death loses state, drains mailbox, preserves mailbox, restarts with initial state, or
  recovers from a journal/snapshot.
- Fairness: state as an assumption unless the scheduler is proved fair.

## Open Systems

Actor systems usually interact with external clients, OS processes, runtimes, or foreign code. Model this explicitly:

- External messages may be delayed, duplicated, reordered, or dropped unless a boundary protocol rules that out.
- External callbacks should be represented as events entering the actor system, not as arbitrary state mutation.
- A model of an "open" actor system should include the set of messages the environment is allowed to inject.

## Useful Invariants

- Every queued message targets a live actor or is accounted for as a dead letter/drop.
- Actor IDs are fresh on spawn unless the design intentionally uses virtual actor identities.
- At most one handler step for a given actor is active at a time.
- Bounded mailbox capacities are preserved.
- Every reply ID is unique while in flight.
- Supervisor restart counters never exceed their configured window without escalation.

## Refinement Shape

When connecting a Lean model to Rust:

- Define an abstract `Step` relation first.
- Define an implementation observation function from runtime traces/logs to model events.
- Prove or test that each implementation event sequence corresponds to zero or more model steps.
- Keep timing, thread IDs, queue internals, and task handles out of the abstract model unless they are user-visible
  semantics.
