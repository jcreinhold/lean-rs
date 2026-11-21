---------------------------- MODULE WorkerCustody ----------------------------
EXTENDS Naturals, TLC

CONSTANT Mode

HistoricalLockOnly == "historical_lock_only"
StaleClosure == "stale_closure"
ExplicitUnlockLockOnly == "explicit_unlock_lock_only"
DirectChildOnly == "direct_child_only"
PublishBeforeAdopt == "publish_before_adopt"
AckBeforeDurable == "ack_before_durable"
LoseAdoption == "lose_adoption"
CollectPreparingOnParentDeath == "collect_preparing_on_parent_death"
DropPreparingAfterPossibleSpawn == "drop_preparing_after_possible_spawn"
DirectExitReturnsTrackerCapacity == "direct_exit_returns_tracker_capacity"
ZombieReceiptBeforeCustodyClosed == "zombie_receipt_before_custody_closed"
ZombieWriterWithoutReceipt == "zombie_writer_without_receipt"

Phases == {"idle", "reserved", "running", "stopping", "closed"}
Observations == {"unknown", "present", "absent"}
Aliases == {"worker", "guardian"}

VARIABLES phase, supervisorAlive, rootPresent, groupPresent,
          guardianPresent, guardianStarted, identitiesAuthenticated,
          bootstrapIdentityDurable, bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
          aliases, ofdLocked, groupObservation, guardianObservation,
          capacityReserved, capacityReturns, protectedReadsEver, writerAdmitted,
          terminalGroup, zombieClosureDurable, successorReserved

vars == <<phase, supervisorAlive, rootPresent, groupPresent,
          guardianPresent, guardianStarted, identitiesAuthenticated,
          bootstrapIdentityDurable, bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
          aliases, ofdLocked, groupObservation, guardianObservation,
          capacityReserved, capacityReturns, protectedReadsEver, writerAdmitted,
          terminalGroup, zombieClosureDurable, successorReserved>>

Init ==
  /\ phase = "idle"
  /\ supervisorAlive = TRUE
  /\ rootPresent = FALSE
  /\ groupPresent = FALSE
  /\ guardianPresent = FALSE
  /\ guardianStarted = FALSE
  /\ identitiesAuthenticated = FALSE
  /\ bootstrapIdentityDurable = FALSE
  /\ bootstrapClosureDurable = FALSE
  /\ leaseAdopted = FALSE
  /\ runningDurable = FALSE
  /\ startupAcked = FALSE
  /\ aliases = {}
  /\ ofdLocked = FALSE
  /\ groupObservation = "absent"
  /\ guardianObservation = "absent"
  /\ capacityReserved = FALSE
  /\ capacityReturns = 0
  /\ protectedReadsEver = FALSE
  /\ writerAdmitted = FALSE
  /\ terminalGroup = "ordinary"
  /\ zombieClosureDurable = FALSE
  /\ successorReserved = FALSE

Reserve ==
  /\ phase = "idle"
  /\ phase' = "reserved"
  /\ capacityReserved' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

SpawnFails ==
  /\ phase = "reserved"
  /\ supervisorAlive
  /\ phase' = "closed"
  /\ capacityReserved' = FALSE
  /\ capacityReturns' = capacityReturns + 1
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

Spawn ==
  /\ phase = "reserved"
  /\ supervisorAlive
  /\ phase' = "running"
  /\ rootPresent' = TRUE
  /\ groupPresent' = TRUE
  /\ aliases' = {}
  /\ ofdLocked' = FALSE
  /\ groupObservation' = "present"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, guardianPresent, guardianStarted,
                 identitiesAuthenticated, leaseAdopted, runningDurable,
                 startupAcked, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

GuardianReady ==
  /\ phase = "running"
  /\ supervisorAlive
  /\ rootPresent
  /\ groupPresent
  /\ ~guardianStarted
  /\ guardianPresent' = TRUE
  /\ guardianStarted' = TRUE
  /\ guardianObservation' = "present"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 identitiesAuthenticated, leaseAdopted, runningDurable,
                 startupAcked, aliases, ofdLocked, groupObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

