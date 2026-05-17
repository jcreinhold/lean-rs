import Lean

/-! Capability category: term elaboration and kernel checking driven from
    Rust through the [`LeanSession`] dispatch. Two `@[export]` shims plus
    the structures they exchange across the ABI. Structure layout is
    deliberately object-slot-only (every field is either `Nat`, `String`,
    a structure, an `Option`, an `Array`, or a nullary-only inductive) so
    the Rust side can decode every field through `take_ctor_objects`
    without reaching into the scalar tail — except for `severity` and
    `truncated`, which Lean's compiler unconditionally packs into the
    scalar tail; those two are read on the Rust side via
    `lean_ctor_get_uint8`. -/

namespace LeanRsFixture.Elaboration

open Lean

/-- Severity tag attached to each diagnostic. Nullary-only so it
    encodes as a `uint8_t` in the surrounding structure's scalar tail. -/
inductive Severity where
  | info
  | warning
  | error
  deriving Inhabited

/-- Marker for whether the host-side bound on diagnostic byte count was
    hit. Nullary-only for the same reason as [`Severity`]. -/
inductive Truncation where
  | complete
  | truncated
  deriving Inhabited

/-- Position attached to a diagnostic. `Nat` (rather than `UInt32`) keeps
    every field in an object slot so the Rust decoder uses one uniform
    `Nat → u32` helper. -/
structure DiagnosticPos where
  line      : Nat
  column    : Nat
  endLine   : Option Nat
  endColumn : Option Nat
  deriving Inhabited

/-- One diagnostic the elaborator emitted. -/
structure Diagnostic where
  severity  : Severity
  message   : String
  position  : Option DiagnosticPos
  fileLabel : String
  deriving Inhabited

/-- Failure payload returned by both shims when elaboration / kernel
    checking did not produce a value. -/
structure ElabFailure where
  diagnostics : Array Diagnostic
  truncated   : Truncation
  deriving Inhabited

/-- Opaque-to-Rust payload returned by [`hostKernelCheck`] on success.
    The contained `Declaration` survives across the FFI boundary as a
    `lean_object*`; Rust holds it as a [`LeanEvidence`] handle. -/
structure Evidence where
  decl : Declaration

/-- Sum of the four kernel-check outcomes. Each non-`Checked` branch
    carries the diagnostics the elaborator emitted before the
    classification. -/
inductive KernelOutcome where
  | checked     : Evidence    → KernelOutcome
  | rejected    : ElabFailure → KernelOutcome
  | unavailable : ElabFailure → KernelOutcome
  | unsupported : ElabFailure → KernelOutcome

/-- Build a one-message failure carrying `msg` as a free-form error.
    Used for diagnostics that have no Lean-level source position
    (parser failures, host-side classification, IO exceptions). -/
