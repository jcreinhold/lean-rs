import Lean
import LeanRsHostShims.Elaboration

/-! Capability category: bounded `MetaM` services. Four `@[export]`
    shims run pre-registered MetaM actions (`Meta.inferType`,
    `Meta.whnf`, `Meta.isDefEq`, and a deliberate heartbeat-burner)
    under a heartbeat ceiling + diagnostic byte budget + reducibility
    setting threaded from Rust. Each shim returns a four-variant
    `MetaResponse` that
    classifies the outcome as `ok` / `failed` / `timeoutOrHeartbeat` /
    `unsupported`; Rust mirrors the inductive with a typed payload sum.

    The Lean side owns all `MetaM` plumbing; Rust never constructs or
    schedules a MetaM program. The closed set of services is fixed by
    the four `@[export]`s here plus the matching Rust constants. -/

namespace LeanRsFixture.Meta

open Lean Meta Core
open LeanRsFixture.Elaboration (ElabFailure singleErrorFailure)

/-- Four-variant outcome returned by every meta-service shim.

    Mirrors the four `MetaCallStatus` cases on the Rust side:
    * `ok payload`             — the MetaM action returned a value;
    * `failed`                 — Lean exception that is **not** a heartbeat /
                                 system-resource exhaustion (type error,
                                 unbound metavar, etc.); carries an
                                 `ElabFailure` with one error-severity
                                 diagnostic;
    * `timeoutOrHeartbeat`     — `Exception.isMaxHeartbeat` matched on the
                                 caught exception, i.e. the heartbeat
                                 ceiling tripped before the action could
                                 finish;
    * `unsupported`            — reserved for a service that classifies its
                                 input as out-of-domain. The landed
                                 services never produce this; the Rust
                                 dispatcher also synthesises an
                                 `Unsupported` response when a service's
                                 symbol is absent from the loaded
                                 capability. -/
inductive MetaResponse (α : Type) where
  | ok                 : α           → MetaResponse α
  | failed             : ElabFailure → MetaResponse α
  | timeoutOrHeartbeat : ElabFailure → MetaResponse α
  | unsupported        : ElabFailure → MetaResponse α