AuthenticateIdentities ==
  /\ phase = "running"
  /\ supervisorAlive
  /\ rootPresent
  /\ groupPresent
  /\ guardianPresent
  /\ ~identitiesAuthenticated
  /\ identitiesAuthenticated' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

PublishBootstrapIdentity ==
  /\ phase = "running"
  /\ supervisorAlive
  /\ rootPresent
  /\ groupPresent
  /\ guardianPresent
  /\ identitiesAuthenticated
  /\ ~bootstrapIdentityDurable
  /\ bootstrapIdentityDurable' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapClosureDurable, phase, supervisorAlive,
                 rootPresent, groupPresent, guardianPresent, guardianStarted,
                 identitiesAuthenticated, leaseAdopted, runningDurable,
                 startupAcked, aliases, ofdLocked, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

AdoptLease ==
  /\ phase = "running"
  /\ supervisorAlive
  /\ rootPresent
  /\ groupPresent
  /\ guardianPresent
  /\ bootstrapIdentityDurable
  /\ ~leaseAdopted
  /\ leaseAdopted' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

PublishRunning ==
  /\ phase = "running"
  /\ supervisorAlive
  /\ rootPresent
  /\ groupPresent
  /\ guardianPresent
  /\ leaseAdopted
  /\ ~runningDurable
  /\ runningDurable' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

AcknowledgeStartup ==
  /\ phase = "running"
  /\ supervisorAlive
  /\ rootPresent
  /\ guardianPresent
  /\ runningDurable
  /\ ~startupAcked
  /\ startupAcked' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

MutantPublishBeforeAdopt ==
  /\ Mode = PublishBeforeAdopt
  /\ phase = "running"
  /\ identitiesAuthenticated
  /\ ~leaseAdopted
  /\ runningDurable' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

MutantAckBeforeDurable ==
  /\ Mode = AckBeforeDurable
  /\ phase = "running"
  /\ leaseAdopted
  /\ ~runningDurable
  /\ startupAcked' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

MutantLoseAdoption ==
  /\ Mode = LoseAdoption
  /\ phase = "running"
  /\ runningDurable
  /\ leaseAdopted
  /\ leaseAdopted' = FALSE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

MutantCollectPreparing ==
  /\ Mode = CollectPreparingOnParentDeath
  /\ phase = "stopping"
  /\ ~supervisorAlive
  /\ ~runningDurable
  /\ groupPresent
  /\ capacityReserved
  /\ phase' = "closed"
  /\ capacityReserved' = FALSE
  /\ capacityReturns' = capacityReturns + 1
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

\* The accept-error Drop path retires Preparing after authenticated HELLO,
\* although the actual payload and guardian still exist. This configuration
\* projects capacityReserved to the durable registry reservation.
MutantDropPreparingAfterPossibleSpawn ==
  /\ Mode = DropPreparingAfterPossibleSpawn
  /\ phase = "running"
  /\ supervisorAlive
  /\ identitiesAuthenticated
  /\ ~runningDurable
  /\ ~leaseAdopted
  /\ groupPresent
  /\ guardianPresent
  /\ capacityReserved
  /\ phase' = "closed"
  /\ capacityReserved' = FALSE
  /\ capacityReturns' = capacityReturns + 1
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable,
                 supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

\* A direct-child wait without an attached lease returns the tracker slot
\* while guardian custody remains open. The payload is already absent here,
\* so this is distinct from the registry/payload counterexample above.
MutantDirectExitReturnsTrackerCapacity ==
  /\ Mode = DirectExitReturnsTrackerCapacity
  /\ phase \in {"running", "stopping"}
  /\ supervisorAlive
  /\ identitiesAuthenticated
  /\ ~leaseAdopted
  /\ ~rootPresent
  /\ ~groupPresent
  /\ guardianPresent
  /\ capacityReserved
  /\ phase' = "closed"
  /\ capacityReserved' = FALSE
  /\ capacityReturns' = capacityReturns + 1
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable,
                 supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

