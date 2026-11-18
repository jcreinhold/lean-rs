# Worker custody model

This directory specifies one small `lean-rs` protocol: ownership of a native
worker from launch reservation through physical cleanup. It does not specify
host jobs, request results, databases, or downstream admission policy.

`WorkerCustody.tla` separates four facts that callers must not conflate:

1. a launch or live worker still occupies compute capacity;
2. the worker process group is physically present;
3. one open-file description still holds the shared artifact lock;
4. a writer has acquired the conflicting lock.

The model is a finite design check. TLC does not prove the Rust code, the OS,
or the assumptions below. The mapping and focused tests identify evidence that
must accompany an implementation claim.

The state is split by concern rather than by implementation object:

| State | Why it is separate |
| --- | --- |
| `phase` | orders reservation, attachment, cleanup and retirement |
| `rootPresent`, `groupPresent` | direct-child exit and whole-group closure are different facts |
| `guardianPresent`, `guardianStarted`, `lifetimeAttached` | helper existence, its single physical lifetime and parent cleanup authority have different failure cuts |
| `aliases`, `ofdLocked` | descriptor aliases and their shared lock state differ after explicit unlock |
| `groupObservation`, `guardianObservation` | authenticated knowledge can remain unknown after physical state changes |
| `capacityReserved`, `capacityReturns` | resource accounting is distinct from process and lock state |
| `writerAdmitted` | states the conflicting-admission safety outcome directly |

Each transition represents one named boundary: reserve, spawn or spawn failure,
guardian readiness, lifetime attachment, alias transfer, direct
root exit, stop request, guardian loss and later detection, physical cleanup,
authenticated absence observation, retirement, guardian close, explicit unlock,
or writer acquisition. These boundaries preserve the distinct physical,
observational and authority facts used by the checked counterexamples. The
model deliberately omits request envelopes, command order, result persistence,
pools of workers and database recovery.

## Observable contract

- Cleanup ownership and a capacity reservation exist before spawn.
- Spawn failure releases the unused reservation.
- Failure before lifetime attachment cannot orphan a spawned worker: it moves
  to cleanup and the selected contract retains its reservation until physical
  group absence is certified. The current direct-child fallback has not yet
  been shown to meet this selected contract.
- Losing the guardian is not closure. A responsive owner must observe that loss
  before it can request cleanup.
- Capacity returns at most once, and never while the worker group is present.
- A writer is not admitted while protected worker execution remains possible.
- An unknown observation stays fenced. It is not converted into absence.
- Under fair cleanup, authentic observation, and retirement, a stopping worker
  eventually releases its capacity reservation. This is conditional progress,
  not a scheduler or OS guarantee.

The `historical_lock_only` mode deliberately admits a writer from lock state
alone. TLC produces the expected counterexample after guardian loss: the
guardian was the last descriptor alias, so its exit releases the lock while the
worker group remains present. This is a model of the observed failure shape,
not proof of its cause in any particular run.

The motivating disposable macOS experiment observed an exclusive lock on the
same inode while the matching payload group still appeared present about 23 ms
later. The controller then reported closed, 85 later samples were clean, and the
final census found the processes absent. It did not identify the remaining
group member, prove a live writer, observe a concurrent write, or establish an
escaped descendant. The model preserves only the warranted counterexample:
lock-based admission can precede authenticated group closure.

## OS and trust assumptions

The checked good protocol assumes:

- a private worker process group has no escaped or regrouped descendants;
- group absence is reported only by an authenticated observer for the exact
  generation and process birth identity;
- responsive cleanup and observation authority remains alive; supervisor loss
  and authority recovery are outside this model;
- after stopping begins, fair stop requests eventually remove the whole group;
  frozen or unkillable workers may remain fenced forever;
- descriptor transfer, `dup`, `fork`, and `exec` preserve aliases to the same
  open-file description; closing the final alias releases its `flock`;
- an explicit `LOCK_UN` through any alias releases that open-file description's
  lock for all aliases;
- managed worker code does not explicitly unlock or replace the protected
  descriptor behind the protocol's back;
- the ownership registry and capacity counter serialize updates without torn or
  forged records.

