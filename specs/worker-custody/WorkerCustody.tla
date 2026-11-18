---------------------------- MODULE WorkerCustody ----------------------------
EXTENDS Naturals, TLC

CONSTANT Mode

HistoricalLockOnly == "historical_lock_only"
StaleClosure == "stale_closure"
ExplicitUnlockLockOnly == "explicit_unlock_lock_only"
DirectChildOnly == "direct_child_only"

Phases == {"idle", "reserved", "running", "stopping", "closed"}
Observations == {"unknown", "present", "absent"}
Aliases == {"worker", "guardian"}

VARIABLES phase, rootPresent, groupPresent, guardianPresent, guardianStarted, lifetimeAttached,
          aliases, ofdLocked, groupObservation, guardianObservation,
          capacityReserved, capacityReturns, writerAdmitted

vars == <<phase, rootPresent, groupPresent, guardianPresent, guardianStarted, lifetimeAttached,
          aliases, ofdLocked, groupObservation, guardianObservation,
          capacityReserved, capacityReturns, writerAdmitted>>

Init ==
  /\ phase = "idle"
  /\ rootPresent = FALSE
  /\ groupPresent = FALSE
  /\ guardianPresent = FALSE
  /\ guardianStarted = FALSE
  /\ lifetimeAttached = FALSE
  /\ aliases = {}
  /\ ofdLocked = FALSE
  /\ groupObservation = "absent"
  /\ guardianObservation = "absent"
  /\ capacityReserved = FALSE
  /\ capacityReturns = 0
  /\ writerAdmitted = FALSE

Reserve ==
  /\ phase = "idle"
  /\ phase' = "reserved"
  /\ capacityReserved' = TRUE
  /\ UNCHANGED <<guardianStarted, rootPresent, groupPresent, guardianPresent, lifetimeAttached,
                 aliases, ofdLocked, groupObservation, guardianObservation,
                 capacityReturns, writerAdmitted>>

SpawnFails ==
  /\ phase = "reserved"
  /\ phase' = "closed"
  /\ capacityReserved' = FALSE
  /\ capacityReturns' = capacityReturns + 1
  /\ UNCHANGED <<guardianStarted, rootPresent, groupPresent, guardianPresent, lifetimeAttached,
                 aliases, ofdLocked, groupObservation, guardianObservation,
                 writerAdmitted>>

Spawn ==
  /\ phase = "reserved"
  /\ phase' = "running"
  /\ rootPresent' = TRUE
  /\ groupPresent' = TRUE
  /\ aliases' = {"worker"}
  /\ ofdLocked' = TRUE
  /\ groupObservation' = "present"
  /\ UNCHANGED <<guardianStarted, guardianPresent, lifetimeAttached, guardianObservation,
                 capacityReserved, capacityReturns,
                 writerAdmitted>>

GuardianReady ==
  /\ phase = "running"
  /\ ~guardianPresent
  /\ ~guardianStarted
  /\ ~lifetimeAttached
  /\ "worker" \in aliases
  /\ guardianPresent' = TRUE
  /\ guardianStarted' = TRUE
  /\ guardianObservation' = "present"
  /\ aliases' = aliases \cup {"guardian"}
  /\ UNCHANGED <<phase, rootPresent, groupPresent, lifetimeAttached,
                 ofdLocked, groupObservation, capacityReserved,
                 capacityReturns, writerAdmitted>>

AttachLifetime ==
  /\ phase = "running"
  /\ guardianPresent
  /\ ~lifetimeAttached
  /\ lifetimeAttached' = TRUE
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, guardianPresent, aliases,
                 ofdLocked, groupObservation, guardianObservation,
                 capacityReserved, capacityReturns,
                 writerAdmitted>>

DropWorkerAlias ==
  /\ phase = "running"
  /\ lifetimeAttached
  /\ "worker" \in aliases
  /\ aliases' = aliases \ {"worker"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, guardianPresent,
                 lifetimeAttached, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted>>