AcquireReads ==
  /\ phase = "running"
  /\ startupAcked
  /\ rootPresent
  /\ groupPresent
  /\ guardianPresent
  /\ aliases = {}
  /\ aliases' = {"worker", "guardian"}
  /\ ofdLocked' = TRUE
  /\ protectedReadsEver' = TRUE
  /\ UNCHANGED <<bootstrapIdentityDurable, bootstrapClosureDurable, phase,
                 supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

DropWorkerAlias ==
  /\ phase = "running"
  /\ startupAcked
  /\ "worker" \in aliases
  /\ aliases' = aliases \ {"worker"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

RequestStop ==
  /\ phase = "running"
  /\ phase' = "stopping"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

SupervisorCrash ==
  /\ phase \in {"reserved", "running", "stopping"}
  /\ supervisorAlive
  /\ supervisorAlive' = FALSE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, rootPresent, groupPresent, guardianPresent, guardianStarted,
                 identitiesAuthenticated, leaseAdopted, runningDurable,
                 startupAcked, aliases, ofdLocked, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

RootExits ==
  /\ phase \in {"running", "stopping"}
  /\ rootPresent
  /\ rootPresent' = FALSE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

GroupExits ==
  /\ phase \in {"running", "stopping"}
  /\ groupPresent
  /\ rootPresent' = FALSE
  /\ groupPresent' = FALSE
  /\ terminalGroup' = "ordinary"
  /\ aliases' = aliases \ {"worker"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ groupObservation' = "unknown"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase,
                 supervisorAlive, guardianPresent, guardianStarted,
                 identitiesAuthenticated, leaseAdopted, runningDurable,
                 startupAcked, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, zombieClosureDurable, successorReserved>>

GuardianDetectsOwnerLoss ==
  /\ phase = "running"
  /\ guardianPresent
  /\ (~supervisorAlive \/ ~rootPresent)
  /\ phase' = "stopping"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

GuardianLoss ==
  /\ phase \in {"running", "stopping"}
  /\ guardianPresent
  /\ guardianPresent' = FALSE
  /\ guardianObservation' = "unknown"
  /\ aliases' = aliases \ {"guardian"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, groupObservation,
                 capacityReserved, capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

ObserveGuardianAbsent ==
  /\ ~guardianPresent
  /\ (supervisorAlive \/ runningDurable \/ bootstrapIdentityDurable \/ bootstrapClosureDurable)
  /\ guardianObservation /= "absent"
  /\ guardianObservation' = "absent"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

DiscoverGuardianLoss ==
  /\ phase = "running"
  /\ supervisorAlive
  /\ leaseAdopted
  /\ guardianObservation = "absent"
  /\ phase' = "stopping"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

RecoverClosedScope ==
  /\ phase = "running"
  /\ ~supervisorAlive
  /\ runningDurable
  /\ groupObservation = "absent"
  /\ guardianObservation = "absent"
  /\ phase' = "stopping"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable,
                 supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

CleanupAuthority == guardianPresent \/ (supervisorAlive /\ leaseAdopted)

CleanupGroup ==
  /\ phase = "stopping"
  /\ CleanupAuthority
  /\ groupPresent
  /\ rootPresent' = FALSE
  /\ groupPresent' = FALSE
  /\ terminalGroup' = "ordinary"
  /\ aliases' = aliases \ {"worker"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ groupObservation' = "unknown"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, guardianPresent, guardianStarted,
                 identitiesAuthenticated, leaseAdopted, runningDurable,
                 startupAcked, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, zombieClosureDurable, successorReserved>>

\* A zombie leader may coexist with a still-capable peer.
TerminalLeaderWithPeer ==
  /\ phase \in {"running", "stopping"}
  /\ rootPresent /\ groupPresent /\ guardianPresent
  /\ rootPresent' = FALSE
  /\ terminalGroup' = "leader_and_peer"
  /\ UNCHANGED <<phase, supervisorAlive, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, bootstrapIdentityDurable,
                 bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
                 aliases, ofdLocked, groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, protectedReadsEver, writerAdmitted, zombieClosureDurable, successorReserved>>

