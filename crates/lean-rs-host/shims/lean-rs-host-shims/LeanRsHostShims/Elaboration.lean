import Lean
import LeanRsInterop.Callback

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

private def reportProgress? (handle trampoline : USize) (current total : Nat) : IO (Option UInt8) := do
  let status ← LeanRsInterop.Callback.Tick.call handle trampoline (UInt64.ofNat current) (UInt64.ofNat total)
  if status == 0 then
    pure none
  else
    pure (some status)

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
    (parser failures, host-side classification, IO exceptions).
    Also reused by `LeanRsFixture.Meta` for the failure / heartbeat /
    unsupported branches of `MetaResponse`. -/
def singleErrorFailure (msg : String) (fileLabel : String) : ElabFailure :=
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

/-- Bulk variant of [`hostElaborate`]: a single IO traversal that folds the
    singular elaboration across `sources`. The boundary stays explicit —
    each source is parsed and elaborated independently against the shared
    environment and bounded options — but the FFI crossing, options
    allocation, and heartbeat counter are paid once per batch instead of
    once per source. Iteration semantics are identical to a Rust-side
    fold over `hostElaborate` with `expectedType := none`. -/
@[export lean_rs_host_elaborate_bulk]
def hostElaborateBulk (env : Environment) (sources : Array String)
    (ns : String) (fileLabel : String) (heartbeats : UInt64) (diagByteLimit : USize)
    : IO (Array (Except ElabFailure Expr)) := do
  sources.mapM fun src =>
    hostElaborate env src none ns fileLabel heartbeats diagByteLimit

@[export lean_rs_host_elaborate_bulk_progress]
def hostElaborateBulkProgress (env : Environment) (sources : Array String)
    (ns : String) (fileLabel : String) (heartbeats : UInt64) (diagByteLimit : USize)
    (handle trampoline : USize) : IO (Except UInt8 (Array (Except ElabFailure Expr))) := do
  let mut out := #[]
  let mut idx := 0
  for src in sources do
    let result ← hostElaborate env src none ns fileLabel heartbeats diagByteLimit
    out := out.push result
    idx := idx + 1
    if let some status ← reportProgress? handle trampoline idx sources.size then
      return .error status
  return .ok out

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

@[export lean_rs_host_kernel_check_progress]
def hostKernelCheckProgress (env : Environment) (src : String)
    (ns : String) (fileLabel : String) (heartbeats : UInt64) (diagByteLimit : USize)
    (handle trampoline : USize) : IO (Except UInt8 KernelOutcome) := do
  if let some status ← reportProgress? handle trampoline 0 1 then
    return .error status
  let outcome ← hostKernelCheck env src ns fileLabel heartbeats diagByteLimit
  if let some status ← reportProgress? handle trampoline 1 1 then
    return .error status
  return .ok outcome

/-! ## Prompt 17 — proof summaries and evidence re-validation

The next two `@[export]` shims extend the evidence surface without
changing the prompt-15 `kernel_check` contract:

- `hostCheckEvidence` re-runs the kernel against the captured
  `Declaration` and reports a fresh `EvidenceStatus`.
- `hostEvidenceSummary` projects display-only metadata from the same
  `Declaration` for diagnostics or storage on the Rust side.

`EvidenceStatus` mirrors the Rust-side `crate::EvidenceStatus` enum
(four nullary constructors, ctor tags 0..=3 in declaration order).
`ProofSummary` carries three byte-bounded `String`s; Rust decodes it
through the structure-pattern primitives without inspecting any
`Lean.Expr` directly. -/

/-- Result of re-validating a `LeanEvidence` against the current
    environment. Nullary-only so it encodes through `ctor_tag` on the
    Rust side. Tag order matches `EvidenceStatus` in Rust. -/
inductive EvidenceStatus where
  | checked
  | rejected
  | unavailable
  | unsupported

/-- Lean-authored summary of a kernel-checked declaration. Carries only
    bounded `String`s so the Rust side can hold it without a `LeanObj`.
    Strings are display-only; they must not be used as semantic keys. -/
structure ProofSummary where
  declarationName : String
  kind            : String
  typeSignature   : String

/-- Soft byte cap on each `ProofSummary` field, mirroring the Rust
    `LEAN_PROOF_SUMMARY_BYTE_LIMIT` constant. -/
private def proofSummaryByteLimit : Nat := 4096

/-- Truncate `s` to at most `proofSummaryByteLimit` UTF-8 bytes, always
    stopping at a character boundary so the result is valid UTF-8.
    Iterates one `Char` at a time and accumulates `Char.utf8Size`, so
    `proofSummaryByteLimit` is a hard upper bound on the returned
    string's `utf8ByteSize`. -/
private def boundString (s : String) : String := Id.run do
  if s.utf8ByteSize ≤ proofSummaryByteLimit then
    return s
  let mut acc : String := ""
  let mut bytes : Nat := 0
  for c in s.toList do
    let nextBytes := bytes + c.utf8Size
    if nextBytes > proofSummaryByteLimit then
      break
    acc := acc.push c
    bytes := nextBytes
  return acc

/-- Project a `Declaration` into the three display fields the
    `ProofSummary` exposes: the declared name, a human-readable kind
    string, and the declared type expression. The two kinds the
    prompt-15 `kernel_check` classifier produces (`thmDecl`,
    `defnDecl`) carry user-visible content; the others surface a
    bounded fallback so the Rust side never sees an empty summary. -/
private def summarizeDeclaration (decl : Declaration) : Name × String × Expr :=
  match decl with
  | .thmDecl    v => (v.name, "theorem",    v.type)
  | .defnDecl   v => (v.name, "definition", v.type)
  | .axiomDecl  v => (v.name, "axiom",      v.type)
  | .opaqueDecl v => (v.name, "opaque",     v.type)
  | _             => (Name.anonymous, "unsupported", Expr.sort .zero)

/-- Re-validate a captured `LeanEvidence` against the current
    environment. Re-runs the kernel via `Environment.addDeclCore`. The
    declaration was accepted once by `hostKernelCheck` against this
    same environment (the session never installs the new constant),
    so the expected outcome on a fresh re-check is `Checked`; a
    `Rejected` result means the kernel now refuses the declaration
    (for example because a referenced constant changed). `Unavailable`
    covers exceptions raised through `IO`. -/
@[export lean_rs_host_check_evidence]
def hostCheckEvidence (env : Environment) (ev : Evidence) : IO EvidenceStatus := do
  try
    match Environment.addDeclCore env 0 ev.decl none with
    | .ok _    => return .checked
    | .error _ => return .rejected
  catch _ =>
    return .unavailable

/-- Summarize a captured `LeanEvidence` into bounded display strings.
    The Rust side reads the result through `take_ctor_objects::<3>`
    plus three `String` decoders; no `Lean.Expr` crosses the FFI
    boundary, only its `ToString`-rendered form. The renderer is
    intentionally the default `ToString Expr` instance rather than the
    delaboration pipeline: it is deterministic, runs without a
    `CoreM`/`MetaM` context, and is sufficient for diagnostics. -/
@[export lean_rs_host_evidence_summary]
def hostEvidenceSummary (_env : Environment) (ev : Evidence) : IO ProofSummary := do
  let (name, kind, typ) := summarizeDeclaration ev.decl
  return {
    declarationName := boundString (toString name)
    kind            := boundString kind
    typeSignature   := boundString (toString typ)
  }

end LeanRsFixture.Elaboration