RequestStop ==
  /\ phase = "running"
  /\ phase' = "stopping"
  /\ UNCHANGED <<guardianStarted, rootPresent, groupPresent, guardianPresent, lifetimeAttached,
                 aliases, ofdLocked, groupObservation, guardianObservation,
                 capacityReserved, capacityReturns, writerAdmitted>>

RootExits ==
  /\ phase \in {"running", "stopping"}
  /\ rootPresent
  /\ rootPresent' = FALSE
  /\ UNCHANGED <<guardianStarted, phase, groupPresent, guardianPresent, lifetimeAttached,
                 aliases, ofdLocked, groupObservation, guardianObservation,
                 capacityReserved, capacityReturns,
                 writerAdmitted>>

GuardianDetectsRootLoss ==
  /\ phase = "running"
  /\ guardianPresent
  /\ ~rootPresent
  /\ phase' = "stopping"
  /\ UNCHANGED <<guardianStarted, rootPresent, groupPresent, guardianPresent, lifetimeAttached,
                 aliases, ofdLocked, groupObservation, guardianObservation,
                 capacityReserved, capacityReturns, writerAdmitted>>

GuardianLoss ==
  /\ phase \in {"running", "stopping"}
  /\ guardianPresent
  /\ guardianPresent' = FALSE
  /\ guardianObservation' = "unknown"
  /\ aliases' = aliases \ {"guardian"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, lifetimeAttached,
                 groupObservation, capacityReserved, capacityReturns,
                 writerAdmitted>>

ObserveGuardianAbsent ==
  /\ ~guardianPresent
  /\ guardianObservation /= "absent"
  /\ guardianObservation' = "absent"
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, guardianPresent,
                 lifetimeAttached, aliases, ofdLocked, groupObservation,
                 capacityReserved, capacityReturns,
                 writerAdmitted>>

DiscoverGuardianLoss ==
  /\ phase = "running"
  /\ lifetimeAttached
  /\ guardianObservation = "absent"
  /\ phase' = "stopping"
  /\ UNCHANGED <<guardianStarted, rootPresent, groupPresent, guardianPresent, lifetimeAttached,
                 aliases, ofdLocked, groupObservation, guardianObservation,
                 capacityReserved, capacityReturns, writerAdmitted>>

CleanupGroup ==
  /\ phase = "stopping"
  /\ groupPresent
  /\ rootPresent' = FALSE
  /\ groupPresent' = FALSE
  /\ aliases' = aliases \ {"worker"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ groupObservation' = "unknown"
  /\ UNCHANGED <<guardianStarted, phase, guardianPresent, lifetimeAttached,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted>>

GuardianExitsAfterGroup ==
  /\ phase = "stopping"
  /\ ~groupPresent
  /\ guardianPresent
  /\ guardianPresent' = FALSE
  /\ guardianObservation' = "unknown"
  /\ aliases' = aliases \ {"guardian"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, lifetimeAttached,
                 groupObservation, capacityReserved, capacityReturns,
                 writerAdmitted>>

ObserveGroupAbsent ==
  /\ ~groupPresent
  /\ groupObservation /= "absent"
  /\ groupObservation' = "absent"
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, guardianPresent,
                 lifetimeAttached, aliases, ofdLocked, guardianObservation,
                 capacityReserved, capacityReturns,
                 writerAdmitted>>

ObserveStaleGroupAbsent ==
  /\ Mode \in {StaleClosure, DirectChildOnly}
  /\ phase = "stopping"
  /\ groupPresent
  /\ IF Mode = DirectChildOnly THEN ~rootPresent /\ ~lifetimeAttached ELSE TRUE
  /\ groupObservation' = "absent"
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, guardianPresent,
                 lifetimeAttached, aliases, ofdLocked, guardianObservation,
                 capacityReserved, capacityReturns,
                 writerAdmitted>>

Retire ==
  /\ phase = "stopping"
  /\ groupObservation = "absent"
  /\ guardianObservation = "absent"
  /\ capacityReserved
  /\ phase' = "closed"
  /\ capacityReserved' = FALSE
  /\ capacityReturns' = capacityReturns + 1
  /\ UNCHANGED <<guardianStarted, rootPresent, groupPresent, guardianPresent,
                 lifetimeAttached, aliases, ofdLocked, groupObservation,
                 guardianObservation, writerAdmitted>>