\* Successful guardian drain can leave the exact unreaped terminal leader.
CleanupToUnreapedLeader ==
  /\ phase = "stopping"
  /\ guardianPresent /\ runningDurable /\ groupPresent
  /\ terminalGroup # "leader_only"
  /\ rootPresent' = FALSE
  /\ terminalGroup' = "leader_only"
  /\ aliases' = aliases \ {"worker"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ UNCHANGED <<phase, supervisorAlive, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, bootstrapIdentityDurable,
                 bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
                 groupObservation, guardianObservation, capacityReserved, capacityReturns,
                 protectedReadsEver, writerAdmitted, zombieClosureDurable, successorReserved>>

PublishZombieClosure ==
  /\ phase = "stopping"
  /\ guardianPresent /\ startupAcked /\ terminalGroup = "leader_only"
  /\ ~zombieClosureDurable
  /\ zombieClosureDurable' = TRUE
  /\ UNCHANGED <<phase, supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, bootstrapIdentityDurable,
                 bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
                 aliases, ofdLocked, groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, protectedReadsEver, writerAdmitted, terminalGroup, successorReserved>>

PublishBootstrapClosure ==
  /\ phase = "stopping"
  /\ ~runningDurable
  /\ (guardianPresent \/ (supervisorAlive /\ leaseAdopted))
  /\ ~groupPresent
  /\ ~bootstrapClosureDurable
  /\ bootstrapClosureDurable' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, phase, supervisorAlive,
                 rootPresent, groupPresent, guardianPresent, guardianStarted,
                 identitiesAuthenticated, leaseAdopted, runningDurable,
                 startupAcked, aliases, ofdLocked, groupObservation,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

GuardianExitsAfterGroup ==
  /\ phase = "stopping"
  /\ (~groupPresent \/ zombieClosureDurable)
  /\ guardianPresent
  /\ (runningDurable \/ bootstrapClosureDurable)
  /\ guardianPresent' = FALSE
  /\ guardianObservation' = "unknown"
  /\ aliases' = aliases \ {"guardian"}
  /\ ofdLocked' = IF aliases' = {} THEN FALSE ELSE ofdLocked
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, groupObservation,
                 capacityReserved, capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

ObserveGroupAbsent ==
  /\ ~groupPresent
  /\ groupObservation /= "absent"
  /\ groupObservation' = "absent"
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

ObserveStaleGroupAbsent ==
  /\ Mode \in {StaleClosure, DirectChildOnly}
  /\ phase = "stopping"
  /\ groupPresent
  /\ IF Mode = DirectChildOnly THEN ~rootPresent /\ ~leaseAdopted ELSE TRUE
  /\ groupObservation' = "absent"
  /\ bootstrapClosureDurable' = IF Mode = DirectChildOnly THEN TRUE ELSE bootstrapClosureDurable
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 guardianObservation, capacityReserved, capacityReturns,
                 writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

Retire ==
  /\ phase = "stopping"
  /\ guardianObservation = "absent"
  /\ ((runningDurable /\ (groupObservation = "absent" \/ zombieClosureDurable)) \/
      (~runningDurable /\ bootstrapClosureDurable))
  /\ capacityReserved
  /\ phase' = "closed"
  /\ capacityReserved' = FALSE
  /\ capacityReturns' = capacityReturns + 1
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, leaseAdopted,
                 runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

ExplicitUnlock ==
  /\ Mode = ExplicitUnlockLockOnly
  /\ "worker" \in aliases
  /\ "guardian" \in aliases
  /\ ofdLocked
  /\ ofdLocked' = FALSE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, writerAdmitted, terminalGroup, zombieClosureDurable, successorReserved>>

