/-!
# Worker runtime model vocabulary

This file contains the small abstract vocabulary shared by the worker and pool
transition-system skeletons. It intentionally depends only on `Init`.
-/

namespace RuntimeModel

/-- Parent-side epoch identifying one concrete child process instance. -/
abbrev Generation := Nat

/-- Identifier for one request admitted by the abstract supervisor. -/
abbrev RequestId := Nat

/-- Identifier for one affine pool lease. -/
abbrev LeaseId := Nat

/-- Identifier for one pool client. -/
abbrev ClientId := Nat

/-- Parent-visible terminal outcome for an admitted request. -/
inductive Outcome where
  | response
  | timeout
  | cancelled
  | childExited
  | childPanicOrAbort
  | rssHardLimitExceeded
  | protocolError
  | sinkPanic
  | restartLimitExceeded
deriving DecidableEq, Repr

/-- Observable runtime events retained by the abstract trace model. -/
inductive Event where
  | spawn (generation : Generation)
  | accept (generation : Generation) (request : RequestId)
  | row (generation : Generation) (request : RequestId)
  | diagnostic (generation : Generation) (request : RequestId)
  | progress (generation : Generation) (request : RequestId)
  | terminal (generation : Generation) (request : RequestId) (outcome : Outcome)
  | shutdownStart (generation : Generation)
  | terminateSent (generation : Generation)
  | killSent (generation : Generation)
  | reaped (generation : Generation)
  | restartAdmitted (fromGeneration toGeneration : Generation)
  | restartRefused (generation : Generation)
  | leaseGranted (client : ClientId) (lease : LeaseId)
  | leaseConsumed (lease : LeaseId)
  | leaseReleased (lease : LeaseId)
  | leaseDropped (lease : LeaseId)
  | admissionRefused (client : ClientId)
deriving DecidableEq, Repr

/-- The generation carried by an event, when the event is generation-indexed. -/
def Event.generation? : Event -> Option Generation
  | .spawn g => some g
  | .accept g _ => some g
  | .row g _ => some g
  | .diagnostic g _ => some g
  | .progress g _ => some g
  | .terminal g _ _ => some g
  | .shutdownStart g => some g
  | .terminateSent g => some g
  | .killSent g => some g
  | .reaped g => some g
  | .restartAdmitted g _ => some g
  | .restartRefused g => some g
  | .leaseGranted _ _ => none
  | .leaseConsumed _ => none
  | .leaseReleased _ => none
  | .leaseDropped _ => none
  | .admissionRefused _ => none

/-- An event belongs to a generation exactly when its generation tag is that generation. -/
def Event.BelongsToGeneration (event : Event) (generation : Generation) : Prop :=
  event.generation? = some generation

/-- An event is the terminal outcome for a specific generation and request. -/
def Event.CompletesRequest
    (event : Event) (generation : Generation) (request : RequestId) (outcome : Outcome) : Prop :=
  event = .terminal generation request outcome

/-- Events from stale generations cannot complete a request admitted under another generation. -/
theorem stale_generation_events_cannot_complete
    {event : Event} {actual expected : Generation} {request : RequestId} {outcome : Outcome}
    (belongs : event.BelongsToGeneration actual)
    (completes : event.CompletesRequest expected request outcome) :
    actual = expected := by
  cases completes
  simp [Event.BelongsToGeneration, Event.generation?] at belongs
  exact belongs.symm

end RuntimeModel
