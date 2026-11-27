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
| `phase`, `supervisorAlive` | orders reservation and cleanup while keeping owner death separate |
| `rootPresent`, `groupPresent` | direct-child exit and whole-group closure are different facts |
| `guardianPresent`, `guardianStarted` | helper existence and its single physical lifetime have different failure cuts |
| `identitiesAuthenticated`, `bootstrapIdentityDurable`, `leaseAdopted`, `runningDurable`, `startupAcked` | authentication, recovery evidence, ownership, publication and admission cannot be one fallible step |
| `bootstrapClosureDurable` | pre-Running recovery needs a persistent group-closure receipt, not parent death |
| `aliases`, `ofdLocked` | descriptor aliases and their shared lock state differ after explicit unlock |
| `protectedReadsEver` | historical lock-only cases require a real post-ACK read-custody interval |
| `groupObservation`, `guardianObservation` | authenticated knowledge can remain unknown after physical state changes |
| `capacityReserved`, `capacityReturns` | resource accounting is distinct from process and lock state |
| `writerAdmitted` | states the conflicting-admission safety outcome directly |

Each transition represents one named boundary: reserve, spawn or spawn failure,
guardian readiness, identity authentication, durable bootstrap identity,
cleanup-lease adoption, durable
Running publication, startup ACK, alias transfer, direct
root exit, stop request, guardian loss and later detection, physical cleanup,
authenticated absence observation, retirement, guardian close, explicit unlock,
or writer acquisition. These boundaries preserve the distinct physical,
observational and authority facts used by the checked counterexamples. The
model deliberately omits request envelopes, command order, result persistence,
pools of workers and database recovery.

## Observable contract

- Cleanup ownership and a capacity reservation exist before spawn.
- Spawn failure releases the unused reservation.
- Startup is admitted only after exact worker and guardian identities are
  authenticated, bootstrap recovery identity is durably published, their
  cleanup lease is adopted by the tracker, Running is durably published, and
  the live child/guardian receive ACK, in that order.
- A failure or supervisor crash at any earlier cut retains the reservation
  until authenticated physical group and guardian absence. The independent
  guardian is the bootstrap cleanup authority before lease adoption; without
  either authority, progress is unresolved and remains fenced.
- The durable Preparing reservation is not collectible merely because its
  supervisor died once spawn may have occurred. Before Running, a surviving
  guardian or adopted owner must publish group closure; guardian absence is
  then authenticated separately before retirement.
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
- a live guardian detects exact supervisor death through its control channel or
  recorded birth identity and keeps its session/group signaling authority;
- at least one modeled cleanup authority remains responsive for conditional
  progress; simultaneous supervisor and guardian loss may remain fenced until
  the group exits by itself;
- after stopping begins, fair stop requests eventually remove the whole group;
  frozen or unkillable workers may remain fenced forever;