WriterAcquire ==
  /\ ~writerAdmitted
  /\ ~ofdLocked
  /\ IF Mode \in {HistoricalLockOnly, ExplicitUnlockLockOnly}
        THEN phase \in {"running", "stopping"} /\ startupAcked /\ guardianStarted /\ protectedReadsEver
        ELSE phase = "closed" /\ (groupObservation = "absent" \/ zombieClosureDurable)
                              /\ guardianObservation = "absent"
  /\ writerAdmitted' = TRUE
  /\ UNCHANGED <<protectedReadsEver, bootstrapIdentityDurable, bootstrapClosureDurable, phase, supervisorAlive, rootPresent, groupPresent,
                 guardianPresent, guardianStarted, identitiesAuthenticated,
                 leaseAdopted, runningDurable, startupAcked, aliases, ofdLocked,
                 groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, terminalGroup, zombieClosureDurable, successorReserved>>

ReserveSuccessor ==
  /\ phase = "closed"
  /\ ~successorReserved
  /\ successorReserved' = TRUE
  /\ UNCHANGED <<phase, supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, bootstrapIdentityDurable,
                 bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
                 aliases, ofdLocked, groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, protectedReadsEver, writerAdmitted, terminalGroup, zombieClosureDurable>>

MutantReserveBeforeGuardianClosure ==
  /\ Mode = ZombieReceiptBeforeCustodyClosed
  /\ zombieClosureDurable /\ groupPresent /\ guardianPresent
  /\ ~successorReserved
  /\ successorReserved' = TRUE
  /\ UNCHANGED <<phase, supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, bootstrapIdentityDurable,
                 bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
                 aliases, ofdLocked, groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, protectedReadsEver, writerAdmitted, terminalGroup, zombieClosureDurable>>

MutantWriterWithoutZombieReceipt ==
  /\ Mode = ZombieWriterWithoutReceipt
  /\ terminalGroup = "leader_only" /\ ~zombieClosureDurable
  /\ ~guardianPresent /\ ~ofdLocked /\ ~writerAdmitted
  /\ writerAdmitted' = TRUE
  /\ UNCHANGED <<phase, supervisorAlive, rootPresent, groupPresent, guardianPresent,
                 guardianStarted, identitiesAuthenticated, bootstrapIdentityDurable,
                 bootstrapClosureDurable, leaseAdopted, runningDurable, startupAcked,
                 aliases, ofdLocked, groupObservation, guardianObservation, capacityReserved,
                 capacityReturns, protectedReadsEver, terminalGroup, zombieClosureDurable, successorReserved>>

Next == Reserve \/ SpawnFails \/ Spawn \/ GuardianReady \/
        AuthenticateIdentities \/ PublishBootstrapIdentity \/ AdoptLease \/ PublishRunning \/
        AcknowledgeStartup \/ MutantPublishBeforeAdopt \/
        MutantAckBeforeDurable \/ MutantLoseAdoption \/
        MutantCollectPreparing \/ MutantDropPreparingAfterPossibleSpawn \/
        MutantDirectExitReturnsTrackerCapacity \/ AcquireReads \/ DropWorkerAlias \/
        RequestStop \/ SupervisorCrash \/ RootExits \/ GroupExits \/ TerminalLeaderWithPeer \/
        GuardianDetectsOwnerLoss \/ GuardianLoss \/ ObserveGuardianAbsent \/
        DiscoverGuardianLoss \/ RecoverClosedScope \/ CleanupGroup \/ CleanupToUnreapedLeader \/ PublishZombieClosure \/ PublishBootstrapClosure \/ GuardianExitsAfterGroup \/
        ObserveGroupAbsent \/ ObserveStaleGroupAbsent \/ Retire \/
        ExplicitUnlock \/ WriterAcquire \/ ReserveSuccessor \/
        MutantReserveBeforeGuardianClosure \/ MutantWriterWithoutZombieReceipt

Spec == Init /\ [][Next]_vars

FairSpec == Spec
  /\ WF_vars(GuardianDetectsOwnerLoss)
  /\ WF_vars(ObserveGuardianAbsent)
  /\ WF_vars(DiscoverGuardianLoss)
  /\ WF_vars(RecoverClosedScope)
  /\ WF_vars(CleanupGroup)
  /\ WF_vars(PublishBootstrapClosure)
  /\ WF_vars(PublishZombieClosure)
  /\ WF_vars(GuardianExitsAfterGroup)
  /\ WF_vars(ObserveGroupAbsent)
  /\ WF_vars(Retire)

