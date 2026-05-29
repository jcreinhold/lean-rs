# Literature Map

Use this file to choose the right source before designing an actor system. Do not treat every runtime convention as a
semantic theorem; each source supports a different layer.

## Core Actor Semantics

- Hewitt, Bishop, and Steiger, "A Universal Modular ACTOR Formalism for Artificial Intelligence" (IJCAI 1973)
  - URL: https://worrydream.com/refs/Hewitt_1973_-_A_Universal_Modular_Actor_Formalism_for_Artificial_Intelligence.pdf
  - Use for the original conceptual unification: actors as the single primitive, message sending as the basic operation,
    actor-local intentions/contracts, capability-like protection, scheduling, monitoring, and resource management.
  - Design lesson: do not expose semaphores, interrupts, locks, or queue internals at the actor API. The actor boundary
    should absorb those mechanisms.

- Gul Agha, *Actors: A Model of Concurrent Computation in Distributed Systems* (MIT Press, 1986)
  - URL: https://mitpress.mit.edu/9780262010924/actors/
  - Use for the actor paradigm as a model for distributed, large-scale, parallel computation with dynamic growth,
    reconfiguration, abstraction, and compositionality.
  - Design lesson: model actor identity and reconfiguration explicitly; do not assume a fixed thread/process topology.

- Agha, Mason, Smith, and Talcott, "A Foundation for Actor Computation" (JFP 1997)
  - URL:
    https://www.cambridge.org/core/journals/journal-of-functional-programming/article/foundation-for-actor-computation/E9A5266BA5D37A1856D50C939679F31C
  - Use for an operational-semantics view: actor configurations, open distributed systems, composability, testing
    equivalence, and fairness.
  - Design lesson: formalize actor systems as configurations and transitions before proving behavior.

## Production Actor Practice

- Erlang/OTP Supervisor Behaviour
  - URL: https://www.erlang.org/doc/system/sup_princ.html
  - Use for supervision trees, restart strategies, child specifications, restart intensity, shutdown, and escalation.
  - Design lesson: restart policy is a first-class part of the system, not an afterthought.

- Akka Message Delivery Reliability
  - URL: https://doc.akka.io/libraries/akka-core/current/general/message-delivery-reliability.html
  - Use for honest delivery and ordering claims: at-most-once by default, per sender-recipient ordering when supported,
    explicit acknowledgement/retry for stronger semantics, and dead-letter limitations.
  - Design lesson: do not promise reliable or exactly-once delivery unless the protocol implements it.

- Orleans Virtual Actors
  - URL: https://www.microsoft.com/en-us/research/project/orleans-virtual-actors/
  - Use for actor identity decoupled from process location, automatic activation, placement, deactivation, and recovery.
  - Design lesson: virtual identity can simplify callers, but it moves complexity into placement/recovery semantics.

## Rust Runtime Sources

- Tokio channels tutorial
  - URL: https://tokio.rs/tokio/tutorial/channels
  - Use for command enums, bounded `mpsc` queues, cloned senders, manager tasks, and `oneshot` replies.
  - Design lesson: bounded queues produce backpressure; `oneshot` responders encode request/reply without sharing state.

- Rustonomicon, `Send` and `Sync`
  - URL: https://dev-doc.rust-lang.org/nightly/nomicon/send-and-sync.html
  - Use for thread-safety contracts, raw-pointer hazards, and when an unsafe `Send`/`Sync` implementation is sound.
  - Design lesson: raw pointers and unsafe thread-safety claims belong behind a small audited abstraction.

- Rustonomicon, FFI
  - URL: https://doc.rust-lang.org/nightly/nomicon/ffi.html
  - Use for wrapping raw C APIs, callbacks, asynchronous callback hazards, and safe high-level interfaces over unsafe
    boundaries.
  - Design lesson: if a foreign thread calls back into Rust or Lean, forward work into the owning runtime by message.

## Lean Runtime Sources

- Lean Language Reference, Tasks and Threads
  - URL: https://lean-lang.org/doc/reference/latest/IO/Tasks-and-Threads/
  - Use for `Task`, cancellation, `IO.asTask`, `Std.Channel`, bounded/unbounded channels, mutexes, and Lean runtime
    threading behavior.
  - Design lesson: Lean tasks are not actors by themselves; actor semantics must be built by a private receive loop,
    mailbox policy, and state ownership discipline.

- Lean Language Reference, Foreign Function Interface
  - URL: https://lean-lang.org/doc/reference/latest/Run-Time-Code/Foreign-Function-Interface/
  - Use for `@[extern]`, `@[export]`, Lean ABI translation, owned vs borrowed `lean_object *`, initialization, and
    thread initialization.
  - Design lesson: FFI signatures are not enough; each boundary needs explicit ownership and runtime-init invariants.

## Source Selection Rules

- For semantic proofs, start with AMST plus `actor-semantics.md`.
- For production restart behavior, start with Erlang/OTP plus `supervision-and-failure.md`.
- For message delivery and ordering claims, start with Akka and state exactly which guarantee is implemented.
- For Rust implementation, start with Tokio plus Rustonomicon `Send`/`Sync`.
- For Lean/Rust boundaries, start with Lean FFI, Lean tasks, and Rustonomicon FFI.
