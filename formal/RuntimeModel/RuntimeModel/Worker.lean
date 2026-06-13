import RuntimeModel.Basic

/-!
# Worker transition-system skeleton

This file models one supervised child process abstractly. It records enough
state to state and prove terminal outcome uniqueness and generation separation
without mentioning Rust channels, OS processes, or Lean elaboration internals.
-/

namespace RuntimeModel

/-- Abstract phase of one supervised worker. -/
inductive WorkerPhase where
  | absent
  | starting
  | idle (generation : Generation)
  | busy (generation : Generation) (request : RequestId)
  | streaming (generation : Generation) (request : RequestId)
  | stopping (generation : Generation)
  | killing (generation : Generation)
  | reaping (generation : Generation)
  | crashed (generation : Generation)
  | restartExhausted
deriving DecidableEq, Repr

/-- Abstract worker state with a functional terminal-outcome ledger. -/
structure WorkerState where
  phase : WorkerPhase
  terminalOutcome : Generation -> RequestId -> Option Outcome

/-- Initial worker state before a child exists. -/
def WorkerState.initial : WorkerState where
  phase := .absent
  terminalOutcome := fun _ _ => none

/-- Record one terminal outcome, leaving all other request slots unchanged. -/
def WorkerState.recordTerminal
    (state : WorkerState) (generation : Generation) (request : RequestId)
    (outcome : Outcome) : WorkerState where
  phase := state.phase
  terminalOutcome := fun g r =>
    if g = generation then
      if r = request then
        some outcome
      else
        state.terminalOutcome g r
    else
      state.terminalOutcome g r

/-- A request has a parent-visible terminal outcome in a state. -/
def HasTerminalOutcome
    (state : WorkerState) (generation : Generation) (request : RequestId)
    (outcome : Outcome) : Prop :=
  state.terminalOutcome generation request = some outcome

/-- Terminal outcomes are unique for each admitted generation/request pair. -/
theorem terminal_outcome_unique
    {state : WorkerState} {generation : Generation} {request : RequestId}
    {left right : Outcome}
    (hleft : HasTerminalOutcome state generation request left)
    (hright : HasTerminalOutcome state generation request right) :
    left = right := by
  unfold HasTerminalOutcome at hleft hright
  rw [hleft] at hright
  injection hright

/-- One abstract worker transition. -/
inductive WorkerStep : WorkerState -> Event -> WorkerState -> Prop where
  | spawn {state : WorkerState} {generation : Generation} :
      state.phase = .absent ->
      WorkerStep state (.spawn generation)
        { state with phase := .idle generation }
  | accept {state : WorkerState} {generation : Generation} {request : RequestId} :
      state.phase = .idle generation ->
      WorkerStep state (.accept generation request)
        { state with phase := .busy generation request }
  | row {state : WorkerState} {generation : Generation} {request : RequestId} :
      state.phase = .busy generation request ∨ state.phase = .streaming generation request ->
      WorkerStep state (.row generation request)
        { state with phase := .streaming generation request }
  | terminal {state : WorkerState} {generation : Generation} {request : RequestId} {outcome : Outcome} :
      state.terminalOutcome generation request = none ->
      (state.phase = .busy generation request ∨ state.phase = .streaming generation request) ->
      WorkerStep state (.terminal generation request outcome)
        { state.recordTerminal generation request outcome with phase := .idle generation }
  | shutdown {state : WorkerState} {generation : Generation} :
      state.phase = .idle generation ->
      WorkerStep state (.shutdownStart generation)
        { state with phase := .stopping generation }
  | kill {state : WorkerState} {generation : Generation} :
      WorkerStep state (.killSent generation)
        { state with phase := .killing generation }
  | reap {state : WorkerState} {generation : Generation} :
      state.phase = .stopping generation ∨
        state.phase = .killing generation ∨
        state.phase = .crashed generation ->
      WorkerStep state (.reaped generation)
        { state with phase := .reaping generation }
  | restartAdmitted {state : WorkerState} {fromGeneration toGeneration : Generation} :
      state.phase = .reaping fromGeneration ->
      WorkerStep state (.restartAdmitted fromGeneration toGeneration)
        { state with phase := .idle toGeneration }
  | restartRefused {state : WorkerState} {generation : Generation} :
      state.phase = .reaping generation ->
      WorkerStep state (.restartRefused generation)
        { state with phase := .restartExhausted }

/-- Reflexive-transitive worker traces, retaining only the final state. -/
inductive WorkerTrace : WorkerState -> List Event -> WorkerState -> Prop where
  | nil {state : WorkerState} :
      WorkerTrace state [] state
  | cons {start mid finish : WorkerState} {event : Event} {events : List Event} :
      WorkerStep start event mid ->
      WorkerTrace mid events finish ->
      WorkerTrace start (event :: events) finish

end RuntimeModel