ExplicitUnlock ==
  /\ Mode = ExplicitUnlockLockOnly
  /\ "worker" \in aliases
  /\ "guardian" \in aliases
  /\ ofdLocked
  /\ ofdLocked' = FALSE
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, guardianPresent,
                 lifetimeAttached, aliases, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted>>

WriterAcquire ==
  /\ ~writerAdmitted
  /\ ~ofdLocked
  /\ IF Mode \in {HistoricalLockOnly, ExplicitUnlockLockOnly}
        THEN phase \in {"running", "stopping"}
        ELSE phase = "closed" /\ groupObservation = "absent"
                              /\ guardianObservation = "absent"
  /\ writerAdmitted' = TRUE
  /\ UNCHANGED <<guardianStarted, phase, rootPresent, groupPresent, guardianPresent,
                 lifetimeAttached, aliases, ofdLocked, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns>>

Next == Reserve \/ SpawnFails \/ Spawn \/ GuardianReady \/ AttachLifetime \/
        DropWorkerAlias \/ RequestStop \/ RootExits \/
        GuardianDetectsRootLoss \/ GuardianLoss \/ ObserveGuardianAbsent \/
        DiscoverGuardianLoss \/ CleanupGroup \/ GuardianExitsAfterGroup \/
        ObserveGroupAbsent \/ ObserveStaleGroupAbsent \/ Retire \/
        ExplicitUnlock \/ WriterAcquire

Spec == Init /\ [][Next]_vars

FairSpec == Spec
  /\ WF_vars(GuardianDetectsRootLoss)
  /\ WF_vars(ObserveGuardianAbsent)
  /\ WF_vars(DiscoverGuardianLoss)
  /\ WF_vars(CleanupGroup)
  /\ WF_vars(GuardianExitsAfterGroup)
  /\ WF_vars(ObserveGroupAbsent)
  /\ WF_vars(Retire)

TypeOK ==
  /\ phase \in Phases
  /\ rootPresent \in BOOLEAN
  /\ groupPresent \in BOOLEAN
  /\ guardianPresent \in BOOLEAN
  /\ guardianStarted \in BOOLEAN
  /\ lifetimeAttached \in BOOLEAN
  /\ aliases \subseteq Aliases
  /\ ofdLocked \in BOOLEAN
  /\ groupObservation \in Observations
  /\ guardianObservation \in Observations
  /\ capacityReserved \in BOOLEAN
  /\ capacityReturns \in Nat
  /\ writerAdmitted \in BOOLEAN

ReservationTracksOpenScope ==
  capacityReserved <=> phase \in {"reserved", "running", "stopping"}
NoEarlyCapacityReturn == groupPresent => capacityReserved
CapacityReturnsOnce == capacityReturns <= 1
ClosedReturnedCapacity == phase = "closed" => capacityReturns = 1
WriterExclusion == ~(writerAdmitted /\ groupPresent)
GuardianLossIsNotClosure == (~guardianPresent /\ groupPresent) => phase /= "closed"
GuardianPresentImpliesCapacity == guardianPresent => capacityReserved
GuardianAliasTracksProcess == ("guardian" \in aliases) <=> guardianPresent
LockTracksAliases == Mode = ExplicitUnlockLockOnly \/ (ofdLocked <=> aliases /= {})
RootBelongsToGroup == rootPresent => groupPresent

ConditionalCleanupProgress ==
  phase = "stopping" ~> ~capacityReserved
ConditionalGuardianLossProgress ==
  (phase = "running" /\ lifetimeAttached /\ ~guardianPresent) ~> phase = "stopping"
ConditionalRootLossProgress ==
  (phase = "running" /\ guardianPresent /\ ~rootPresent) ~>
    (phase = "stopping" \/ ~guardianPresent)

=============================================================================
