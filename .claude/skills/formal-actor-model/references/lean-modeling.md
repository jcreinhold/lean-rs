# Lean Modeling

Use Lean to state the actor semantics and prove safety properties. Do not try to prove the whole scheduler correct on
the first pass; isolate a small transition boundary and grow from there.

## Recommended Shape

Start with abstract, finite or finitely supported state:

```lean
structure ActorSpec where
  State : Type
  Msg : Type
  step : State -> Msg -> State -> List Effect -> Prop

structure Config where
  actors : ActorId -> Option ActorCell
  mailboxes : ActorId -> Mailbox Message
  supervisors : SupervisorState

inductive Event where
  | delivered : ActorId -> MessageId -> Event
  | sent : ActorId -> ActorId -> MessageId -> Event
  | spawned : ActorId -> Event
  | stopped : ActorId -> Event
  | crashed : ActorId -> Failure -> Event
  | restarted : ActorId -> Event
  | dropped : ActorId -> MessageId -> DropReason -> Event

inductive Step : Config -> Event -> Config -> Prop
```

Keep the real scheduler, queue representation, and implementation data structures behind `Step`.

## Safety First

Good first theorems:

- `mailbox_bound_preserved`: every `Step` preserves configured mailbox capacities.
- `single_owner_state`: only the recipient actor's state changes during a receive step, except supervisor metadata.
- `no_reply_id_collision`: fresh reply IDs remain unique.
- `live_target_or_drop`: after delivery attempts, each message is queued for a live actor or accounted for as dropped.
- `restart_limit_respected`: restart counters either stay within limits or force escalation.

Prefer invariants over trace liveness until the safety story is stable.

## Liveness And Fairness

Lean proofs of actor liveness almost always depend on scheduler assumptions. Make them explicit:

```lean
def FairScheduler (tr : Trace Event) : Prop := ...
theorem eventually_processed
    (hfair : FairScheduler tr)
    (henabled : EventuallyAlwaysEnabled actor msg tr) :
    Eventually (MessageProcessed actor msg) tr := ...
```

Do not hide fairness inside executable code. If the implementation is best-effort, the theorem should say best-effort.

## Executable Models

Executable models are useful for examples and tests, but they should refine the relation:

- Write a deterministic scheduler only as one implementation of the abstract transition system.
- Prove each executable transition implies `Step`.
- Test nondeterministic edge cases by enumerating small configurations or by property tests outside Lean.

## Modeling Bounded Queues

Represent capacity in the type or in an invariant:

```lean
structure Mailbox (Msg : Type) where
  entries : List Msg
  capacity : Option Nat
  bounded : capacity = none \/ entries.length <= capacity.getD entries.length
```

If using a type-level bound, keep the proof burden local to mailbox constructors. Do not force every actor theorem to
carry queue arithmetic.

## FFI Boundary Invariants

When Lean specifies a Rust-backed actor system, model handles abstractly:

- `HandleValid h cfg`: the handle refers to a live or recoverable actor.
- `OwnsRust h`: Lean is allowed to send commands through `h` but cannot inspect Rust state.
- `CallbackWellFormed e`: every callback event originated from a registered handle before destruction.

The model should not expose raw pointer values, Lean reference counts, or Rust channel internals.