/-- Map a Rust-side `LeanMetaTransparency` byte to Lean's
    `Meta.TransparencyMode`. Stable encoding (Rust enum declaration
    order):
      * `0 → default`   (Lean's standard reducibility)
      * `1 → reducible` (`@[reducible]` only)
      * `2 → instances` (default + instance-binding bodies)
      * `3 → all`       (every definition unfolds)
    Any other byte falls back to `default` so the Rust side cannot
    drive the Lean side into an unrepresentable state by accident. -/
private def transparencyOfByte : UInt8 → TransparencyMode
  | 0 => .default
  | 1 => .reducible
  | 2 => .instances
  | 3 => .all
  | _ => .default

/-- Run a `MetaM α` action under a heartbeat ceiling and reducibility
    setting, classifying the outcome into a `MetaResponse α`. The
    `Core.Context` / `Core.State` are constructed minimally — no parser
    cache, no info-state, no name-generator seed — since meta-services
    do not elaborate source.

    `diagBytes` is threaded through the C-ABI signature so the Rust
    `LeanMetaOptions` shape stays uniform with the prompt-15
    `LeanElabOptions`; the single-message failure branches do not
    actively truncate (Rust's `LeanDiagnostic` decoder already enforces
    `LEAN_ERROR_MESSAGE_LIMIT` on the way back). Multi-message services
    added later can use the budget to bound their cumulative byte sum
    the way [`LeanRsFixture.Elaboration.serializeMessages`] does. -/
private def runMetaBounded {α : Type}
    (env : Environment) (heartbeats : UInt64) (_diagBytes : USize)
    (transparency : UInt8) (action : MetaM α) : IO (MetaResponse α) := do
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let coreCtx : Core.Context := { fileName := "<meta>", fileMap := default, options := opts }
  let coreState : Core.State := { env }
  let tmode := transparencyOfByte transparency
  let metaAction : MetaM α := Meta.withTransparency tmode action
  let coreAction : CoreM α := metaAction.run' {} {}
  let eio : EIO Exception α := (coreAction coreCtx).run' coreState
  match ← eio.toBaseIO with
  | .ok value => pure (.ok value)
  | .error ex =>
    let raw ← ex.toMessageData.toString
    let failure : ElabFailure := singleErrorFailure raw "<meta>"
    if ex.isMaxHeartbeat then
      pure (.timeoutOrHeartbeat failure)
    else
      pure (.failed failure)

/-- Service: infer the type of an `Expr`. -/
@[export lean_rs_host_meta_infer_type]
def metaInferType (env : Environment) (expr : Expr)
    (heartbeats : UInt64) (diagBytes : USize) (transparency : UInt8)
    : IO (MetaResponse Expr) :=
  runMetaBounded env heartbeats diagBytes transparency (Meta.inferType expr)

/-- Service: weak-head normalise an `Expr`. -/
@[export lean_rs_host_meta_whnf]
def metaWhnf (env : Environment) (expr : Expr)
    (heartbeats : UInt64) (diagBytes : USize) (transparency : UInt8)
    : IO (MetaResponse Expr) :=
  runMetaBounded env heartbeats diagBytes transparency (Meta.whnf expr)

/-- Service: decide whether two `Expr`s are definitionally equal.

    The request is a Lean product `(lhs, rhs, transparency)`. The final
    `UInt8` parameter is still present because Rust dispatches every
    registered meta service through the same `(env, request, heartbeats,
    diagBytes, optionsTransparency)` shape; this service uses the
    request-local transparency so callers can compare several pairs
    under different modes without rebuilding the option bundle. -/
@[export lean_rs_host_meta_is_def_eq]
def metaIsDefEq (env : Environment) (request : Expr × Expr × UInt8)
    (heartbeats : UInt64) (diagBytes : USize) (_optionsTransparency : UInt8)
    : IO (MetaResponse Bool) :=
  let lhs := request.1
  let rhs := request.2.1
  let transparency := request.2.2
  runMetaBounded env heartbeats diagBytes transparency (Meta.isDefEq lhs rhs)

/-- Non-terminating recursion guarded only by the heartbeat check.
    `partial` because it does not reduce structurally — termination
    relies on the heartbeat exception, which is the point. -/
private partial def burnLoop : MetaM Expr := do
  Core.checkMaxHeartbeats "lean_rs_host_meta_heartbeat_burn"
  burnLoop

/-- Diagnostic burner: a recursion that consumes a heartbeat per step
    via `Core.checkMaxHeartbeats`. Any nonzero heartbeat budget below
    the recursion bound trips `Exception.isMaxHeartbeat`, surfacing as
    `MetaResponse.timeoutOrHeartbeat`. The `Expr` argument is ignored —
    the shape exists so the service shares the typed signature used by
    `metaInferType` / `metaWhnf`, keeping the Rust `LeanMetaService`
    surface uniform.

    Pragmatically: when heartbeat = 1 the very first
    `checkMaxHeartbeats` call throws, so this test runs in well under a
    millisecond. -/
@[export lean_rs_host_meta_heartbeat_burn]
def metaHeartbeatBurn (env : Environment) (_expr : Expr)
    (heartbeats : UInt64) (diagBytes : USize) (transparency : UInt8)
    : IO (MetaResponse Expr) :=
  runMetaBounded env heartbeats diagBytes transparency burnLoop

/-- Service: pretty-print an `Expr` via `Lean.PrettyPrinter.ppExpr` and
    return its rendered string form. Slow relative to
    `lean_rs_host_env_expr_to_string_raw` — `ppExpr` consults the
    environment for notation, unfolding hints, and binder choices —
    but produces the form a Lean user reads. Heartbeat-bounded like
    every other meta service, so a deeply nested term under a tiny
    budget surfaces as `MetaResponse.timeoutOrHeartbeat`. -/
@[export lean_rs_host_meta_pp_expr]
def metaPpExpr (env : Environment) (expr : Expr)
    (heartbeats : UInt64) (diagBytes : USize) (transparency : UInt8)
    : IO (MetaResponse String) :=
  runMetaBounded env heartbeats diagBytes transparency do
    let fmt ← Lean.PrettyPrinter.ppExpr expr
    return fmt.pretty

end LeanRsFixture.Meta