private def singleErrorFailure (msg : String) (fileLabel : String) : ElabFailure :=
  let diag : Diagnostic :=
    { severity := .error, message := msg, position := none, fileLabel }
  { diagnostics := #[diag], truncated := .complete }

/-- Map Lean's `MessageSeverity` to the Rust-facing `Severity` tag. -/
private def severityOfMessage : MessageSeverity → Severity
  | .information => .info
  | .warning     => .warning
  | .error       => .error

/-- Walk a `MessageLog`, converting each entry into a `Diagnostic` and
    bounding the running byte sum at `byteLimit`. Always emits at least
    one diagnostic if any are present so callers always see *some*
    failure context; subsequent diagnostics stop being collected once
    the cumulative body bytes meet the limit. -/
private def serializeMessages
    (msgs : MessageLog) (byteLimit : USize) (fallbackLabel : String)
    : BaseIO (Array Diagnostic × Truncation) := do
  let mut out : Array Diagnostic := #[]
  let mut bytes : USize := 0
  let mut trunc : Truncation := .complete
  for m in msgs.toArray do
    if out.size > 0 && bytes >= byteLimit then
      trunc := .truncated
      break
    let body ← m.data.toString
    let pos : Option DiagnosticPos := some {
      line      := m.pos.line
      column    := m.pos.column
      endLine   := m.endPos.map (·.line)
      endColumn := m.endPos.map (·.column)
    }
    let label := if m.fileName.isEmpty then fallbackLabel else m.fileName
    let diag : Diagnostic :=
      { severity := severityOfMessage m.severity, message := body, position := pos, fileLabel := label }
    out := out.push diag
    bytes := bytes + body.utf8ByteSize.toUSize
  pure (out, trunc)

/-- Construct a `Lean.Options` value carrying the heartbeat limit
    requested by the host. `heartbeats = 0` selects Lean's "no limit"
    semantics via [`Lean.maxHeartbeats`]. -/
private def buildOptions (heartbeats : UInt64) : Options :=
  Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat

/-- Parse and elaborate a single Lean term. The boundary is explicit:
    Rust passes the source text, namespace context, file label, and
    bounded options; Lean parses, elaborates, optionally checks against
    `expectedType`, and returns either the resulting `Expr` or an
    `ElabFailure` carrying typed diagnostics. -/
@[export lean_rs_host_elaborate]
def hostElaborate (env : Environment) (src : String) (expectedType : Option Expr)
    (ns : String) (fileLabel : String) (heartbeats : UInt64) (diagByteLimit : USize)
    : IO (Except ElabFailure Expr) := do
  let opts := buildOptions heartbeats
  let inputCtx := Parser.mkInputContext src fileLabel
  match Parser.runParserCategory env `term src fileLabel with
  | .error err =>
    return .error (singleErrorFailure err fileLabel)
  | .ok stx =>
    let cmdCtx : Elab.Command.Context := {
      fileName  := fileLabel
      fileMap   := inputCtx.fileMap
      snap?     := none
      cancelTk? := none
    }
    let initState := Elab.Command.mkState env {} opts
    let initState :=
      if ns.isEmpty then
        initState
      else
        let head := initState.scopes.headD { header := "", opts }
        { initState with scopes := [{ head with currNamespace := ns.toName }] }
    let elabAction : Elab.Command.CommandElabM (Option Expr) := do
      try
        let e ← Elab.Command.liftTermElabM <|
          Elab.Term.elabTermAndSynthesize stx expectedType
        return some e
      catch ex =>
        Elab.logException ex
        return none
    match (← EIO.toIO' <| (elabAction cmdCtx).run initState) with
    | .error _ =>
      return .error (singleErrorFailure "uncaught internal exception during elaboration" fileLabel)
    | .ok (result, finalState) =>
      let (diags, trunc) ← serializeMessages finalState.messages diagByteLimit fileLabel
      match result with
      | none => return .error { diagnostics := diags, truncated := trunc }
      | some e =>
        if finalState.messages.hasErrors then
          return .error { diagnostics := diags, truncated := trunc }
        return .ok e

/-- Parse, elaborate, and kernel-check a Lean declaration source.
    Drives the full `Lean.Elab.Frontend.process` pipeline, which runs
    `Command.elabCommand` followed by `Environment.addDecl` — the latter
    is what invokes the kernel. The outcome is classified into one of
    `Checked` / `Rejected` / `Unavailable` / `Unsupported` based on
    whether a new theorem/definition appeared in the resulting
    environment and whether the message log carries errors. -/
@[export lean_rs_host_kernel_check]
def hostKernelCheck (env : Environment) (src : String)
    (ns : String) (fileLabel : String) (heartbeats : UInt64) (diagByteLimit : USize)
    : IO KernelOutcome := do
  let opts := buildOptions heartbeats
  let prelude := if ns.isEmpty then "" else s!"namespace {ns}\n"
  let fullSrc := prelude ++ src
  try
    let result ← Lean.Elab.process fullSrc env opts (some fileLabel)
    let env' : Environment := result.1
    let msgs : MessageLog := result.2
    let (diags, trunc) ← serializeMessages msgs diagByteLimit fileLabel
    let failurePayload : ElabFailure := { diagnostics := diags, truncated := trunc }
    let newName? : Option Name := Id.run do
      for (n, _) in (Environment.constants env').toList do
        unless Environment.contains env n do
          return some n
      none
    -- Lean's elaborator recovers from errors by inserting `sorry` and
    -- still registering a `thmInfo` / `defnInfo` in the environment.
    -- Treat *any* error-severity diagnostic as `Rejected`, regardless
    -- of whether a new constant ended up in `env'`, so kernel-rejected
    -- proofs do not silently surface as `Checked`.
    if msgs.hasErrors then
      return .rejected failurePayload
    match newName? with
    | some n =>
      match Environment.find? env' n with
      | some (ConstantInfo.thmInfo  v) => return .checked { decl := Declaration.thmDecl v }
      | some (ConstantInfo.defnInfo v) => return .checked { decl := Declaration.defnDecl v }
      | _ => return .unsupported failurePayload
    | none =>
      return .unsupported failurePayload
  catch ex =>
    return .unavailable (singleErrorFailure (toString ex) fileLabel)

end LeanRsFixture.Elaboration