- a group may also exit naturally after all cleanup authorities disappear;
  passive recovery still requires persisted authenticated identity or a durable
  bootstrap-closure receipt;
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
| reserve before possible spawn | `LeanWorkerProcessTracker::launch`; `ParentLifetime::dispatch`; `LeanWorkerProcessTracker::spawn_guarded` | Tracker capacity and durable `dispatching` precede `Command::spawn`. Its error can publish `preparing_closed`, including when a transient child failed exec and was reaped. Rename/fsync uncertainty remains fail-closed. |
| authenticate identities and retain cleanup | `ParentLifetime::accept`; `BootstrapCustody`; `LeanWorkerNativePendingCleanup` | HELLO nonce/protocol/worker identity and kernel birth/session/group identities are checked; authenticated stream and identities enter shared custody before fallible publication; `native_pending_cleanup_retains_exact_closure` covers the retained error owner |
| persist bootstrap recovery identity | `ParentLifetime::accept`; `native_guardian::publish_startup_owner` | durable `bootstrap` publication binds parent, worker, guardian, generation and capacity slot before adoption; recovery after loss of both cleanup authorities remains unsupported |
| adopt cleanup lease | `OwnedChild::attach_lifetime`; `OwnedCommandGroup::adopt_private_session` | adoption precedes Running publication; an adoption or publication error returns typed `AfterSpawn` custody instead of a plain no-spawn error |
| publish Running, then ACK | `ParentLifetimeLease::publish_running`; `ParentLifetimeLease::acknowledge`; `LeanWorkerNativeScopeLease::acknowledge` | ACK refuses a lease without durable Running; the clean partial-ACK fault test used synthetic admission to isolate the real write loop and observed a positive wire write followed by failure without reporting success |
| guardian loss requests stop | `OwnedChild::poll_guardian`; `LeanWorkerProcessTracker::reap_pending` | `guardian_failure` tests cover direct and deferred parent cleanup. The independent-writer negative control checks that raw EX can open earlier than managed admission. |
| supervisor loss | guardian parent stream and recorded identity; `native_guardian.rs` | `supervisor_loss_at_generated_startup_prefixes` enumerates seven projected startup cuts, including loss after a positive partial ACK write and real Lean work after full ACK. The startup projection does not cover a host GO or durable request store. |
| authentic physical closure | `OwnedCommandGroup::try_wait`; `ParentLifetimeLease::retirement_closed` | direct wait and ECHILD/whole-group tests; OS behavior remains assumed |
| custody-capacity return | `LeanWorkerProcessTracker::release`; `ParentLifetimeLease::retirement_closed`; `BootstrapCustody::closed` | guarded closure requires both group absence and guardian death; the clean retained-`AfterSpawn` test observed tracker 1 before closure and 0 after exact closure, but is not a proof of every wait/ECHILD/drop cut. Exception: a direct child reaped before HELLO closes its own `dispatching` record through `close_unacknowledged_owner_record`, so a guardian that never authenticated may briefly outlive the reservation. The model has no action for this; `GuardianPresentImpliesCapacity` does not cover it. `guarded_child_that_exits_before_hello_closes_its_record_and_slot` tests it. |
| terminal leader in a pinned group | `publish_guardian_zombie_closure`, `matches_zombie_birth`, `observed_group_singleton` | `TerminalLeaderWithPeer` still permits a payload writer. `CleanupToUnreapedLeader` removes the peers while the original guardian pins the group. `PublishZombieClosure` records the exact unreaped terminal leader and singleton observation. |
| separate custody and admission gates | `payload_closed`, `record_can_be_removed`, `protected_native_reads_stopped`, `reserve_owner_record_with_slot` | `ZombieAdmissionFenced` checks same-inode writer admission and successor slot reservation only after durable publication, original guardian closure, and release of the kernel lock. The stopped-parent fixture exercises admission and one replacement reservation while the original parent remains stopped. The surviving-leaf fixture checks refusal while a potential writer remains. |
| shared lock aliases | `shared_reader`, `native_guardian::retain_reads`, child inherited reads | `durable_fd_union_ack_survives_abrupt_root_exit_and_fd_closing_descendant` observes retention after close; `explicit_unlock_releases_alias_lock_but_registry_fences_live_group` observes raw EX and a late write after unlock while the managed reader fence still refuses admission |
| writer exclusion | `writer_lock` and shared-reader locks | same-inode contention tests; guardian-loss qualification exposed an early-admission trace |

The ordinary group-absence path models the process tracker's custody-capacity
return. The pinned terminal-leader path projects the registry's special
stopped-parent admission rule. It tracks one retiring generation and a single
successor reservation, with separate evidence for payload closure, original
guardian closure, and the same-inode lock. A replacement cannot create the
terminal-leader witness after both original owners are lost. Passive collection
can still use authentic group absence. Physical closure and semantic request
outcome remain separate.

`"worker"` in `aliases` represents all payload-side aliases collectively, not
one literal root-process descriptor. `GuardianReady` means the helper has sent
its generation-bound HELLO and can drain the private group if ACK never arrives;
artifact read transfer still occurs after startup and remains abstract here.