The last three are environmental obligations. The explicit-unlock negative
mode combines an unlock with the historical lock-only admission rule; it shows
that descriptor aliases do not preserve a lock after one alias unlocks it. The
selected observation-based managed-writer rule still fences that state. An
uncooperative external writer that ignores the registry can acquire the lock,
so this is not a sandbox against adversarial native code.

## Mapping to current Rust

| Model step or fact | Current implementation | Focused evidence / missing evidence |
| --- | --- | --- |
| reserve before spawn | `LeanWorkerProcessTracker::launch`, `process.rs` | `reaper_start_failure_prevents_launch_and_releases_no_unowned_capacity`; `failed_launch_holds_reservation_until_spawn_returns` |
| attach exact lifetime and private group | `OwnedChild::attach_lifetime`; `OwnedCommandGroup::adopt_private_session` | process-group identity tests; before attachment, direct wait can release the tracker without a whole-group witness, so conformance to this selected contract is unresolved |
| guardian loss requests stop | `OwnedChild::poll_guardian`; `LeanWorkerProcessTracker::reap_pending` | source mapping exists; current-code guardian-loss regression is still missing |
| authentic physical closure | `OwnedCommandGroup::try_wait`; `ParentLifetimeLease::retirement_closed` | direct wait and ECHILD/whole-group tests; OS behavior remains assumed |
| custody-capacity return | `LeanWorkerProcessTracker::release`; `ParentLifetimeLease::retirement_closed` | tracker reservation/drop tests; guarded closure requires group and guardian absence, but no refinement test exists |
| shared lock aliases | `shared_reader`, `native_guardian::retain_reads`, child inherited reads | transfer/union tests cover selected paths; no explicit-unlock negative test |
| writer exclusion | `writer_lock` and shared-reader locks | same-inode contention tests; guardian-loss qualification exposed an early-admission trace |

This model returns the process tracker's custody-capacity reservation. Registry
compute occupancy, which may ignore a guardian-only record after payload closure,
is a separate policy outside this model. `retirement_closed` is evidence for resource closure, not an exit-status
receipt. The model likewise keeps physical closure separate from a request's
semantic outcome.

`"worker"` in `aliases` represents all payload-side aliases collectively, not
one literal root-process descriptor. `GuardianReady` abstracts the protected
read interval in which the helper has accepted custody; it does not model the
earlier guardian bootstrap before artifact reads.

## TLC checks

The checked state space is intentionally small: one generation, one worker
group, one guardian, and one shared open-file description.

```sh
cd specs/worker-custody
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config Good.cfg WorkerCustody.tla
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config HistoricalLockOnly.cfg WorkerCustody.tla
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config StaleClosure.cfg WorkerCustody.tla
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config ExplicitUnlockLockOnly.cfg WorkerCustody.tla
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config DirectChildOnly.cfg WorkerCustody.tla
```

Expected outcomes:

- `Good.cfg`: all safety invariants and conditional progress pass;
- `HistoricalLockOnly.cfg`: `WriterExclusion` fails after guardian loss;
- `StaleClosure.cfg`: `NoEarlyCapacityReturn` fails when forged or stale
  absence is treated as authentic.
- `ExplicitUnlockLockOnly.cfg`: `WriterExclusion` fails when explicit unlock is
  combined with the historical lock-only admission rule.
- `DirectChildOnly.cfg`: `NoEarlyCapacityReturn` fails when direct-child exit is
  mistaken for whole-group absence before lifetime attachment.

Keep the failing configurations. They are executable statements of distinct trust
boundaries, not passing qualification cases.

With TLC 2.19, Java 21.0.13.0.3 and one worker, the checked good configuration
generated 130 states and found 62 distinct states. The negative configurations
stop at their first expected counterexample, so their explored counts are not
complete state-space sizes.

## Composition boundary

The host may request a fresh worker and consume these outputs only:

- launch admitted or refused;
- worker attached or cleanup pending;
- closure certified, or closure unknown;
- capacity returned once.

The host must not infer closure from a free artifact lock, guardian death,
controller death, a numeric PID, or missing status. End-to-end host properties
must state their reliance on this interface; passing this model and a host model
separately is not a composition proof.

Each main worker, preparation worker, or semantic companion is a separate
instance of this custody contract. Multiple commands within one companion are
host/worker-protocol semantics and stay outside this model. The host must keep a
companion's capacity reserved when its command outcome is known but cleanup is
not; the SDK reports those facts separately.