TypeOK ==
  /\ phase \in Phases
  /\ supervisorAlive \in BOOLEAN
  /\ rootPresent \in BOOLEAN
  /\ groupPresent \in BOOLEAN
  /\ guardianPresent \in BOOLEAN
  /\ guardianStarted \in BOOLEAN
  /\ identitiesAuthenticated \in BOOLEAN
  /\ bootstrapIdentityDurable \in BOOLEAN
  /\ bootstrapClosureDurable \in BOOLEAN
  /\ leaseAdopted \in BOOLEAN
  /\ runningDurable \in BOOLEAN
  /\ startupAcked \in BOOLEAN
  /\ aliases \subseteq Aliases
  /\ ofdLocked \in BOOLEAN
  /\ groupObservation \in Observations
  /\ guardianObservation \in Observations
  /\ capacityReserved \in BOOLEAN
  /\ capacityReturns \in Nat
  /\ protectedReadsEver \in BOOLEAN
  /\ writerAdmitted \in BOOLEAN
  /\ terminalGroup \in {"ordinary", "leader_and_peer", "leader_only"}
  /\ zombieClosureDurable \in BOOLEAN
  /\ successorReserved \in BOOLEAN

ReservationTracksOpenScope ==
  capacityReserved <=> phase \in {"reserved", "running", "stopping"}
PotentialPayloadWriter == groupPresent /\ terminalGroup # "leader_only"
NoEarlyCapacityReturn == PotentialPayloadWriter => capacityReserved
CapacityReturnsOnce == capacityReturns <= 1
ClosedReturnedCapacity == phase = "closed" => capacityReturns = 1
WriterExclusion == ~(writerAdmitted /\ PotentialPayloadWriter)
GuardianLossIsNotClosure == (~guardianPresent /\ PotentialPayloadWriter) => phase /= "closed"
ZombieEvidenceIsSound == zombieClosureDurable => ~PotentialPayloadWriter
ZombieAdmissionFenced ==
  (groupPresent /\ (writerAdmitted \/ successorReserved \/ ~capacityReserved)) =>
    terminalGroup = "leader_only" /\ zombieClosureDurable
      /\ ~guardianPresent /\ guardianObservation = "absent" /\ ~ofdLocked
GuardianPresentImpliesCapacity == guardianPresent => capacityReserved
GuardianAliasHasProcess == ("guardian" \in aliases) => guardianPresent
LockTracksAliases == Mode = ExplicitUnlockLockOnly \/ (ofdLocked <=> aliases /= {})
RootBelongsToGroup == rootPresent => groupPresent
BootstrapClosureIsSound == bootstrapClosureDurable => ~groupPresent
BootstrapIdentityIsAuthenticated == bootstrapIdentityDurable => identitiesAuthenticated
AuthenticationPrecedesAdoption == leaseAdopted => bootstrapIdentityDurable
AdoptionPrecedesRunning == runningDurable => leaseAdopted
RunningPrecedesAck == startupAcked => runningDurable
AckRequiresGuardianStarted == startupAcked => guardianStarted
AckKeepsOpenCustodyReserved == (startupAcked /\ phase /= "closed") => capacityReserved

PhysicalAbsenceProgress ==
  (phase = "stopping" /\ ~groupPresent /\ ~guardianPresent /\
   (runningDurable \/ bootstrapClosureDurable)) ~> phase = "closed"
ConditionalCleanupProgress ==
  (phase = "stopping" /\ CleanupAuthority) ~>
    (phase = "closed" \/ ~CleanupAuthority)
ConditionalGuardianLossProgress ==
  (phase = "running" /\ supervisorAlive /\ leaseAdopted /\ ~guardianPresent) ~>
    (phase = "stopping" \/ ~supervisorAlive)
ConditionalSupervisorLossProgress ==
  (phase = "stopping" /\ ~supervisorAlive /\ guardianPresent) ~>
    (phase = "closed" \/ ~guardianPresent)

=============================================================================