The earlier implementation had two distinct hazards. Preparing collection
could use exact parent death as closure even though a pre-ACK child still
existed. Separately, retained `run-before-running/receipt.json` on SDK
`21cd9f5` observed early registry admission while the payload group remained
present, followed by tracker release while the guardian remained live after a
publication collision. Current code instead publishes `dispatching` before
possible spawn, installs authenticated custody before fallible `bootstrap`
publication, adopts before Running, and requires Running before ACK. The clean
focused receipts at `/tmp/lean-sdk-clean-partial-394b6f9/receipt.json` and
`/tmp/lean-sdk-clean-afterspawn-escalated-394b6f9/receipt.json` cover an
isolated real ACK write loop under synthetic admission and retained
`AfterSpawn` cleanup on one Darwin host. They do not cover supervisor loss at
every transition, ECHILD at every cleanup path, escaped descendants, both
cleanup authorities disappearing, Linux managed-group parity, or power loss.
If both authorities disappear in pre-Running `dispatching` or `bootstrap`
without a durable closure receipt, the reservation remains fenced; neither the
code nor the model claims automatic recovery for that state. Persisted Running
has a separate passive group-absence recovery rule.

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
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config PublishBeforeAdopt.cfg WorkerCustody.tla
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config AckBeforeDurable.cfg WorkerCustody.tla
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config LoseAdoption.cfg WorkerCustody.tla
java -cp /path/to/tla2tools.jar tlc2.TLC -workers 1 -config CollectPreparingOnParentDeath.cfg WorkerCustody.tla
```

Expected outcomes:

- `Good.cfg`: all safety invariants and conditional progress pass;
- `HistoricalLockOnly.cfg`: `WriterExclusion` fails after guardian loss;
- `StaleClosure.cfg`: `NoEarlyCapacityReturn` fails when forged or stale
  absence is treated as authentic.
- `ExplicitUnlockLockOnly.cfg`: `WriterExclusion` fails when explicit unlock is
  combined with the historical lock-only admission rule.
- `DirectChildOnly.cfg`: `BootstrapClosureIsSound` fails when direct-child exit
  is mistaken for authenticated bootstrap group closure before lease adoption.
- `PublishBeforeAdopt.cfg`: `AdoptionPrecedesRunning` fails if Running becomes
  durable before the cleanup lease is installed.
- `AckBeforeDurable.cfg`: `RunningPrecedesAck` fails if the child is released
  before durable Running publication.
- `LoseAdoption.cfg`: `AdoptionPrecedesRunning` fails if a published generation
  can lose its installed cleanup authority.
- `CollectPreparingOnParentDeath.cfg`: `NoEarlyCapacityReturn` fails if parent
  death alone collects a pre-ACK reservation while its group remains.
- `ZombieReceiptBeforeCustodyClosed.cfg`: `ZombieAdmissionFenced` fails when
  a successor reserves the slot after publication but before original guardian closure.
- `ZombieWriterWithoutReceipt.cfg`: `ZombieAdmissionFenced` fails when
  the writer is admitted for a remaining numeric group without a durable
  terminal-leader witness.

The pinned terminal-leader model passed `Good.cfg` with 2,193 generated states
and 701 distinct states. TLC rejected both pinned terminal-leader mutations.

Keep the failing configurations. They are executable statements of distinct trust
boundaries, not passing qualification cases.

With TLC 2.19, Java 21.0.13.0.3 and one worker, the checked good configuration
generated 1,461 states and found 465 distinct states. The negative configurations
stop at their first expected counterexample, so their explored counts are not
complete state-space sizes.

## Composition boundary

The host may request a fresh worker and consume these outputs only:

- launch admitted or refused;
- bootstrap cleanup pending, or worker admitted after startup ACK;
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

## Startup-error negative controls

Two focused controls preserve the distinct failures observed by the one-use
`before-running` experiment on runtime `5ff75ec` (SDK model revision `21cd9f5`):

- `DropPreparingAfterPossibleSpawn.cfg` reaches authenticated HELLO before
  Running or lease adoption, then models erroneous Preparing retirement after
  an accept error. `NoEarlyCapacityReturn` fails while payload and guardian are
  present. The concrete cut was `owner.write` returning `AlreadyExists`, followed
  by `ParentLifetime::drop` retiring and deleting the original Preparing record.
- `DirectExitReturnsTrackerCapacity.cfg` reaches authenticated HELLO without
  adoption, then payload absence with a live guardian. It models the unattached
  direct-child wait returning the tracker reservation.
  `GuardianPresentImpliesCapacity` fails even though the payload is absent.

In the first control `capacityReserved` projects the durable registry reservation;
in the second it projects the same in-memory tracker's custody reservation. These
are separate configurations, not a model of two coupled counters. In particular,
the tracker observation must not be inferred from registry admission, and the
registry's guardian-only occupancy policy is not the payload-present failure.
The good protocol is unchanged. Neither negative control simulates filesystem
error delivery, kernel wait semantics, or proves their Rust implementation.

Run these controls with the same Java/TLC command above, substituting their
`.cfg` names. Each must fail its named invariant with a reachable trace; a failed
control is not successful runtime qualification. Preserve the failed experiment
and its clean final census separately from these abstract traces.
