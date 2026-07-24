import Lean
import LeanRsHostShims.Elaboration

/-! Capability category: bounded module queries over Lean's info tree.

    The exported shim parses a full Lean source file, preserves the source
    coordinate system, elaborates the body, and returns only the projection
    requested by `ModuleQuery`. Lean owns header processing, info-tree
    traversal, cursor selection, and bounded rendering; Rust receives a small
    value type instead of a whole-file raw info-tree dump. -/

namespace LeanRsFixture.InfoTree

open Lean Elab Meta

private def renderByteLimit : Nat := 64 * 1024
private def referenceLimit : Nat := 1000
private def proofBoundaryCandidateLimit : Nat := 32
private def proofBoundaryExcerptLimit : Nat := 256

/-- Query shape for one module-processing request. -/
inductive ModuleQuery where
  | diagnostics
  | typeAt (line : Nat) (column : Nat)
  | goalAt (line : Nat) (column : Nat)
  | references (name : String)
  deriving Inhabited

inductive ProofPositionSelector where
  | default
  | index (index : Nat)
  | afterText (text : String) (occurrence : Option Nat)
  -- `entry` is appended last so the constructor tags of the prior variants stay
  -- fixed (`default` = 0, `index` = 1, `afterText` = 2, `entry` = 3); the Rust
  -- `IntoLean` marshaller hands a nullary `entry` across as the scalar `3u8`.
  | entry
  deriving Inhabited

/-- One selector in a batched module-processing request. Each selector carries
    a caller-chosen id so the result can be correlated without relying on
    ordering. -/
inductive ModuleQuerySelector where
  | diagnostics (id : String)
  | proofState (id : String) (line : Nat) (column : Nat)
  | typeAt (id : String) (line : Nat) (column : Nat)
  | references (id : String) (name : String)
  | declarationTarget (id : String) (name? : Option String) (line? column? : Option Nat)
  | surroundingDeclaration (id : String) (line : Nat) (column : Nat)
  | proofStateInDeclaration (id declaration : String) (position : ProofPositionSelector) (localsRaw : Nat)
  | declarationOutline (id : String)
  deriving Inhabited

/-- Explicit byte budgets for batch projections. `perFieldBytes` caps individual
    rendered fields; `totalBytes` is a conservative budget for the combined
    selector payloads before serialization. -/
structure ModuleQueryOutputBudgets where
  perFieldBytes : Nat
  totalBytes : Nat
  deriving Inhabited

/-- Source span in the original file. Lines and columns follow Lean's
    `FileMap.toPosition` convention. -/
structure ModuleSourceSpan where
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  deriving Inhabited

/-- Bounded rendered text. `truncated = true` means rendering stopped before
    visiting the whole expression or goal stream. -/
structure RenderedInfo where
  value : String
  truncated : Bool
  deriving Inhabited

/-- Identifier reference. `isBinder` distinguishes binding-site occurrences
    from use-site references. -/
structure SerializableNameRef where
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  name : String
  isBinder : Bool
  deriving Inhabited

inductive TypeAtResult where
  | term
      (span : ModuleSourceSpan)
      (expr : RenderedInfo)
      (typeStr : RenderedInfo)
      (expectedType : Option RenderedInfo)
  | noTerm
  deriving Inhabited

inductive GoalAtResult where
  | goal
      (span : ModuleSourceSpan)
      (goalsBefore : Array String)
      (goalsAfter : Array String)
      (truncated : Bool)
  | noTacticContext
  deriving Inhabited

structure ReferencesResult where
  references : Array SerializableNameRef
  truncated : Bool
  deriving Inhabited

structure LocalInfo where
  name : String
  binderInfo : String
  typeStr : RenderedInfo
  value : Option RenderedInfo
  deriving Inhabited

structure DeclarationTargetInfo where
  shortName : String
  declarationName : String
  namespaceName : String
  declarationKind : String
  declarationSpan : ModuleSourceSpan
  nameSpan : ModuleSourceSpan
  bodySpan : ModuleSourceSpan
  deriving Inhabited

inductive DeclarationTargetResult where
  | target (info : DeclarationTargetInfo)
  | notFound
  | ambiguous (candidates : Array DeclarationTargetInfo)
  deriving Inhabited

structure DeclarationOutlineResult where
  declarations : Array DeclarationTargetInfo
  truncated : Bool
  deriving Inhabited

structure ProofBoundaryCandidate where
  index : Nat
  kind : String
  source : ModuleSourceSpan
  excerpt : RenderedInfo
  deriving Inhabited

structure ProofStateInfo where
  declarationName : Option String
  namespaceName : String
  safeEdit : Option DeclarationTargetInfo
  span : ModuleSourceSpan
  goalsBefore : Array String
  goalsAfter : Array String
  locals : Array LocalInfo
  expectedType : Option RenderedInfo
  truncated : Bool
  proofBoundaries : Array ProofBoundaryCandidate
  proofBoundariesTruncated : Bool
  deriving Inhabited

inductive ProofStateResult where
  | state (info : ProofStateInfo)
  | unavailable (message : String) (proofBoundaries : Array ProofBoundaryCandidate)
      (proofBoundariesTruncated : Bool)
  | ambiguous (candidates : Array DeclarationTargetInfo)
  | needsBuild (missing : Array String)
  deriving Inhabited

inductive SurroundingDeclarationResult where
  | declaration (info : DeclarationTargetInfo)
  | none
  deriving Inhabited

inductive ModuleQueryResult where
  | diagnostics (failure : LeanRsFixture.Elaboration.ElabFailure)
  | typeAt (result : TypeAtResult)
  | goalAt (result : GoalAtResult)
  | references (result : ReferencesResult)
  deriving Inhabited

inductive ModuleQueryOutcome where
  | ok
      (result : ModuleQueryResult)
      (imports : Array String)
  | missingImports
      (result : ModuleQueryResult)
      (imports : Array String)
      (missing : Array String)
  | headerParseFailed
      (diagnostics : LeanRsFixture.Elaboration.ElabFailure)
  deriving Inhabited

inductive ModuleQueryBatchResult where
  | diagnostics (failure : LeanRsFixture.Elaboration.ElabFailure)
  | proofState (result : ProofStateResult)
  | typeAt (result : TypeAtResult)
  | references (result : ReferencesResult)
  | declarationTarget (result : DeclarationTargetResult)
  | surroundingDeclaration (result : SurroundingDeclarationResult)
  | declarationOutline (result : DeclarationOutlineResult)
  deriving Inhabited

inductive ModuleQueryBatchItem where
  | ok (id : String) (result : ModuleQueryBatchResult)
  | unavailable (id message : String)
  | budgetExceeded (id message : String)
  deriving Inhabited

structure ModuleQueryBatchEnvelope where
  items : Array ModuleQueryBatchItem
  totalTruncated : Bool
  deriving Inhabited

inductive ModuleQueryBatchOutcome where
  | ok
      (result : ModuleQueryBatchEnvelope)
      (imports : Array String)
  | missingImports
      (result : ModuleQueryBatchEnvelope)
      (imports : Array String)
      (missing : Array String)
  | headerParseFailed
      (diagnostics : LeanRsFixture.Elaboration.ElabFailure)
  deriving Inhabited

inductive ModuleQueryCacheStatus where
  | hit
  | miss
  | rebuilt
  | evicted
  deriving Inhabited

structure ModuleQueryTimings where
  headerImportMicros : UInt64
  elaborationMicros : UInt64
  projectionMicros : UInt64
  renderingMicros : UInt64
  deriving Inhabited

structure ModuleQueryCacheFacts where
  cacheStatus : ModuleQueryCacheStatus
  timings : ModuleQueryTimings
  outputBytes : UInt64
  cacheEntryCount : Option Nat
  cacheApproxBytes : Option Nat
  deriving Inhabited

structure ModuleQueryCachePolicy where
  fileIdentity : String
  key : String
  maxEntries : UInt64
  ttlMillis : UInt64
  maxBytes : UInt64
  deriving Inhabited

inductive ModuleQueryBatchCachedOutcome where
  | ok
      (result : ModuleQueryBatchEnvelope)
      (imports : Array String)
      (facts : ModuleQueryCacheFacts)
  | missingImports
      (result : ModuleQueryBatchEnvelope)
      (imports : Array String)
      (missing : Array String)
      (facts : ModuleQueryCacheFacts)
  | headerParseFailed
      (diagnostics : LeanRsFixture.Elaboration.ElabFailure)
      (facts : ModuleQueryCacheFacts)
  deriving Inhabited

structure ModuleSnapshotCacheClearResult where
  entriesCleared : UInt64
  approxBytesCleared : UInt64
  deriving Inhabited

inductive ProofEditTarget where
  | declaration (name : String) (position : ProofPositionSelector)
  deriving Inhabited

structure ProofCandidate where
  id : String
  text : String
  deriving Inhabited

structure ProofAttemptRequest where
  source : String
  edit : ProofEditTarget
  candidates : Array ProofCandidate
  budgets : ModuleQueryOutputBudgets
  deriving Inhabited

inductive ProofAttemptStatus where
  | closed
  | progressed
  | failed
  | timeout
  | budgetExceeded
  | notAttempted
  | unsupported
  deriving Inhabited

structure ProofPositionSummary where
  index : Nat
  tactic : RenderedInfo
  deriving Inhabited

structure ProofAttemptRow where
  id : String
  status : ProofAttemptStatus
  candidateText : RenderedInfo
  diagnostics : LeanRsFixture.Elaboration.ElabFailure
  downstreamDiagnostics : LeanRsFixture.Elaboration.ElabFailure
  goals : Array RenderedInfo
  declaration : Option DeclarationTargetInfo
  proofPosition : Option ProofPositionSummary
  outputTruncated : Bool
  deriving Inhabited

structure ProofAttemptEnvelope where
  candidates : Array ProofAttemptRow
  candidateLimit : Nat
  candidatesTruncated : Bool
  /-- Goal state at the resolved proof position before any candidate ran — the
      selected tactic's `goalsBefore`, i.e. the state the proof-position query
      reports as `goalsBefore` at the same position (the goal a candidate for
      the *next* step must close). Rendered once per batch through the same
      machinery as the proof-position query; empty when the entry state is
      degraded or unresolvable (resolution failure or the source-text
      fallback), never an error. -/
  entryGoals : Array RenderedInfo
  /-- Local hypotheses at the resolved proof position, collected once per batch
      with `LocalsRendering.pretty` (the proof-position query's default) from
      the same `goalsBefore` state as `entryGoals` — identical to what the
      proof-position query reports as `locals` at the same position. Empty
      under the same conditions as `entryGoals`. -/
  locals : Array LocalInfo
  deriving Inhabited

inductive ProofAttemptOutcome where
  | ok (result : ProofAttemptEnvelope) (imports : Array String)
  | missingImports (result : ProofAttemptEnvelope) (imports missing : Array String)
  | headerParseFailed (diagnostics : LeanRsFixture.Elaboration.ElabFailure)
  | unsupported
  deriving Inhabited

inductive DeclarationVerificationTarget where
  | name (name : String)
  | span (span : ModuleSourceSpan)
  deriving Inhabited

inductive SorryPolicy where
  | allow
  | deny
  deriving Inhabited

structure DeclarationVerificationRequest where
  source : String
  target : DeclarationVerificationTarget
  sorryPolicy : Nat
  reportAxioms : Nat
  budgets : ModuleQueryOutputBudgets
  deriving Inhabited

structure DeclarationVerificationBatchItem where
  id : String
  target : DeclarationVerificationTarget
  deriving Inhabited

structure DeclarationVerificationBatchRequest where
  source : String
  targets : Array DeclarationVerificationBatchItem
  sorryPolicy : Nat
  reportAxioms : Nat
  budgets : ModuleQueryOutputBudgets
  deriving Inhabited

inductive DeclarationVerificationStatus where
  | accepted
  | rejected
  | notFound
  | ambiguous
  | timeout
  | budgetExceeded
  | unsupported
  | needsBuild
  deriving Inhabited

structure DeclarationVerificationFacts where
  target : Option DeclarationTargetInfo
  diagnostics : LeanRsFixture.Elaboration.ElabFailure
  unresolvedGoals : Array RenderedInfo
  containsSorry : Bool
  containsAdmit : Bool
  containsSorryAx : Bool
  axioms : Array String
  axiomsTruncated : Bool
  outputTruncated : Bool
  /-- Competing declarations when `status = ambiguous`; empty otherwise. -/
  candidates : Array DeclarationTargetInfo
  /-- `false` when the axiom dependency set could not be computed (the target
      did not resolve, or `collectAxioms` was not run): an empty `axioms` then
      means "not computed", not "no axioms". `true` with empty `axioms` means a
      genuine no-nontrivial-axioms result. -/
  axiomsAvailable : Bool
  deriving Inhabited

inductive DeclarationVerificationOutcome where
  | ok (status : DeclarationVerificationStatus) (facts : DeclarationVerificationFacts) (imports : Array String)
  | missingImports
      (status : DeclarationVerificationStatus) (facts : DeclarationVerificationFacts)
      (imports missing : Array String)
  | headerParseFailed (diagnostics : LeanRsFixture.Elaboration.ElabFailure)
  | unsupported
  deriving Inhabited

structure DeclarationVerificationBatchRow where
  id : String
  target : DeclarationVerificationTarget
  status : DeclarationVerificationStatus
  facts : DeclarationVerificationFacts
  deriving Inhabited

inductive DeclarationVerificationBatchOutcome where
  | ok (results : Array DeclarationVerificationBatchRow) (imports : Array String)
  | missingImports
      (results : Array DeclarationVerificationBatchRow) (imports missing : Array String)
  | headerParseFailed (diagnostics : LeanRsFixture.Elaboration.ElabFailure)
  | unsupported
  deriving Inhabited

private def rangeOfStx (fileMap : FileMap) (stx : Syntax) : Option ModuleSourceSpan :=
  match stx.getRange? with
  | none => none
  | some ⟨sp, ep⟩ =>
    let s := fileMap.toPosition sp
    let e := fileMap.toPosition ep
    some {
      startLine := s.line, startColumn := s.column + 1
      endLine := e.line, endColumn := e.column + 1
    }

private def rangeContains (span : ModuleSourceSpan) (line column : Nat) : Bool :=
  if line < span.startLine || line > span.endLine then
    false
  else if line == span.startLine && column < span.startColumn then
    false
  else if line == span.endLine && column > span.endColumn then
    false
  else
    true

private def rangeArea (span : ModuleSourceSpan) : Nat :=
  let lineSpan := span.endLine - span.startLine
  let colSpan := span.endColumn - span.startColumn
  lineSpan * 1000000 + colSpan

private structure SourceDocument where
  source : String
  fileMap : FileMap
  bodyFileMap : FileMap
  lineOffset : Nat

private def SourceDocument.fromSources (source bodySource : String) (lineOffset : Nat) : SourceDocument :=
  { source, fileMap := source.toFileMap, bodyFileMap := bodySource.toFileMap, lineOffset }

private def SourceDocument.fileLineToBody (doc : SourceDocument) (line : Nat) : Nat :=
  if line > doc.lineOffset then line - doc.lineOffset else 0

private def SourceDocument.bodySpanToFile (doc : SourceDocument) (span : ModuleSourceSpan) : ModuleSourceSpan :=
  { span with startLine := span.startLine + doc.lineOffset, endLine := span.endLine + doc.lineOffset }

private def SourceDocument.fileSpanToBody (doc : SourceDocument) (span : ModuleSourceSpan) : ModuleSourceSpan :=
  { span with startLine := doc.fileLineToBody span.startLine, endLine := doc.fileLineToBody span.endLine }

private def SourceDocument.syntaxSpan? (doc : SourceDocument) (stx : Syntax) : Option ModuleSourceSpan :=
  rangeOfStx doc.bodyFileMap stx

private def SourceDocument.bodyCursor (doc : SourceDocument) (line column : Nat) : Nat × Nat :=
  (doc.fileLineToBody line, column)

private def SourceDocument.bodySpanContains (span : ModuleSourceSpan) (line column : Nat) : Bool :=
  rangeContains span line column

private def SourceDocument.rawOffset? (doc : SourceDocument) (line column : Nat) : Option String.Pos.Raw :=
  if line == 0 || column == 0 then
    none
  else
    some <| doc.fileMap.ofPosition { line, column := column - 1 }

private def SourceDocument.replaceRawSpan (doc : SourceDocument) (replacement : String) (start stop : String.Pos.Raw) :
    String :=
  let pfx := String.Pos.Raw.extract doc.source 0 start
  let suffix := String.Pos.Raw.extract doc.source stop doc.source.rawEndPos
  pfx ++ replacement ++ suffix

private def SourceDocument.extractFileSpan? (doc : SourceDocument) (span : ModuleSourceSpan) : Option String :=
  match doc.rawOffset? span.startLine span.startColumn,
      doc.rawOffset? span.endLine span.endColumn with
  | some start, some stop => some (String.Pos.Raw.extract doc.source start stop)
  | _, _ => none

private def SourceDocument.extractBodySpan? (doc : SourceDocument) (span : ModuleSourceSpan) : Option String :=
  doc.extractFileSpan? (doc.bodySpanToFile span)

private structure RenderState where
  limit : Nat := renderByteLimit
  out : String := ""
  bytes : Nat := 0
  truncated : Bool := false

private def appendBounded (s : String) (st : RenderState) : RenderState :=
  if st.truncated then
    st
  else
    let b := s.utf8ByteSize
    if st.bytes + b > st.limit then
      { st with out := st.out ++ "<truncated>", truncated := true }
    else
      { st with out := st.out ++ s, bytes := st.bytes + b }

private partial def renderExprInto (e : Expr) (st : RenderState) : RenderState :=
  if st.truncated then
    st
  else
    match e with
    | .bvar i =>
      appendBounded ("#" ++ toString i) st
    | .fvar id =>
      appendBounded (toString id.name) st
    | .mvar id =>
      appendBounded ("?" ++ toString id.name) st
    | .sort level =>
      appendBounded ("Sort " ++ toString level) st
    | .const name levels =>
      let suffix :=
        if levels.isEmpty then "" else ".{" ++ String.intercalate ", " (levels.map toString) ++ "}"
      appendBounded (toString name ++ suffix) st
    | .lit lit =>
      let s :=
        match lit with
        | .natVal n => toString n
        | .strVal s => reprStr s
      appendBounded s st
    | .app f a =>
      let st := appendBounded "(" st
      let st := renderExprInto f st
      let st := appendBounded " " st
      let st := renderExprInto a st
      appendBounded ")" st
    | .lam name type body _ =>
      let st := appendBounded ("(fun " ++ toString name ++ " : ") st
      let st := renderExprInto type st
      let st := appendBounded " => " st
      let st := renderExprInto body st
      appendBounded ")" st
    | .forallE name type body _ =>
      let st := appendBounded ("(forall " ++ toString name ++ " : ") st
      let st := renderExprInto type st
      let st := appendBounded ", " st
      let st := renderExprInto body st
      appendBounded ")" st
    | .letE name type value body _ =>
      let st := appendBounded ("(let " ++ toString name ++ " : ") st
      let st := renderExprInto type st
      let st := appendBounded " := " st
      let st := renderExprInto value st
      let st := appendBounded "; " st
      let st := renderExprInto body st
      appendBounded ")" st
    | .mdata _ body =>
      renderExprInto body st
    | .proj typeName idx struct =>
      let st := appendBounded ("(" ++ toString typeName ++ "." ++ toString idx ++ " ") st
      let st := renderExprInto struct st
      appendBounded ")" st

private def renderExprBounded (e : Expr) : RenderedInfo :=
  let st := renderExprInto e {}
  { value := st.out, truncated := st.truncated }

private def renderExprBoundedWith (limit : Nat) (e : Expr) : RenderedInfo :=
  let st := renderExprInto e { limit := limit }
  { value := st.out, truncated := st.truncated }

private def renderStringBoundedWith (limit : Nat) (text : String) : RenderedInfo :=
  if text.utf8ByteSize ≤ limit then
    { value := text, truncated := false }
  else
    let st := text.foldl (init := { limit := limit }) fun st c => appendBounded c.toString st
    { value := st.out, truncated := true }

private def emptyFailure : LeanRsFixture.Elaboration.ElabFailure :=
  { diagnostics := #[], truncated := .complete }

private def failureFromMessages (messages : MessageLog) (diagBytes : USize) (fileLabel : String) :
    IO LeanRsFixture.Elaboration.ElabFailure := do
  let (diags, trunc) ← LeanRsFixture.Elaboration.serializeMessages messages diagBytes fileLabel
  pure { diagnostics := diags, truncated := trunc }

/-- Collect the metavariables that occur structurally in `e`, without ever
    dereferencing an assignment. The `hasExprMVar` cached bit prunes whole
    subtrees, so this is cheap on the common (mvar-free) case. -/
private partial def collectExprMVars (e : Expr) (acc : Array MVarId) : Array MVarId :=
  if !e.hasExprMVar then
    acc
  else
    match e with
    | .mvar id => acc.push id
    | .app f a => collectExprMVars a (collectExprMVars f acc)
    | .lam _ t b _ => collectExprMVars b (collectExprMVars t acc)
    | .forallE _ t b _ => collectExprMVars b (collectExprMVars t acc)
    | .letE _ t v b _ => collectExprMVars b (collectExprMVars v (collectExprMVars t acc))
    | .mdata _ b => collectExprMVars b acc
    | .proj _ _ s => collectExprMVars s acc
    | _ => acc

/-- A goal is *degraded* when the metavariable itself, or any metavariable
    reachable structurally from its target type or local-context hypotheses, has
    no declaration in `mctx`. Rendering such a goal drives `Meta.ppGoal` /
    `instantiateMVars` into the *pure*, panicking `MetavarContext.getDecl`, which
    aborts the worker process — an interrupted elaboration under memory pressure
    leaves exactly this shape. The check is total: only `findDecl?` and a
    structural walk, never an assignment dereference. It is best-effort (it
    cannot see metavariables that only surface through delayed-assignment
    binding), so the supervisor's process-level guard remains authoritative. -/
private def goalDegraded (mctx : MetavarContext) (mvarId : MVarId) : Bool :=
  match mctx.findDecl? mvarId with
  | none => true
  | some decl =>
    let mvars := collectExprMVars decl.type #[]
    let mvars := decl.lctx.foldl (init := mvars) fun acc localDecl =>
      let acc := collectExprMVars localDecl.type acc
      match localDecl.value? with
      | some value => collectExprMVars value acc
      | none => acc
    mvars.any fun id => (mctx.findDecl? id).isNone

private def renderGoal (ctx : ContextInfo) (mctx : MetavarContext) (mvarId : MVarId)
    (byteLimit : USize) (currentBytes : USize) : IO (String × USize × Bool) := do
  if currentBytes ≥ byteLimit then
    return ("<truncated>", currentBytes, true)
  if goalDegraded mctx mvarId then
    return ("<goal unavailable: elaboration degraded under resource pressure>", currentBytes, false)
  let s ← ctx.runMetaM .empty do
    setMCtx mctx
    try
      let fmt ← Meta.ppGoal mvarId
      pure fmt.pretty
    catch _ =>
      pure "<error rendering goal>"
  let newBytes := currentBytes + s.utf8ByteSize.toUSize
  if newBytes > byteLimit then
    return ("<truncated>", currentBytes, true)
  return (s, newBytes, false)

private def renderGoals (ctx : ContextInfo) (mctx : MetavarContext) (mvars : List MVarId)
    (byteLimit : USize) (start : USize) : IO (Array String × USize × Bool) := do
  let mut out : Array String := #[]
  let mut bytes := start
  let mut truncated := false
  for mvarId in mvars do
    let (s, newBytes, didTruncate) ← renderGoal ctx mctx mvarId byteLimit bytes
    out := out.push s
    bytes := newBytes
    if didTruncate then
      truncated := true
      break
  return (out, bytes, truncated)

private structure TermCandidate where
  span : ModuleSourceSpan
  ctx : ContextInfo
  expr : Expr
  lctx : LocalContext
  expectedType? : Option Expr

private structure TypeAtAcc where
  line : Nat
  column : Nat
  best? : Option TermCandidate := none

private def betterSpan (candidate : ModuleSourceSpan) (best? : Option TermCandidate) : Bool :=
  match best? with
  | none => true
  | some best => rangeArea candidate < rangeArea best.span

private def collectTypeAt (fileMap : FileMap) (ctx : ContextInfo) (info : Info) (acc : TypeAtAcc) :
    IO TypeAtAcc := do
  match info with
  | .ofTermInfo ti =>
    match rangeOfStx fileMap ti.stx with
    | some span =>
      if rangeContains span acc.line acc.column && betterSpan span acc.best? then
        pure { acc with best? := some {
          span := span, ctx := ctx, expr := ti.expr, lctx := ti.lctx, expectedType? := ti.expectedType?
        } }
      else
        pure acc
    | none => pure acc
  | _ => pure acc

private def runTypeAt (doc : SourceDocument) (trees : PersistentArray InfoTree) (line column : Nat) :
    IO TypeAtResult := do
  let (line, column) := doc.bodyCursor line column
  let mut acc : TypeAtAcc := { line, column }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectTypeAt doc.bodyFileMap)
  match acc.best? with
  | none => pure .noTerm
  | some candidate =>
    let typeInfo ← candidate.ctx.runMetaM candidate.lctx do
      try
        let ty ← Meta.inferType candidate.expr
        pure (renderExprBounded ty)
      catch _ =>
        pure { value := "", truncated := false }
    pure <| .term (doc.bodySpanToFile candidate.span) (renderExprBounded candidate.expr) typeInfo
      (candidate.expectedType?.map renderExprBounded)

private def runTypeAtWith (doc : SourceDocument) (trees : PersistentArray InfoTree) (line column : Nat)
    (limit : Nat) : IO TypeAtResult := do
  let (line, column) := doc.bodyCursor line column
  let mut acc : TypeAtAcc := { line, column }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectTypeAt doc.bodyFileMap)
  match acc.best? with
  | none => pure .noTerm
  | some candidate =>
    let typeInfo ← candidate.ctx.runMetaM candidate.lctx do
      try
        let ty ← Meta.inferType candidate.expr
        pure (renderExprBoundedWith limit ty)
      catch _ =>
        pure { value := "", truncated := false }
    pure <| .term (doc.bodySpanToFile candidate.span) (renderExprBoundedWith limit candidate.expr) typeInfo
      (candidate.expectedType?.map (renderExprBoundedWith limit))

private structure TacticCandidate where
  span : ModuleSourceSpan
  ctx : ContextInfo
  mctxBefore : MetavarContext
  goalsBefore : List MVarId
  mctxAfter : MetavarContext
  goalsAfter : List MVarId

/-- A captured tactic state is degraded when any before/after goal is degraded
    against its respective metavariable context (see `goalDegraded`). -/
private def tacticCandidateDegraded (candidate : TacticCandidate) : Bool :=
  candidate.goalsBefore.any (goalDegraded candidate.mctxBefore) ||
    candidate.goalsAfter.any (goalDegraded candidate.mctxAfter)

private structure GoalAtAcc where
  line : Nat
  column : Nat
  best? : Option TacticCandidate := none

private def betterTacticSpan (candidate : ModuleSourceSpan) (best? : Option TacticCandidate) : Bool :=
  match best? with
  | none => true
  | some best => rangeArea candidate < rangeArea best.span

private def collectGoalAt (fileMap : FileMap) (ctx : ContextInfo) (info : Info) (acc : GoalAtAcc) :
    IO GoalAtAcc := do
  match info with
  | .ofTacticInfo ti =>
    match rangeOfStx fileMap ti.stx with
    | some span =>
      if rangeContains span acc.line acc.column && betterTacticSpan span acc.best? then
        pure { acc with best? := some {
          span := span, ctx := ctx, mctxBefore := ti.mctxBefore, goalsBefore := ti.goalsBefore
          mctxAfter := ti.mctxAfter, goalsAfter := ti.goalsAfter
        } }
      else
        pure acc
    | none => pure acc
  | _ => pure acc

private def runGoalAt (doc : SourceDocument) (trees : PersistentArray InfoTree) (line column : Nat)
    (diagBytes : USize) : IO GoalAtResult := do
  let (line, column) := doc.bodyCursor line column
  let mut acc : GoalAtAcc := { line, column }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectGoalAt doc.bodyFileMap)
  match acc.best? with
  | none => pure .noTacticContext
  | some candidate =>
    let (before, bytesAfterBefore, truncBefore) ←
      renderGoals candidate.ctx candidate.mctxBefore candidate.goalsBefore diagBytes 0
    let (after, _, truncAfter) ←
      renderGoals candidate.ctx candidate.mctxAfter candidate.goalsAfter diagBytes bytesAfterBefore
    pure <| .goal (doc.bodySpanToFile candidate.span) before after (truncBefore || truncAfter)

private structure ReferencesAcc where
  target : String
  refs : Array SerializableNameRef := #[]
  truncated : Bool := false

private def pushNameRef (span : ModuleSourceSpan) (name : String) (isBinder : Bool) (acc : ReferencesAcc) :
    ReferencesAcc :=
  if acc.truncated then
    acc
  else if name != acc.target then
    acc
  else if acc.refs.size >= referenceLimit then
    { acc with truncated := true }
  else
    { acc with refs := acc.refs.push {
      startLine := span.startLine, startColumn := span.startColumn
      endLine := span.endLine, endColumn := span.endColumn
      name := name, isBinder := isBinder
    } }

private def collectReferences (doc : SourceDocument) (_ctx : ContextInfo) (info : Info) (acc : ReferencesAcc) :
    IO ReferencesAcc := do
  if acc.truncated then
    pure acc
  else
    match info with
    | .ofTermInfo ti =>
      match doc.syntaxSpan? ti.stx with
      | none => pure acc
      | some span =>
        if ti.expr.isConst && ti.stx.isIdent then
          pure <| pushNameRef (doc.bodySpanToFile span) (toString ti.expr.constName!) ti.isBinder acc
        else if ti.stx.isIdent && ti.isBinder then
          pure <| pushNameRef (doc.bodySpanToFile span) (toString ti.stx.getId) true acc
        else
          pure acc
    | _ => pure acc

private def runReferences (doc : SourceDocument) (trees : PersistentArray InfoTree) (name : String) :
    IO ReferencesResult := do
  let mut acc : ReferencesAcc := { target := name }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectReferences doc)
    if acc.truncated then
      break
  pure { references := acc.refs, truncated := acc.truncated }

private def lemmaSyntaxKind : SyntaxNodeKind :=
  Name.mkSimple "lemma"

private partial def firstIdent? (stx : Syntax) : Option Syntax :=
  if stx.isIdent then
    some stx
  else
    stx.getArgs.findSome? firstIdent?

private def declIdNameStx (stx : Syntax) : Syntax :=
  if stx.getKind == ``Lean.Parser.Command.instance then
    if let some identStx := firstIdent? stx[3] then
      identStx
    else
      stx[1]
  else if let some identStx := firstIdent? stx[1] then
    identStx
  else
    stx[0]

private def declarationKeyword (stx : Syntax) : String :=
  if stx.getKind == lemmaSyntaxKind then
    "lemma"
  else if stx.getKind == ``Lean.Parser.Command.instance then
    "instance"
  else if stx.getNumArgs > 0 && stx[0].isAtom then
    stx[0].getAtomVal
  else
    "theorem"

private def catalogDeclarationSyntax? (stx : Syntax) : Option Syntax :=
  if stx.getKind == ``Lean.Parser.Command.declaration then
    let inner := stx[1]
    if inner.getKind == ``Lean.Parser.Command.theorem ||
        inner.getKind == ``Lean.Parser.Command.definition ||
        inner.getKind == ``Lean.Parser.Command.abbrev ||
        inner.getKind == ``Lean.Parser.Command.opaque ||
        inner.getKind == ``Lean.Parser.Command.instance then
      some inner
    else
      none
  else if stx.getKind == lemmaSyntaxKind then
    some stx
  else
    none

private partial def declarationBodySyntax? (stx : Syntax) : Option Syntax :=
  if stx.getKind == ``Lean.Parser.Command.instance && stx.getNumArgs >= 6 then
    some stx[5]
  else if stx.getKind == ``Lean.Parser.Command.declValSimple && stx.getNumArgs >= 2 then
    some stx[1]
  else
    stx.getArgs.findSome? declarationBodySyntax?

private structure DeclarationCandidate where
  info : DeclarationTargetInfo
  bodySpanBodyCoords : ModuleSourceSpan
  declarationSpanBodyCoords : ModuleSourceSpan

private def fullDeclarationName (namespaceName shortName : Name) : Name :=
  if namespaceName.isAnonymous then shortName else namespaceName ++ shortName

private def declarationRangeMatchesSpan (range : DeclarationRange) (span : ModuleSourceSpan) : Bool :=
  range.pos.line == span.startLine &&
    range.pos.column == span.startColumn &&
    range.endPos.line == span.endLine &&
    range.endPos.column == span.endColumn

private def resolvedDeclarationName? (ctx : ContextInfo) (nameSpan : ModuleSourceSpan) : Option Name := do
  let commandEnv ← ctx.cmdEnv?
  Id.run do
    let mut matched : Array Name := #[]
    for (name, _) in commandEnv.constants.map₂ do
      if !ctx.env.contains name then
        let ranges? :=
          declRangeExt.find? (level := .exported) commandEnv name <|>
            declRangeExt.find? (level := .server) commandEnv name
        match ranges? with
        | some ranges =>
          if declarationRangeMatchesSpan ranges.selectionRange nameSpan then
            matched := matched.push name
        | none => pure ()
    match matched with
    | #[name] => some name
    | _ => none

private def commandDeclaration? (doc : SourceDocument) (ctx : ContextInfo) (stx : Syntax) :
    IO (Option DeclarationCandidate) := do
  let some declStx := catalogDeclarationSyntax? stx | return none
  let nameStx := declIdNameStx declStx
  if !nameStx.isIdent then
    return none
  let some bodyStx := declarationBodySyntax? declStx | return none
  let some declarationSpanBody := doc.syntaxSpan? declStx | return none
  let some nameSpanBody := doc.syntaxSpan? nameStx | return none
  let some bodySpanBody := doc.syntaxSpan? bodyStx | return none
  let shortName := nameStx.getId
  let namespaceName := ctx.currNamespace
  let nameSpan := doc.bodySpanToFile nameSpanBody
  let fullName := (resolvedDeclarationName? ctx nameSpan).getD (fullDeclarationName namespaceName shortName)
  let info : DeclarationTargetInfo := {
    shortName := shortName.toString
    declarationName := fullName.toString
    namespaceName := namespaceName.toString
    declarationKind := declarationKeyword declStx
    declarationSpan := doc.bodySpanToFile declarationSpanBody
    nameSpan
    bodySpan := doc.bodySpanToFile bodySpanBody
  }
  return some { info, bodySpanBodyCoords := bodySpanBody, declarationSpanBodyCoords := declarationSpanBody }

private def collectDeclarations (doc : SourceDocument) (ctx : ContextInfo) (info : Info)
    (acc : Array DeclarationCandidate) : IO (Array DeclarationCandidate) := do
  match info with
  | .ofCommandInfo ci =>
    match ← commandDeclaration? doc ctx ci.stx with
    | some candidate => pure (acc.push candidate)
    | none => pure acc
  | _ => pure acc

private def declarationCandidates (doc : SourceDocument) (trees : PersistentArray InfoTree) :
    IO (Array DeclarationCandidate) := do
  let mut out : Array DeclarationCandidate := #[]
  for tree in trees do
    out ← tree.foldInfoM (init := out) (collectDeclarations doc)
  pure out

/-- Collapse candidates that name the *same* declaration. Source-snapshot
    candidates are collected from `CommandInfo` nodes (`declarationCandidates`),
    and a single declaration can be recorded more than once — e.g. when a
    degraded environment re-elaborates a command. Two candidates are the same
    declaration iff they share a fully-qualified `declarationName`; Lean forbids
    two distinct valid declarations from sharing one, so this can never merge a
    genuine collision. It keeps the first occurrence, preserving declaration
    order. Genuine ambiguity (a short name matching declarations in different
    namespaces) survives because those carry distinct `declarationName`s. -/
private def dedupByDeclarationName (candidates : Array DeclarationCandidate) : Array DeclarationCandidate := Id.run do
  let mut seen : Array String := #[]
  let mut out : Array DeclarationCandidate := #[]
  for candidate in candidates do
    if seen.contains candidate.info.declarationName then
      continue
    seen := seen.push candidate.info.declarationName
    out := out.push candidate
  pure out

private def declarationTargetByName (candidates : Array DeclarationCandidate) (name : String) :
    DeclarationTargetResult :=
  let matched := dedupByDeclarationName <| candidates.filter fun candidate =>
    candidate.info.declarationName == name || candidate.info.shortName == name
  match matched with
  | #[] => .notFound
  | #[candidate] => .target candidate.info
  | many => .ambiguous (many.map (·.info))

private def declarationAt? (doc : SourceDocument) (candidates : Array DeclarationCandidate) (line column : Nat) :
    Option DeclarationCandidate :=
  let bodyCursorLine := doc.fileLineToBody line
  let containing := candidates.filter fun candidate =>
    rangeContains candidate.declarationSpanBodyCoords bodyCursorLine column
  containing.foldl
    (init := none)
    fun best? candidate =>
      match best? with
      | none => some candidate
      | some best =>
        if rangeArea candidate.declarationSpanBodyCoords < rangeArea best.declarationSpanBodyCoords then
          some candidate
        else
          best?

private def declarationTargetByPosition (doc : SourceDocument) (candidates : Array DeclarationCandidate) (line column : Nat) :
    DeclarationTargetResult :=
  match declarationAt? doc candidates line column with
  | some candidate => .target candidate.info
  | none => .notFound

private def selectorDeclarationTarget (doc : SourceDocument) (candidates : Array DeclarationCandidate) (name? : Option String)
    (line? column? : Option Nat) : DeclarationTargetResult :=
  match name?, line?, column? with
  | some name, _, _ => declarationTargetByName candidates name
  | none, some line, some column => declarationTargetByPosition doc candidates line column
  | _, _, _ => .notFound

private structure ProofPosition where
  index : Nat
  tactic : TacticCandidate
  summary : ProofPositionSummary

private def lineColumnAfterText (line column : Nat) (text : String) : Nat × Nat :=
  let charCount (text : String) : Nat :=
    text.foldl (init := 0) fun n _ => n + 1
  let parts := text.splitOn "\n"
  match parts.reverse with
  | [] => (line, column)
  | last :: _ =>
    if parts.length == 1 then
      (line, column + charCount text)
    else
      (line + parts.length - 1, charCount last + 1)

private def tacticSummary (doc : SourceDocument) (index : Nat) (span : ModuleSourceSpan) : ProofPositionSummary :=
  let text := (doc.extractBodySpan? span).getD ""
  { index, tactic := { value := text, truncated := false } }

private def spanStrictlyContains (outer inner : ModuleSourceSpan) : Bool :=
  rangeContains outer inner.startLine inner.startColumn &&
    rangeContains outer inner.endLine inner.endColumn &&
    !(outer.startLine == inner.startLine && outer.startColumn == inner.startColumn &&
      outer.endLine == inner.endLine && outer.endColumn == inner.endColumn)

private def minimalTacticCandidates (tactics : Array TacticCandidate) : Array TacticCandidate :=
  tactics.filter fun candidate =>
    !tactics.any fun other => spanStrictlyContains candidate.span other.span

private def collectTacticsInDeclaration (doc : SourceDocument) (decl : DeclarationCandidate)
    (trees : PersistentArray InfoTree) : IO (Array ProofPosition) := do
  let mut tactics : Array TacticCandidate := #[]
  for tree in trees do
    tactics ← tree.foldInfoM (init := tactics) fun ctx info acc => do
      match info with
      | .ofTacticInfo ti =>
        -- The `by` keyword carries its own `TacticInfo` as a bare atom (the range
        -- of just the `by` token). It is not an insertable tactic: selecting it
        -- would splice the candidate before the first real tactic at the `by`
        -- line's indentation — column 0 for a top-level declaration — which
        -- Lean ≥ 4.31 rejects as a dedented command. Keep only real tactic nodes.
        if ti.stx.isAtom then
          pure acc
        else match rangeOfStx doc.bodyFileMap ti.stx with
          | some span =>
            if rangeContains decl.bodySpanBodyCoords span.startLine span.startColumn then
              pure <| acc.push {
                span, ctx, mctxBefore := ti.mctxBefore, goalsBefore := ti.goalsBefore,
                mctxAfter := ti.mctxAfter, goalsAfter := ti.goalsAfter
              }
            else
              pure acc
          | none => pure acc
      | _ => pure acc
  let sorted := tactics.qsort fun left right =>
    left.span.startLine < right.span.startLine ||
      (left.span.startLine == right.span.startLine && left.span.startColumn < right.span.startColumn) ||
      (left.span.startLine == right.span.startLine && left.span.startColumn == right.span.startColumn &&
        rangeArea left.span < rangeArea right.span)
  let minimal := minimalTacticCandidates sorted
  pure <| minimal.mapIdx fun idx tactic =>
    { index := idx, tactic, summary := tacticSummary doc idx tactic.span }

private def boundaryExcerptLimit (budgets : ModuleQueryOutputBudgets) : Nat :=
  min proofBoundaryExcerptLimit budgets.perFieldBytes

private def proofBoundaryCandidate (doc : SourceDocument) (kind : String) (pos : ProofPosition)
    (budgets : ModuleQueryOutputBudgets) : ProofBoundaryCandidate :=
  {
    index := pos.index,
    kind,
    source := doc.bodySpanToFile pos.tactic.span,
    excerpt := renderStringBoundedWith (boundaryExcerptLimit budgets) pos.summary.tactic.value
  }

private def proofBoundaryCandidates (doc : SourceDocument) (positions : Array ProofPosition)
    (budgets : ModuleQueryOutputBudgets) : Array ProofBoundaryCandidate × Bool := Id.run do
  let mut out : Array ProofBoundaryCandidate := #[]
  let mut truncated := false
  if let some first := positions[0]? then
    out := out.push (proofBoundaryCandidate doc "entry" first budgets)
  for pos in positions do
    if out.size < proofBoundaryCandidateLimit then
      out := out.push (proofBoundaryCandidate doc "after_tactic" pos budgets)
    else
      truncated := true
  pure (out, truncated)

/-- Where a `try_proof_step` candidate is spliced relative to the resolved
    tactic. Every selector except `entry` resolves to a tactic state and splices
    `after` it; `entry` anchors on the first tactic's pre-state and splices
    `before` it, so a from-scratch tactic block elaborates against the pristine
    entry goal. Kept orthogonal to *which* position is selected. -/
private inductive TacticPlacement where
  | before
  | after
  deriving Inhabited

private def selectorPlacement : ProofPositionSelector → TacticPlacement
  | .entry => .before
  | _ => .after

private def selectProofPosition (_doc : SourceDocument) (positions : Array ProofPosition)
    (selector : ProofPositionSelector) : Option ProofPosition :=
  match selector with
  | .default => positions[0]?
  | .index index => positions[index]?
  | .afterText text occurrence? =>
    let wanted := text.trimAscii
    let occurrence := occurrence?.getD 0
    let hits := positions.filter fun pos => pos.summary.tactic.value.trimAscii == wanted
    hits[occurrence]?
  -- The pristine entry goal is the first tactic's pre-state, so `entry` anchors
  -- on `positions[0]`; `selectorPlacement` makes the splice land *before* it.
  -- Requires at least one elaborated tactic to anchor that pre-state.
  | .entry => positions[0]?

private def prefixBeforeOccurrence? (haystack needle : String) (occurrence : Nat) : Option String :=
  let parts := haystack.splitOn needle
  if parts.length <= occurrence + 1 then
    none
  else
    some (String.intercalate needle (parts.take (occurrence + 1)))

private def sourceTextProofPosition? (doc : SourceDocument) (decl : DeclarationCandidate)
    (text : String) (occurrence? : Option Nat) : Option (ModuleSourceSpan × ProofPositionSummary) := do
  let bodyText ← doc.extractBodySpan? decl.bodySpanBodyCoords
  let occurrence := occurrence?.getD 0
  let before ← prefixBeforeOccurrence? bodyText text occurrence
  let (startLine, startColumn) :=
    lineColumnAfterText decl.bodySpanBodyCoords.startLine decl.bodySpanBodyCoords.startColumn before
  let (endLine, endColumn) := lineColumnAfterText startLine startColumn text
  let span : ModuleSourceSpan := { startLine, startColumn, endLine, endColumn }
  some (span, { index := occurrence, tactic := { value := text, truncated := false } })

/-- How a proof state's local hypotheses are rendered.

    `pretty` delaborates each hypothesis type/value through the same
    notation-aware path as `goals_*`; `raw` keeps the fully-elaborated structural
    `Expr` form (`_uniq.NNNN` and all) for expert callers; `skip` renders no
    locals at all — used by callers (verification, proof-step classification)
    that read only the goals and discard the locals, so they never pay for
    rendering a term they throw away. -/
inductive LocalsRendering where
  | skip
  | raw
  | pretty
  deriving Inhabited

/-- Render one local hypothesis expression. The `pretty` path runs inside the
    caller's `MetaM` context, so free variables resolve to their user names and
    notation fires; on any pretty-printer failure it falls back to the raw
    structural form rather than dropping the hypothesis. -/
private def renderLocalExpr (mode : LocalsRendering) (limit : Nat) (e : Expr) : MetaM RenderedInfo :=
  match mode with
  | .pretty =>
    try
      let fmt ← Meta.ppExpr e
      pure (renderStringBoundedWith limit fmt.pretty)
    catch _ =>
      pure (renderExprBoundedWith limit e)
  | _ => pure (renderExprBoundedWith limit e)

private def collectLocalContextMeta (mode : LocalsRendering) (limit : Nat) (lctx : LocalContext) :
    MetaM (Array LocalInfo) :=
  lctx.foldlM (init := #[]) fun acc localDecl => do
    if localDecl.isImplementationDetail then
      pure acc
    else
      let typeStr ← renderLocalExpr mode limit localDecl.type
      let value ← localDecl.value?.mapM (renderLocalExpr mode limit)
      pure <| acc.push {
        name := localDecl.userName.toString
        binderInfo := (repr localDecl.binderInfo).pretty
        typeStr
        value
      }

private def collectTermLocals (mode : LocalsRendering) (limit : Nat) (ctx : ContextInfo) (lctx : LocalContext) :
    IO (Array LocalInfo) :=
  match mode with
  | .skip => pure #[]
  | _ => ctx.runMetaM lctx (collectLocalContextMeta mode limit lctx)

private def collectGoalLocals (mode : LocalsRendering) (limit : Nat) (ctx : ContextInfo) (mctx : MetavarContext)
    (goals : List MVarId) : IO (Array LocalInfo) := do
  match mode with
  | .skip => pure #[]
  | _ =>
    let ctx := { ctx with mctx := mctx }
    ctx.runMetaM {} do
      match goals with
      | goal :: _ =>
        goal.withContext do
          let decl ← goal.getDecl
          collectLocalContextMeta mode limit decl.lctx
      | [] => pure #[]

/-- Locate the smallest tactic state covering a body cursor, folding the info
    trees once. Shared by the proof-state projections and the verification
    degradation screen so each pays a single traversal. -/
private def findTacticCandidate (doc : SourceDocument) (trees : PersistentArray InfoTree) (line column : Nat) :
    IO (Option TacticCandidate) := do
  let (bodyCursorLine, bodyCursorColumn) := doc.bodyCursor line column
  let mut goalAcc : GoalAtAcc := { line := bodyCursorLine, column := bodyCursorColumn }
  for tree in trees do
    goalAcc ← tree.foldInfoM (init := goalAcc) (collectGoalAt doc.bodyFileMap)
  pure goalAcc.best?

private def runProofState (doc : SourceDocument) (trees : PersistentArray InfoTree)
    (candidates : Array DeclarationCandidate) (line column : Nat) (budgets : ModuleQueryOutputBudgets)
    (localsMode : LocalsRendering) : IO ProofStateResult := do
  let safeEditCandidate? := declarationAt? doc candidates line column
  let safeEdit := safeEditCandidate?.map (·.info)
  let (proofBoundaries, proofBoundariesTruncated) ←
    match safeEditCandidate? with
    | some decl => do
      let positions ← collectTacticsInDeclaration doc decl trees
      pure (proofBoundaryCandidates doc positions budgets)
    | none => pure (#[], false)
  match ← findTacticCandidate doc trees line column with
  | some candidate =>
    if tacticCandidateDegraded candidate then
      return (ProofStateResult.unavailable "proof state unavailable: elaboration degraded under resource pressure"
        proofBoundaries proofBoundariesTruncated)
    let (before, bytesAfterBefore, truncBefore) ←
      renderGoals candidate.ctx candidate.mctxBefore candidate.goalsBefore budgets.perFieldBytes.toUSize 0
    let (after, _, truncAfter) ←
      renderGoals candidate.ctx candidate.mctxAfter candidate.goalsAfter budgets.perFieldBytes.toUSize bytesAfterBefore
    let locals ← collectGoalLocals localsMode budgets.perFieldBytes candidate.ctx candidate.mctxBefore candidate.goalsBefore
    let declName := candidate.ctx.parentDecl?.map (·.toString)
    pure <| .state {
      declarationName := declName
      namespaceName := candidate.ctx.currNamespace.toString
      safeEdit
      span := doc.bodySpanToFile candidate.span
      goalsBefore := before
      goalsAfter := after
      locals
      expectedType := none
      truncated := truncBefore || truncAfter
      proofBoundaries
      proofBoundariesTruncated
    }
  | none =>
    let (bodyCursorLine, bodyCursorColumn) := doc.bodyCursor line column
    let mut termAcc : TypeAtAcc := { line := bodyCursorLine, column := bodyCursorColumn }
    for tree in trees do
      termAcc ← tree.foldInfoM (init := termAcc) (collectTypeAt doc.bodyFileMap)
    match termAcc.best? with
    | none => pure <| (ProofStateResult.unavailable "no tactic or term context covers the requested cursor"
        proofBoundaries proofBoundariesTruncated)
    | some candidate =>
      let expectedType ← candidate.ctx.runMetaM candidate.lctx do
        try
          match candidate.expectedType? with
          | some expected => pure (some (renderExprBoundedWith budgets.perFieldBytes expected))
          | none =>
            let ty ← Meta.inferType candidate.expr
            pure (some (renderExprBoundedWith budgets.perFieldBytes ty))
        catch _ =>
          pure none
      let locals ← collectTermLocals localsMode budgets.perFieldBytes candidate.ctx candidate.lctx
      let declName := candidate.ctx.parentDecl?.map (·.toString)
      pure <| .state {
        declarationName := declName
        namespaceName := candidate.ctx.currNamespace.toString
        safeEdit
        span := doc.bodySpanToFile candidate.span
        goalsBefore := #[]
        goalsAfter := #[]
        locals
        expectedType
        truncated := expectedType.any (·.truncated)
        proofBoundaries
        proofBoundariesTruncated
      }

private def proofStateFromPosition (doc : SourceDocument) (decl : DeclarationCandidate)
    (pos : ProofPosition) (placement : TacticPlacement) (budgets : ModuleQueryOutputBudgets)
    (localsMode : LocalsRendering) (proofBoundaries : Array ProofBoundaryCandidate)
    (proofBoundariesTruncated : Bool) : IO ProofStateResult := do
  let candidate := pos.tactic
  if tacticCandidateDegraded candidate then
    return (ProofStateResult.unavailable "proof state unavailable: elaboration degraded under resource pressure"
      proofBoundaries proofBoundariesTruncated)
  let (before, bytesAfterBefore, truncBefore) ←
    renderGoals candidate.ctx candidate.mctxBefore candidate.goalsBefore budgets.perFieldBytes.toUSize 0
  -- At the pristine entry (`before`) no tactic has run, so the "after" state is
  -- still the entry goal: render `goalsBefore` for both, mirroring how the
  -- candidate would elaborate against an untouched goal.
  let (after, _, truncAfter) ← match placement with
    | .before =>
      renderGoals candidate.ctx candidate.mctxBefore candidate.goalsBefore budgets.perFieldBytes.toUSize bytesAfterBefore
    | .after =>
      renderGoals candidate.ctx candidate.mctxAfter candidate.goalsAfter budgets.perFieldBytes.toUSize bytesAfterBefore
  let locals ← collectGoalLocals localsMode budgets.perFieldBytes candidate.ctx candidate.mctxBefore candidate.goalsBefore
  pure <| .state {
    declarationName := some decl.info.declarationName
    namespaceName := decl.info.namespaceName
    safeEdit := some decl.info
    span := doc.bodySpanToFile candidate.span
    goalsBefore := before
    goalsAfter := after
    locals
    expectedType := none
    truncated := truncBefore || truncAfter
    proofBoundaries
    proofBoundariesTruncated
  }

private def runProofStateInDeclaration (doc : SourceDocument) (trees : PersistentArray InfoTree)
    (candidates : Array DeclarationCandidate) (name : String) (selector : ProofPositionSelector)
    (budgets : ModuleQueryOutputBudgets) (missing : Array String) (localsMode : LocalsRendering) :
    IO ProofStateResult := do
  match declarationTargetByName candidates name with
  | .target info =>
    let some decl := candidates.find? (fun candidate => candidate.info.declarationName == info.declarationName)
      | pure <| ProofStateResult.unavailable "resolved declaration is not available in the module snapshot" #[] false
    let positions ← collectTacticsInDeclaration doc decl trees
    let (proofBoundaries, proofBoundariesTruncated) := proofBoundaryCandidates doc positions budgets
    match selectProofPosition doc positions selector with
    | some pos =>
      proofStateFromPosition doc decl pos (selectorPlacement selector) budgets localsMode
        proofBoundaries proofBoundariesTruncated
    | none =>
      pure <| (ProofStateResult.unavailable "declaration has no proof position matching the selector"
        proofBoundaries proofBoundariesTruncated)
  -- The name did not resolve against this overlay. Distinguish a genuine
  -- not-found (the open environment is complete) from an incomplete
  -- environment whose missing imports may define the name elsewhere.
  | .notFound =>
    if missing.isEmpty then
      pure <| ProofStateResult.unavailable "declaration was not found in the module" #[] false
    else
      pure <| .needsBuild missing
  | .ambiguous candidates => pure <| .ambiguous candidates

private def emptyResultFor (query : ModuleQuery) : ModuleQueryResult :=
  match query with
  | .diagnostics => .diagnostics emptyFailure
  | .typeAt .. => .typeAt .noTerm
  | .goalAt .. => .goalAt .noTacticContext
  | .references .. => .references { references := #[], truncated := false }

private def countNewlines (s : String) : Nat :=
  s.foldl (init := 0) fun n c => if c == '\n' then n + 1 else n

private def SourceDocument.offsetDiagnosticPos (doc : SourceDocument)
    (pos : LeanRsFixture.Elaboration.DiagnosticPos) :
    LeanRsFixture.Elaboration.DiagnosticPos :=
  { pos with
    line := pos.line + doc.lineOffset
    endLine := pos.endLine.map (· + doc.lineOffset) }

private def SourceDocument.offsetDiagnostic (doc : SourceDocument)
    (diagnostic : LeanRsFixture.Elaboration.Diagnostic) :
    LeanRsFixture.Elaboration.Diagnostic :=
  { diagnostic with position := diagnostic.position.map (doc.offsetDiagnosticPos) }

private def SourceDocument.offsetFailure (doc : SourceDocument)
    (failure : LeanRsFixture.Elaboration.ElabFailure) :
    LeanRsFixture.Elaboration.ElabFailure :=
  if doc.lineOffset == 0 then
    failure
  else
    { failure with diagnostics := failure.diagnostics.map (doc.offsetDiagnostic) }

private def resultFor (query : ModuleQuery) (doc : SourceDocument) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (diagBytes : USize) (fileLabel : String) :
    IO ModuleQueryResult := do
  match query with
  | .diagnostics =>
    let failure ← failureFromMessages messages diagBytes fileLabel
    pure <| .diagnostics (doc.offsetFailure failure)
  | .typeAt line column =>
    pure <| .typeAt (← runTypeAt doc trees line column)
  | .goalAt line column =>
    pure <| .goalAt (← runGoalAt doc trees line column diagBytes)
  | .references name =>
    pure <| .references (← runReferences doc trees name)

private def selectorId : ModuleQuerySelector → String
  | .diagnostics id => id
  | .proofState id .. => id
  | .typeAt id .. => id
  | .references id .. => id
  | .declarationTarget id .. => id
  | .surroundingDeclaration id .. => id
  | .proofStateInDeclaration id .. => id
  | .declarationOutline id => id

private def renderedBytes (info : RenderedInfo) : Nat :=
  info.value.utf8ByteSize

private def failureBytes (failure : LeanRsFixture.Elaboration.ElabFailure) : Nat :=
  failure.diagnostics.foldl (init := 0) fun bytes diagnostic =>
    let severityBytes :=
      match diagnostic.severity with
      | .info => 4
      | .warning => 7
      | .error => 5
    bytes + diagnostic.message.utf8ByteSize + diagnostic.fileLabel.utf8ByteSize + severityBytes

private def localInfoBytes (info : LocalInfo) : Nat :=
  info.name.utf8ByteSize + info.binderInfo.utf8ByteSize + renderedBytes info.typeStr +
    match info.value with
    | some value => renderedBytes value
    | none => 0

private def spanBytes (_span : ModuleSourceSpan) : Nat := 32

private def declarationTargetInfoBytes (info : DeclarationTargetInfo) : Nat :=
  info.shortName.utf8ByteSize + info.declarationName.utf8ByteSize + info.namespaceName.utf8ByteSize +
    info.declarationKind.utf8ByteSize + spanBytes info.declarationSpan + spanBytes info.nameSpan + spanBytes info.bodySpan

private def proofBoundaryCandidateBytes (candidate : ProofBoundaryCandidate) : Nat :=
  8 + candidate.kind.utf8ByteSize + spanBytes candidate.source + renderedBytes candidate.excerpt

private def typeAtBytes : TypeAtResult → Nat
  | .noTerm => 0
  | .term span expr typeStr expectedType =>
    spanBytes span + renderedBytes expr + renderedBytes typeStr +
      match expectedType with
      | some expected => renderedBytes expected
      | none => 0

private def goalAtBytes : GoalAtResult → Nat
  | .noTacticContext => 0
  | .goal span before after _ =>
    spanBytes span + before.foldl (init := 0) (· + ·.utf8ByteSize) +
      after.foldl (init := 0) (· + ·.utf8ByteSize)

private def referencesBytes (result : ReferencesResult) : Nat :=
  result.references.foldl (init := 0) fun bytes node =>
    bytes + node.name.utf8ByteSize + 32

private def proofStateBytes : ProofStateResult → Nat
  | .unavailable message proofBoundaries _ =>
    message.utf8ByteSize +
      proofBoundaries.foldl (init := 0) (fun bytes candidate => bytes + proofBoundaryCandidateBytes candidate)
  | .ambiguous candidates =>
    candidates.foldl (init := 0) fun bytes candidate => bytes + declarationTargetInfoBytes candidate
  | .needsBuild missing =>
    missing.foldl (init := 0) fun bytes name => bytes + name.utf8ByteSize
  | .state info =>
    (match info.declarationName with | some name => name.utf8ByteSize | none => 0) +
      info.namespaceName.utf8ByteSize +
      (match info.safeEdit with | some target => declarationTargetInfoBytes target | none => 0) +
      spanBytes info.span +
      info.goalsBefore.foldl (init := 0) (· + ·.utf8ByteSize) +
      info.goalsAfter.foldl (init := 0) (· + ·.utf8ByteSize) +
      info.locals.foldl (init := 0) (fun bytes localInfo => bytes + localInfoBytes localInfo) +
      (match info.expectedType with | some expected => renderedBytes expected | none => 0) +
      info.proofBoundaries.foldl (init := 0) (fun bytes candidate => bytes + proofBoundaryCandidateBytes candidate)

private def declarationTargetBytes : DeclarationTargetResult → Nat
  | .target info => declarationTargetInfoBytes info
  | .notFound => 0
  | .ambiguous candidates =>
    candidates.foldl (init := 0) fun bytes candidate => bytes + declarationTargetInfoBytes candidate

private def declarationOutlineBytes (result : DeclarationOutlineResult) : Nat :=
  result.declarations.foldl (init := 0) fun bytes declaration =>
    bytes + declarationTargetInfoBytes declaration

private def surroundingDeclarationBytes : SurroundingDeclarationResult → Nat
  | .none => 0
  | .declaration info => declarationTargetInfoBytes info

private def batchResultBytes : ModuleQueryBatchResult → Nat
  | .diagnostics failure => failureBytes failure
  | .proofState result => proofStateBytes result
  | .typeAt result => typeAtBytes result
  | .references result => referencesBytes result
  | .declarationTarget result => declarationTargetBytes result
  | .surroundingDeclaration result => surroundingDeclarationBytes result
  | .declarationOutline result => declarationOutlineBytes result

private def selectorDeclarationOutline (candidates : Array DeclarationCandidate)
    (budgets : ModuleQueryOutputBudgets) : DeclarationOutlineResult := Id.run do
  let candidates := dedupByDeclarationName candidates
  let mut declarations : Array DeclarationTargetInfo := #[]
  let mut spent : Nat := 0
  let mut truncated := false
  for candidate in candidates do
    if !truncated then
      let bytes := declarationTargetInfoBytes candidate.info
      if spent + bytes > budgets.totalBytes then
        truncated := true
      else
        declarations := declarations.push candidate.info
        spent := spent + bytes
  pure { declarations, truncated }

private def batchResultFor (selector : ModuleQuerySelector) (doc : SourceDocument) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (candidates : Array DeclarationCandidate)
    (budgets : ModuleQueryOutputBudgets) (fileLabel : String) (missing : Array String) :
    IO ModuleQueryBatchResult := do
  match selector with
  | .diagnostics _ =>
    let failure ← failureFromMessages messages budgets.perFieldBytes.toUSize fileLabel
    pure <| .diagnostics (doc.offsetFailure failure)
  | .proofState _ line column =>
    pure <| .proofState (← runProofState doc trees candidates line column budgets .pretty)
  | .proofStateInDeclaration _ declaration position localsRaw =>
    let localsMode := if localsRaw != 0 then LocalsRendering.raw else LocalsRendering.pretty
    pure <| .proofState (← runProofStateInDeclaration doc trees candidates declaration position budgets missing localsMode)
  | .typeAt _ line column =>
    pure <| .typeAt (← runTypeAtWith doc trees line column budgets.perFieldBytes)
  | .references _ name =>
    pure <| .references (← runReferences doc trees name)
  | .declarationTarget _ name? line? column? =>
    pure <| .declarationTarget (selectorDeclarationTarget doc candidates name? line? column?)
  | .surroundingDeclaration _ line column =>
    let result :=
      match declarationAt? doc candidates line column with
      | some candidate => .declaration candidate.info
      | none => .none
    pure <| .surroundingDeclaration result
  | .declarationOutline _ =>
    pure <| .declarationOutline (selectorDeclarationOutline candidates budgets)

private def emptyBatchResultFor (selector : ModuleQuerySelector) : ModuleQueryBatchResult :=
  match selector with
  | .diagnostics _ => .diagnostics emptyFailure
  | .proofState _ .. =>
    .proofState (ProofStateResult.unavailable "module processing did not reach the requested context" #[] false)
  | .proofStateInDeclaration _ .. =>
    .proofState (ProofStateResult.unavailable "module processing did not reach the requested declaration" #[] false)
  | .typeAt _ .. => .typeAt .noTerm
  | .references _ .. => .references { references := #[], truncated := false }
  | .declarationTarget _ .. => .declarationTarget .notFound
  | .surroundingDeclaration _ .. => .surroundingDeclaration .none
  | .declarationOutline _ => .declarationOutline { declarations := #[], truncated := false }

private def batchEnvelopeFromFailure (selectors : Array ModuleQuerySelector)
    (failure : LeanRsFixture.Elaboration.ElabFailure) : ModuleQueryBatchEnvelope :=
  let items := selectors.map fun selector =>
    match selector with
    | .diagnostics id => .ok id (.diagnostics failure)
    | _ => .ok (selectorId selector) (emptyBatchResultFor selector)
  { items, totalTruncated := false }

/-- Empty (not failed) results for every selector. Used when an incomplete
    import closure short-circuits elaboration: the parent reads `missing` to
    degrade to `needs_build` / `files_skipped` and ignores this payload. -/
private def emptyBatchEnvelope (selectors : Array ModuleQuerySelector) : ModuleQueryBatchEnvelope :=
  { items := selectors.map fun selector => .ok (selectorId selector) (emptyBatchResultFor selector)
    totalTruncated := false }

private def runBatchSelectorsWithCandidates (selectors : Array ModuleQuerySelector) (doc : SourceDocument) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (candidates : Array DeclarationCandidate)
    (budgets : ModuleQueryOutputBudgets) (fileLabel : String) (missing : Array String) :
    IO ModuleQueryBatchEnvelope := do
  let totalLimit := budgets.totalBytes
  let mut items : Array ModuleQueryBatchItem := #[]
  let mut spent : Nat := 0
  let mut totalTruncated := false
  for selector in selectors do
    let id := selectorId selector
    if spent >= totalLimit then
      items := items.push (.budgetExceeded id "module query batch total byte budget exhausted")
      totalTruncated := true
    else
      try
        let result ← batchResultFor selector doc messages trees candidates budgets fileLabel missing
        let bytes := batchResultBytes result
        if spent + bytes > totalLimit then
          items := items.push (.budgetExceeded id "module query selector would exceed the batch total byte budget")
          totalTruncated := true
        else
          items := items.push (.ok id result)
          spent := spent + bytes
      catch ex =>
        items := items.push (.unavailable id (toString ex))
  pure { items, totalTruncated }

private def runBatchSelectors (selectors : Array ModuleQuerySelector) (doc : SourceDocument) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (budgets : ModuleQueryOutputBudgets) (fileLabel : String)
    (missing : Array String) :
    IO ModuleQueryBatchEnvelope := do
  let candidates ← declarationCandidates doc trees
  runBatchSelectorsWithCandidates selectors doc messages trees candidates budgets fileLabel missing

private def batchEnvelopeBytes (envelope : ModuleQueryBatchEnvelope) : Nat :=
  envelope.items.foldl (init := 0) fun bytes item =>
    bytes +
      match item with
      | .ok id result => id.utf8ByteSize + batchResultBytes result
      | .unavailable id message => id.utf8ByteSize + message.utf8ByteSize
      | .budgetExceeded id message => id.utf8ByteSize + message.utf8ByteSize

private def elapsedMicrosSince (startMs : Nat) : IO Nat := do
  let nowMs ← IO.monoMsNow
  pure ((nowMs - startMs) * 1000)

private def u64 (n : Nat) : UInt64 :=
  n.toUInt64

private structure ModuleSnapshot where
  fileIdentity : String
  key : String
  document : SourceDocument
  messages : MessageLog
  trees : PersistentArray InfoTree
  candidates : Array DeclarationCandidate
  imports : Array String
  missing : Array String
  /-- The environment after elaborating the overlay, used to walk a verified
      declaration's axiom dependencies. The `InfoTree`s already pin this env
      graph transitively, so storing the reference adds negligible memory
      beyond the trees already retained. -/
  env : Environment
  approxBytes : Nat
  lastUsedMs : Nat

private structure ModuleSnapshotCacheState where
  entries : Array ModuleSnapshot := #[]
  approxBytes : Nat := 0
  deriving Inhabited

private instance : Nonempty (IO.Ref ModuleSnapshotCacheState) :=
  inferInstanceAs <| Nonempty (IO.Ref _)

initialize moduleSnapshotCache : IO.Ref ModuleSnapshotCacheState ← IO.mkRef {}

private def approxSnapshotBytes (source : String) (_messages : MessageLog) (trees : PersistentArray InfoTree)
    (candidates : Array DeclarationCandidate) : Nat :=
  source.utf8ByteSize * 8 + trees.size * 4096 + candidates.size * 256 + 65536

private def cacheFacts (status : ModuleQueryCacheStatus) (timings : ModuleQueryTimings) (outputBytes : Nat)
    (cache : ModuleSnapshotCacheState) : ModuleQueryCacheFacts :=
  {
    cacheStatus := status
    timings
    outputBytes := u64 outputBytes
    cacheEntryCount := some cache.entries.size
    cacheApproxBytes := some cache.approxBytes
  }

private def wrapCachedOutcome (snapshot : ModuleSnapshot) (envelope : ModuleQueryBatchEnvelope)
    (facts : ModuleQueryCacheFacts) : ModuleQueryBatchCachedOutcome :=
  if snapshot.missing.isEmpty then
    .ok envelope snapshot.imports facts
  else
    .missingImports envelope snapshot.imports snapshot.missing facts

private def headerFailureCachedOutcome (failure : LeanRsFixture.Elaboration.ElabFailure)
    (status : ModuleQueryCacheStatus) (headerMicros : Nat) (cache : ModuleSnapshotCacheState) :
    ModuleQueryBatchCachedOutcome :=
  let timings : ModuleQueryTimings := {
    headerImportMicros := u64 headerMicros
    elaborationMicros := 0
    projectionMicros := 0
    renderingMicros := 0
  }
  .headerParseFailed failure (cacheFacts status timings (failureBytes failure) cache)

private def pruneExpired (nowMs : Nat) (ttlMillis : Nat) (entries : Array ModuleSnapshot) :
    Array ModuleSnapshot × Nat × Bool :=
  if ttlMillis == 0 then
    (#[], entries.foldl (init := 0) (fun bytes e => bytes + e.approxBytes), !entries.isEmpty)
  else
    entries.foldl (init := (#[], 0, false)) fun (kept, removedBytes, removedAny) entry =>
      if nowMs - entry.lastUsedMs >= ttlMillis then
        (kept, removedBytes + entry.approxBytes, true)
      else
        (kept.push entry, removedBytes, removedAny)

private partial def enforceCacheLimits (maxEntries maxBytes : Nat) (entries : Array ModuleSnapshot) :
    Array ModuleSnapshot × Nat × Bool :=
  let totalBytes := entries.foldl (init := 0) (fun bytes e => bytes + e.approxBytes)
  if entries.size <= maxEntries && totalBytes <= maxBytes then
    (entries, totalBytes, false)
  else
    match entries with
    | #[] => (#[], 0, false)
    | arr =>
      let oldest? := arr.foldl (init := none) fun best? entry =>
        match best? with
        | none => some entry
        | some best => if entry.lastUsedMs < best.lastUsedMs then some entry else some best
      match oldest? with
      | none => (#[], 0, false)
      | some oldest =>
        enforceCacheLimits maxEntries maxBytes (arr.filter (fun entry => entry.key != oldest.key))

private def pruneCache (policy : ModuleQueryCachePolicy) (nowMs : Nat) (cache : ModuleSnapshotCacheState) :
    ModuleSnapshotCacheState × Bool :=
  let maxEntries := policy.maxEntries.toNat.max 1
  let maxBytes := policy.maxBytes.toNat.max 1
  let (ttlEntries, _ttlBytes, ttlRemoved) := pruneExpired nowMs policy.ttlMillis.toNat cache.entries
  let (boundedEntries, boundedBytes, limitRemoved) := enforceCacheLimits maxEntries maxBytes ttlEntries
  ({ entries := boundedEntries, approxBytes := boundedBytes }, ttlRemoved || limitRemoved)

private def findSnapshot? (key : String) (entries : Array ModuleSnapshot) : Option ModuleSnapshot :=
  entries.find? (fun entry => entry.key == key)

private def hasFileIdentity (fileIdentity : String) (entries : Array ModuleSnapshot) : Bool :=
  entries.any (fun entry => entry.fileIdentity == fileIdentity)

private def upsertSnapshot (snapshot : ModuleSnapshot) (entries : Array ModuleSnapshot) : Array ModuleSnapshot :=
  (entries.filter (fun entry => entry.key != snapshot.key)).push snapshot

private def buildModuleSnapshot (env : Environment) (source namespaceContext fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize) (policy : ModuleQueryCachePolicy) :
    IO (Except (LeanRsFixture.Elaboration.ElabFailure × Nat) (ModuleSnapshot × ModuleQueryTimings)) := do
  let headerStart ← IO.monoMsNow
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let inputCtx := Parser.mkInputContext source fileLabel
  let (header, parserState, headerMessages) ← Lean.Parser.parseHeader inputCtx
  if headerMessages.hasErrors then
    let elapsed ← elapsedMicrosSince headerStart
    return .error (← failureFromMessages headerMessages diagBytes fileLabel, elapsed)

  let userImports := (Lean.Elab.headerToImports header (includeInit := false)).map (·.module.toString)
  let loadedModules := env.header.moduleNames.map (·.toString)
  let missing := userImports.filter (fun nm => ! loadedModules.contains nm)
  let (commandEnv, initialMessages) ←
    if Lean.Elab.HeaderSyntax.isModule header && missing.isEmpty then
      try
        unsafe Lean.enableInitializersExecution
        Lean.Elab.processHeader header opts headerMessages inputCtx (mainModule := Name.anonymous)
      catch ex =>
        let elapsed ← elapsedMicrosSince headerStart
        return .error (LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel, elapsed)
    else
      pure (env, headerMessages)
  let headerMicros ← elapsedMicrosSince headerStart

  let mut commandState : Command.State := Command.mkState commandEnv initialMessages opts
  commandState := { commandState with infoState.enabled := true }
  if !namespaceContext.isEmpty then
    let head := commandState.scopes.headD { header := "", opts }
    commandState := { commandState with scopes := [{ head with currNamespace := namespaceContext.toName }] }
  let elabStart ← IO.monoMsNow
  let headerSource := String.Pos.Raw.extract source 0 parserState.pos
  let bodySource := String.Pos.Raw.extract source parserState.pos source.rawEndPos
  let lineOffset := countNewlines headerSource
  let document := SourceDocument.fromSources source bodySource lineOffset
  -- Incomplete import closure: the open environment is missing this file's
  -- imports (`processHeader` was already skipped above), so a full
  -- `processCommands` run would only emit unknown-symbol errors, and the parent
  -- discards that output as a `needs_build` / `files_skipped` degrade. Skip the
  -- body elaboration so a project-scope scan pays O(parse-header) per
  -- incomplete file instead of a full failing elaboration. The unprocessed
  -- `commandState` supplies a genuinely empty info-tree snapshot, and
  -- `elaborationMicros` stays 0 — the per-file cost attribution the caller reads.
  if !missing.isEmpty then
    let nowMs ← IO.monoMsNow
    let snapshot : ModuleSnapshot := {
      fileIdentity := policy.fileIdentity
      key := policy.key
      document
      messages := commandState.messages
      trees := commandState.infoState.trees
      candidates := #[]
      imports := userImports
      missing
      env := commandState.env
      approxBytes := approxSnapshotBytes source commandState.messages commandState.infoState.trees #[]
      lastUsedMs := nowMs
    }
    return .ok (snapshot, {
      headerImportMicros := u64 headerMicros
      elaborationMicros := 0
      projectionMicros := 0
      renderingMicros := 0
    })
  try
    let bodyInputCtx := Parser.mkInputContext bodySource fileLabel
    let st ← Lean.Elab.IO.processCommands bodyInputCtx { : Parser.ModuleParserState } commandState
    let elabMicros ← elapsedMicrosSince elabStart
    let finalCmdState := st.commandState
    let candidates ← declarationCandidates document finalCmdState.infoState.trees
    let approxBytes := approxSnapshotBytes source finalCmdState.messages finalCmdState.infoState.trees candidates
    let nowMs ← IO.monoMsNow
    let snapshot : ModuleSnapshot := {
      fileIdentity := policy.fileIdentity
      key := policy.key
      document
      messages := finalCmdState.messages
      trees := finalCmdState.infoState.trees
      candidates
      imports := userImports
      missing
      env := finalCmdState.env
      approxBytes
      lastUsedMs := nowMs
    }
    pure <| .ok (snapshot, {
      headerImportMicros := u64 headerMicros
      elaborationMicros := u64 elabMicros
      projectionMicros := 0
      renderingMicros := 0
    })
  catch ex =>
    let elapsed ← elapsedMicrosSince headerStart
    return .error (LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel, elapsed)

private def proofCandidateLimit : Nat := 16

private def oneShotPolicy (source fileLabel : String) : ModuleQueryCachePolicy :=
  {
    fileIdentity := fileLabel
    key := source
    maxEntries := 1
    ttlMillis := 0
    maxBytes := 1
  }

private def SourceDocument.replaceFileSpan? (doc : SourceDocument) (replacement : String) (span : ModuleSourceSpan) :
    Option String :=
  match doc.rawOffset? span.startLine span.startColumn,
      doc.rawOffset? span.endLine span.endColumn with
  | some start, some stop => some (doc.replaceRawSpan replacement start stop)
  | _, _ => none

private def SourceDocument.insertAt? (doc : SourceDocument) (insertion : String) (line column : Nat) : Option String :=
  match doc.rawOffset? line column with
  | some pos => some (doc.replaceRawSpan insertion pos pos)
  | none => none

private def trimTrailingNewlines (text : String) : String :=
  Id.run do
    let mut chars := text.toList.reverse
    for ch in text.toList.reverse do
      if ch == '\n' || ch == '\r' then
        chars := chars.drop 1
      else
        return chars.reverse.foldl (init := "") (fun out ch => out.push ch)
    ""

private def trimLeftAscii (text : String) : String :=
  Id.run do
    let mut skipping := true
    let mut out := ""
    for ch in text.toList do
      if skipping && (ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r') then
        pure ()
      else
        skipping := false
        out := out.push ch
    out

private def trimAsciiSimple (text : String) : String :=
  trimTrailingNewlines (trimLeftAscii text)

private def stripOptionalByLine (text : String) : String :=
  let trimmed := trimAsciiSimple text
  if trimmed == "by" then
    ""
  else if trimmed.startsWith "by\n" then
    String.Pos.Raw.extract trimmed ⟨3⟩ trimmed.rawEndPos
  else if trimmed.startsWith "by\r\n" then
    String.Pos.Raw.extract trimmed ⟨4⟩ trimmed.rawEndPos
  else
    text

private def indentFragment (indent text : String) : String :=
  let text := stripOptionalByLine text |> trimTrailingNewlines
  let lines := text.splitOn "\n"
  String.intercalate "\n" <| lines.map fun line =>
    if trimAsciiSimple line |>.isEmpty then
      ""
    else
      indent ++ trimLeftAscii line

private structure ProofInsertionSite where
  tacticBodySpan : ModuleSourceSpan
  tacticFileSpan : ModuleSourceSpan
  insertedBodySpan : ModuleSourceSpan
  insertedFileSpan : ModuleSourceSpan
  declaration : Option DeclarationTargetInfo
  proofPosition : Option ProofPositionSummary

private def SourceDocument.diagnosticStartsInBodySpan
    (_doc : SourceDocument) (span : ModuleSourceSpan) (diagnostic : LeanRsFixture.Elaboration.Diagnostic) : Bool :=
  match diagnostic.position with
  | none => false
  | some pos => SourceDocument.bodySpanContains span pos.line pos.column

private def SourceDocument.splitFailureAtBodySpan
    (doc : SourceDocument) (span : ModuleSourceSpan) (failure : LeanRsFixture.Elaboration.ElabFailure) :
    LeanRsFixture.Elaboration.ElabFailure × LeanRsFixture.Elaboration.ElabFailure :=
  let localDiags := failure.diagnostics.filter (doc.diagnosticStartsInBodySpan span)
  let downstream := failure.diagnostics.filter (fun diagnostic => !doc.diagnosticStartsInBodySpan span diagnostic)
  ({ failure with diagnostics := localDiags }, { failure with diagnostics := downstream })

/-- A resolved proof position: a source span at a positive (1-based) column,
    plus where the candidate is spliced relative to it. `tacticInsertion?`
    accepts only this and derives the candidate's indentation from
    `span.startColumn`, so a candidate can never be spliced at column 0 — a
    top-level/command position that Lean ≥ 4.31 rejects. The bare `by` atom is
    excluded earlier, in `collectTacticsInDeclaration`. -/
private structure SelectedTactic where
  span : ModuleSourceSpan
  placement : TacticPlacement
  colPos : 0 < span.startColumn

/-- Build a `SelectedTactic`, witnessing a positive column. Every span fed here
    comes from `rangeOfStx`/`lineColumnAfterText`, whose columns are `_ + 1`, so
    `none` is unreachable in practice; it keeps the invariant total without
    leaking those internals into callers. -/
private def SelectedTactic.ofSpan? (placement : TacticPlacement) (span : ModuleSourceSpan) :
    Option SelectedTactic :=
  if h : 0 < span.startColumn then some { span, placement, colPos := h } else none

private def resolveEditTarget (snapshot : ModuleSnapshot) (edit : ProofEditTarget) :
    IO (Except String (SelectedTactic × Option DeclarationTargetInfo × Option ProofPositionSummary × Option TacticCandidate)) := do
  match edit with
  | .declaration name selector =>
    match declarationTargetByName snapshot.candidates name with
    | .target info =>
      let some decl := snapshot.candidates.find? (fun candidate => candidate.info.declarationName == info.declarationName)
        | pure <| .error "resolved declaration is not available in the module snapshot"
      let positions ← collectTacticsInDeclaration snapshot.document decl snapshot.trees
      match selectProofPosition snapshot.document positions selector with
      | some pos =>
        match SelectedTactic.ofSpan? (selectorPlacement selector) pos.tactic.span with
        | some sel => pure <| .ok (sel, some decl.info, some pos.summary, some pos.tactic)
        | none => pure <| .error "selected proof position is at an invalid column"
      | none =>
        match selector with
        | .afterText text occurrence? =>
          match sourceTextProofPosition? snapshot.document decl text occurrence? with
          | some (span, summary) =>
            -- The source-text fallback always splices after the matched text. It
            -- carries no elaborated tactic state, so the envelope's entry goals
            -- and locals come out empty for this path.
            match SelectedTactic.ofSpan? .after span with
            | some sel => pure <| .ok (sel, some decl.info, some summary, none)
            | none => pure <| .error "selected proof position is at an invalid column"
          | none => pure <| .error "declaration has no proof position matching the selector"
        | _ => pure <| .error "declaration has no proof position matching the selector"
    | .notFound => pure <| .error "declaration was not found in the module"
    | .ambiguous _ => pure <| .error "declaration name is ambiguous in the module"

private def SourceDocument.tacticInsertion? (doc : SourceDocument) (selected : SelectedTactic)
    (declaration : Option DeclarationTargetInfo) (proofPosition : Option ProofPositionSummary) (text : String) :
    Option (String × ProofInsertionSite) := do
  let span := selected.span
  let fileSpan := doc.bodySpanToFile span
  -- Indent the candidate to the selected tactic's own column, not the end line's
  -- leading whitespace. `selected.colPos` guarantees a positive column, so the
  -- candidate aligns with a sibling tactic the parser already accepted
  -- (`tacticSeq1Indented`/`colGe`) and never lands at column 0. This placement is
  -- robust on both lenient (≤ 4.30) and strict (≥ 4.31) tactic-block parsers.
  let indent := "".pushn ' ' (span.startColumn - 1)
  match selected.placement with
  | .after =>
    -- Splice on a fresh line after the selected tactic; the candidate runs
    -- against that tactic's `goalsAfter`.
    let fragment := "\n" ++ indentFragment indent text
    let pos ← doc.rawOffset? fileSpan.endLine fileSpan.endColumn
    let overlay := doc.replaceRawSpan fragment pos pos
    let (fileEndLine, fileEndColumn) := lineColumnAfterText fileSpan.endLine fileSpan.endColumn fragment
    let (bodyEndLine, bodyEndColumn) := lineColumnAfterText span.endLine span.endColumn fragment
    let insertedFileSpan := {
      startLine := fileSpan.endLine, startColumn := fileSpan.endColumn,
      endLine := fileEndLine, endColumn := fileEndColumn
    }
    let insertedBodySpan := {
      startLine := span.endLine, startColumn := span.endColumn,
      endLine := bodyEndLine, endColumn := bodyEndColumn
    }
    some (overlay, {
      tacticBodySpan := span,
      tacticFileSpan := fileSpan,
      insertedBodySpan,
      insertedFileSpan,
      declaration,
      proofPosition
    })
  | .before =>
    -- Splice before the first tactic, on its own line(s) aligned to the tactic's
    -- column, so a from-scratch tactic block runs against the pristine entry
    -- goal. Insert at the start of the tactic's line (column 1 — a valid raw
    -- offset; only column 0 is rejected): `body` carries its own `indent`, then a
    -- newline leaves the original tactic line below it untouched, never dedented.
    let body := indentFragment indent text
    let fragment := body ++ "\n"
    let pos ← doc.rawOffset? fileSpan.startLine 1
    let overlay := doc.replaceRawSpan fragment pos pos
    -- The inserted span covers just the candidate `body` (not the trailing
    -- newline), so the post-splice cursor lands at the end of the candidate's
    -- last tactic and `splitFailureAtBodySpan` attributes the original tactics'
    -- downstream errors (e.g. "no goals" once the candidate closes them) as
    -- downstream rather than candidate-local.
    let (fileEndLine, fileEndColumn) := lineColumnAfterText fileSpan.startLine 1 body
    let (bodyEndLine, bodyEndColumn) := lineColumnAfterText span.startLine 1 body
    let insertedFileSpan := {
      startLine := fileSpan.startLine, startColumn := 1,
      endLine := fileEndLine, endColumn := fileEndColumn
    }
    let insertedBodySpan := {
      startLine := span.startLine, startColumn := 1,
      endLine := bodyEndLine, endColumn := bodyEndColumn
    }
    some (overlay, {
      tacticBodySpan := span,
      tacticFileSpan := fileSpan,
      insertedBodySpan,
      insertedFileSpan,
      declaration,
      proofPosition
    })

private def diagnosticHasError (failure : LeanRsFixture.Elaboration.ElabFailure) : Bool :=
  failure.diagnostics.any fun diagnostic =>
    match diagnostic.severity with
    | .error => true
    | _ => false

private def diagnosticMentionsHeartbeat (failure : LeanRsFixture.Elaboration.ElabFailure) : Bool :=
  failure.diagnostics.any fun diagnostic =>
    let msg := diagnostic.message
    (msg.splitOn "heartbeat").length > 1 || (msg.splitOn "Heartbeats").length > 1 ||
      (msg.splitOn "maximum number of heartbeats").length > 1

/-- The elaboration hit a resource ceiling other than heartbeats — a recursion
    depth blow-up, an explicit interruption, or an allocation failure. Unlike a
    genuine "name absent" result, a `notFound` accompanied by one of these is a
    job the worker could not complete, not a trustworthy answer; the caller
    relabels it to `budgetExceeded`. -/
private def diagnosticIndicatesResourceLimit (failure : LeanRsFixture.Elaboration.ElabFailure) : Bool :=
  failure.diagnostics.any fun diagnostic =>
    let msg := diagnostic.message
    (msg.splitOn "deep recursion").length > 1 ||
      (msg.splitOn "maximum recursion depth").length > 1 ||
      (msg.splitOn "(interrupted)").length > 1 ||
      (msg.splitOn "out of memory").length > 1 ||
      (msg.splitOn "excessive memory").length > 1

private def renderedGoalInfos (goals : Array String) (limit : Nat) : Array RenderedInfo :=
  goals.map fun goal =>
    if goal.utf8ByteSize ≤ limit then
      { value := goal, truncated := false }
    else
      let st := goal.foldl (init := { limit := limit }) fun st c => appendBounded c.toString st
      { value := st.out, truncated := true }

private def proofStateGoalsAfter (result : ProofStateResult) (limit : Nat) : Array RenderedInfo × Bool :=
  match result with
  | .state info =>
    (renderedGoalInfos info.goalsAfter limit, info.truncated)
  | .unavailable .. | .ambiguous _ | .needsBuild _ => (#[], false)

private def attemptRowBytes (row : ProofAttemptRow) : Nat :=
  row.id.utf8ByteSize + renderedBytes row.candidateText +
    failureBytes row.diagnostics + failureBytes row.downstreamDiagnostics +
    row.goals.foldl (init := 0) (fun bytes goal => bytes + renderedBytes goal) +
    match row.declaration with
    | some info => declarationTargetInfoBytes info
    | none => 0

private def classifyAttempt
    (failure : LeanRsFixture.Elaboration.ElabFailure) (goals : Array RenderedInfo) : ProofAttemptStatus :=
  if diagnosticMentionsHeartbeat failure then
    .timeout
  else if diagnosticHasError failure then
    .failed
  else if goals.isEmpty then
    .closed
  else
    .progressed

private def attemptCandidate
    (env : Environment) (request : ProofAttemptRequest) (namespaceContext fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize) (document : SourceDocument) (selected : SelectedTactic)
    (declaration : Option DeclarationTargetInfo) (proofPosition : Option ProofPositionSummary)
    (candidate : ProofCandidate) : IO ProofAttemptRow := do
  let candidateText := renderStringBoundedWith request.budgets.perFieldBytes candidate.text
  let some (overlay, site) := document.tacticInsertion? selected declaration proofPosition candidate.text
    | return {
        id := candidate.id, status := .failed,
        candidateText,
        diagnostics := LeanRsFixture.Elaboration.singleErrorFailure "proof edit target is outside the source text" fileLabel,
        downstreamDiagnostics := emptyFailure,
        goals := #[], declaration, proofPosition, outputTruncated := candidateText.truncated
      }
  match ← buildModuleSnapshot env overlay namespaceContext fileLabel heartbeats diagBytes (oneShotPolicy overlay fileLabel) with
  | .error (failure, _) =>
    let (localFailure, downstreamFailure) := document.splitFailureAtBodySpan site.insertedBodySpan failure
    return {
      id := candidate.id,
      status := if diagnosticMentionsHeartbeat failure then .timeout else .failed,
      candidateText,
      diagnostics := localFailure,
      goals := #[],
      downstreamDiagnostics := downstreamFailure,
      declaration := site.declaration,
      proofPosition := site.proofPosition,
      outputTruncated := candidateText.truncated
    }
  | .ok (snapshot, _) =>
    let failure ← failureFromMessages snapshot.messages request.budgets.perFieldBytes.toUSize fileLabel
    let (localFailure, downstreamFailure) := snapshot.document.splitFailureAtBodySpan site.insertedBodySpan failure
    let cursorLine := site.insertedFileSpan.endLine
    let cursorColumn := site.insertedFileSpan.endColumn
    let proofState ←
      runProofState snapshot.document snapshot.trees snapshot.candidates cursorLine cursorColumn request.budgets .skip
    let (goals, proofStateTruncated) := proofStateGoalsAfter proofState request.budgets.perFieldBytes
    let status := classifyAttempt localFailure goals
    return {
      id := candidate.id,
      status,
      candidateText,
      diagnostics := localFailure,
      downstreamDiagnostics := downstreamFailure,
      goals,
      declaration := site.declaration,
      proofPosition := site.proofPosition,
      outputTruncated := candidateText.truncated || proofStateTruncated ||
        (match localFailure.truncated with
        | LeanRsFixture.Elaboration.Truncation.truncated => true
        | LeanRsFixture.Elaboration.Truncation.complete => false) ||
        (match downstreamFailure.truncated with
        | LeanRsFixture.Elaboration.Truncation.truncated => true
        | LeanRsFixture.Elaboration.Truncation.complete => false)
    }

private def candidateCapRows (candidates : Array ProofCandidate) : Array ProofCandidate × Bool :=
  (candidates.extract 0 (min candidates.size proofCandidateLimit), candidates.size > proofCandidateLimit)

private def attemptEnvelope
    (env : Environment) (request : ProofAttemptRequest) (namespaceContext fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize) (base : ModuleSnapshot) :
    IO ProofAttemptEnvelope := do
  let (candidates, candidatesTruncated) := candidateCapRows request.candidates
  match ← resolveEditTarget base request.edit with
  | .error message =>
    let rows := candidates.map fun candidate => {
          id := candidate.id,
          status := ProofAttemptStatus.failed,
          candidateText := renderStringBoundedWith request.budgets.perFieldBytes candidate.text,
          diagnostics := LeanRsFixture.Elaboration.singleErrorFailure message fileLabel,
          downstreamDiagnostics := emptyFailure,
          goals := #[],
          declaration := none,
          proofPosition := none,
          outputTruncated := (renderStringBoundedWith request.budgets.perFieldBytes candidate.text).truncated
        }
    pure { candidates := rows, candidateLimit := proofCandidateLimit, candidatesTruncated,
           entryGoals := #[], locals := #[] }
  | .ok (selected, declaration, proofPosition, entry?) =>
    -- Render the entry state once per batch: the selected tactic's pre-state
    -- (`goalsBefore`), exactly what the proof-position query reports as
    -- `goalsBefore` and `locals` at the same position, through the same
    -- `renderGoals` / `collectGoalLocals` machinery and its default `.pretty`
    -- locals mode. A degraded entry state, or the source-text fallback that
    -- carries no tactic state, yields empty arrays — never an error, and never
    -- blocks the attempt itself.
    let (entryGoals, locals) ← match entry? with
      | some entryCandidate =>
        if tacticCandidateDegraded entryCandidate then
          pure (#[], #[])
        else do
          let (rendered, _, _) ←
            renderGoals entryCandidate.ctx entryCandidate.mctxBefore entryCandidate.goalsBefore
              request.budgets.perFieldBytes.toUSize 0
          let entryLocals ←
            collectGoalLocals .pretty request.budgets.perFieldBytes entryCandidate.ctx
              entryCandidate.mctxBefore entryCandidate.goalsBefore
          pure (renderedGoalInfos rendered request.budgets.perFieldBytes, entryLocals)
      | none => pure (#[], #[])
    let mut rows : Array ProofAttemptRow := #[]
    let mut spent : Nat := 0
    for candidate in candidates do
      if spent ≥ request.budgets.totalBytes then
        rows := rows.push {
          id := candidate.id,
          status := .notAttempted,
          candidateText := renderStringBoundedWith request.budgets.perFieldBytes candidate.text,
          diagnostics := emptyFailure,
          downstreamDiagnostics := emptyFailure,
          goals := #[],
          declaration,
          proofPosition,
          outputTruncated := true
        }
      else
        let row ←
          attemptCandidate env request namespaceContext fileLabel heartbeats diagBytes base.document selected
            declaration proofPosition candidate
        let bytes := attemptRowBytes row
        if spent + bytes > request.budgets.totalBytes then
          rows := rows.push { row with status := .budgetExceeded, outputTruncated := true }
          spent := request.budgets.totalBytes
        else
          rows := rows.push row
          spent := spent + bytes
    pure { candidates := rows, candidateLimit := proofCandidateLimit, candidatesTruncated,
           entryGoals, locals }

private def containsSubstr (text needle : String) : Bool :=
  (text.splitOn needle).length > 1

private def resolveVerificationTarget (snapshot : ModuleSnapshot) (target : DeclarationVerificationTarget) :
    DeclarationTargetResult :=
  match target with
  | .name name => declarationTargetByName snapshot.candidates name
  | .span span => declarationTargetByPosition snapshot.document snapshot.candidates span.startLine span.startColumn

/-- Walk a verified declaration's axiom dependencies via `Lean.collectAxioms`
    over the overlay's elaborated environment, bounded by the same heartbeat
    budget as elaboration and by `perFieldBytes`. Returns `none` when the walk
    could not run (the constant is absent or the budget tripped before any
    result) so the caller can report "axioms unavailable" rather than an empty
    list that reads as "no axioms". The returned `Bool` is `true` when the byte
    budget truncated an otherwise-successful walk. -/
private def collectAxiomNames (env : Environment) (declName : Name) (heartbeats : UInt64) (perFieldBytes : Nat) :
    IO (Option (Array String × Bool)) := do
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let coreCtx : Core.Context := { fileName := "<verify-axioms>", fileMap := default, options := opts }
  let coreState : Core.State := { env }
  let action : CoreM (Array Name) := collectAxioms declName
  match ← ((action coreCtx).run' coreState).toBaseIO with
  | .ok names =>
    let mut out : Array String := #[]
    let mut spent := 0
    let mut truncated := false
    for name in names do
      let rendered := name.toString
      if spent + rendered.utf8ByteSize > perFieldBytes then
        truncated := true
      else
        out := out.push rendered
        spent := spent + rendered.utf8ByteSize
    pure (some (out, truncated))
  | .error _ => pure none

/-- Build the verification facts for a resolved target, given the tactic state
    already located by the caller (`candidate?`; `none` for a term-mode proof or
    an unresolved/ambiguous target).

    The returned `Bool` is the *degraded* flag: when the captured proof state
    carries dangling metavariables (an interrupted elaboration under resource
    pressure), every metavariable-touching post-elaboration walk — goal
    rendering and `collectAxioms` — is both unsafe (it can reach the pure,
    process-aborting `MetavarContext.getDecl`) and untrustworthy. In that case
    the goals walk and the axiom walk are skipped and the axiom set is reported
    unavailable, so the verdict can be the honest `budgetExceeded`. -/
private def verificationFacts
    (request : DeclarationVerificationRequest) (snapshot : ModuleSnapshot) (fileLabel : String)
    (target? : Option DeclarationTargetInfo) (candidate? : Option TacticCandidate)
    (candidates : Array DeclarationTargetInfo) (heartbeats : UInt64) :
    IO (DeclarationVerificationFacts × Bool) := do
  let failure ← failureFromMessages snapshot.messages request.budgets.perFieldBytes.toUSize fileLabel
  let bodyText :=
    match target? with
    | some target => (snapshot.document.extractFileSpan? target.bodySpan).getD ""
    | none => ""
  let containsSorry := containsSubstr bodyText "sorry"
  let containsAdmit := containsSubstr bodyText "admit"
  let containsSorryAx := containsSubstr request.source "sorryAx"
  let degraded := candidate?.any tacticCandidateDegraded
  let unresolvedGoals ←
    if degraded then pure #[]
    else match candidate? with
      | none => pure #[]
      | some candidate => do
        let (goals, _, _) ← renderGoals candidate.ctx candidate.mctxAfter candidate.goalsAfter
          request.budgets.perFieldBytes.toUSize 0
        pure (renderedGoalInfos goals request.budgets.perFieldBytes)
  -- Real axiom dependency walk. Only attempted when requested, a single
  -- declaration resolved, and the proof state is not degraded; otherwise the
  -- set is honestly "unavailable".
  let (axioms, axiomsAvailable, axiomsTruncated) ←
    match target?, request.reportAxioms != 0, degraded with
    | some target, true, false => do
      match ← collectAxiomNames snapshot.env target.declarationName.toName heartbeats request.budgets.perFieldBytes with
      | some (names, truncated) => pure (names, true, truncated)
      | none => pure (#[], false, false)
    | _, _, _ => pure (#[], false, false)
  pure ({
    target := target?,
    diagnostics := failure,
    unresolvedGoals,
    containsSorry,
    containsAdmit,
    containsSorryAx,
    axioms,
    axiomsTruncated,
    candidates,
    axiomsAvailable,
    outputTruncated :=
      match failure.truncated with
      | LeanRsFixture.Elaboration.Truncation.truncated => true
      | LeanRsFixture.Elaboration.Truncation.complete => false
  }, degraded)

private def verificationStatus (policy : Nat) (degraded : Bool) (facts : DeclarationVerificationFacts) :
    DeclarationVerificationStatus :=
  if diagnosticMentionsHeartbeat facts.diagnostics then
    .timeout
  -- An interrupted elaboration left dangling metavariables: the worker could
  -- not complete the check, so it reports `budgetExceeded` rather than vouching
  -- for `accepted`/`rejected` on a degraded term. Verification is monotone, so
  -- this never downgrades an honest `accepted`.
  else if degraded then
    .budgetExceeded
  else if diagnosticHasError facts.diagnostics then
    .rejected
  else if !facts.unresolvedGoals.isEmpty then
    .rejected
  else
    if policy == 0 then
      .accepted
    else if facts.containsSorry || facts.containsAdmit || facts.containsSorryAx then
      .rejected
    else
      .accepted

private def unavailableVerificationFacts : DeclarationVerificationFacts :=
  {
    target := none,
    diagnostics := { diagnostics := #[], truncated := LeanRsFixture.Elaboration.Truncation.complete },
    unresolvedGoals := #[],
    containsSorry := false,
    containsAdmit := false,
    containsSorryAx := false,
    axioms := #[],
    axiomsTruncated := false,
    outputTruncated := false,
    candidates := #[],
    axiomsAvailable := false
  }

private def verificationTargetBytes : DeclarationVerificationTarget → Nat
  | .name name => name.utf8ByteSize
  | .span span => spanBytes span

private def verificationFactsBytes (facts : DeclarationVerificationFacts) : Nat :=
  (match facts.target with | some target => declarationTargetInfoBytes target | none => 0) +
    failureBytes facts.diagnostics +
    facts.unresolvedGoals.foldl (init := 0) (fun bytes goal => bytes + renderedBytes goal) +
    facts.axioms.foldl (init := 0) (fun bytes axiomName => bytes + axiomName.utf8ByteSize) +
    facts.candidates.foldl (init := 0) (fun bytes candidate => bytes + declarationTargetInfoBytes candidate)

private def verificationBatchRowBytes (row : DeclarationVerificationBatchRow) : Nat :=
  row.id.utf8ByteSize + verificationTargetBytes row.target + verificationFactsBytes row.facts

private def verifyDeclarationRow
    (request : DeclarationVerificationBatchRequest) (snapshot : ModuleSnapshot) (fileLabel : String)
    (heartbeats : UInt64) (item : DeclarationVerificationBatchItem) :
    IO DeclarationVerificationBatchRow := do
  let singleRequest : DeclarationVerificationRequest :=
    {
      source := request.source,
      target := item.target,
      sorryPolicy := request.sorryPolicy,
      reportAxioms := request.reportAxioms,
      budgets := request.budgets
    }
  let targetResult := resolveVerificationTarget snapshot item.target
  let (status, facts) ←
    match targetResult with
    | .target info => do
      let candidate? ← findTacticCandidate snapshot.document snapshot.trees
        info.bodySpan.startLine info.bodySpan.startColumn
      let (facts, degraded) ← verificationFacts singleRequest snapshot fileLabel (some info) candidate? #[] heartbeats
      pure (verificationStatus request.sorryPolicy degraded facts, facts)
    | .notFound => do
      let (facts, _) ← verificationFacts singleRequest snapshot fileLabel none none #[] heartbeats
      let status :=
        if !snapshot.missing.isEmpty then .needsBuild
        else if diagnosticMentionsHeartbeat facts.diagnostics then .timeout
        else if diagnosticIndicatesResourceLimit facts.diagnostics then .budgetExceeded
        else .notFound
      pure (status, facts)
    | .ambiguous candidates => do
      let (facts, _) ← verificationFacts singleRequest snapshot fileLabel none none candidates heartbeats
      pure (.ambiguous, facts)
  pure { id := item.id, target := item.target, status, facts }

private def verificationBudgetExceededRow (item : DeclarationVerificationBatchItem) :
    DeclarationVerificationBatchRow :=
  {
    id := item.id,
    target := item.target,
    status := .budgetExceeded,
    facts := { unavailableVerificationFacts with outputTruncated := true }
  }

private def verifyDeclarationRows
    (request : DeclarationVerificationBatchRequest) (snapshot : ModuleSnapshot) (fileLabel : String)
    (heartbeats : UInt64) : IO (Array DeclarationVerificationBatchRow) := do
  let mut rows : Array DeclarationVerificationBatchRow := #[]
  let mut spent : Nat := 0
  for item in request.targets do
    if spent ≥ request.budgets.totalBytes then
      rows := rows.push (verificationBudgetExceededRow item)
    else
      let row ← verifyDeclarationRow request snapshot fileLabel heartbeats item
      let bytes := verificationBatchRowBytes row
      if bytes > request.budgets.perFieldBytes || spent + bytes > request.budgets.totalBytes then
        rows := rows.push (verificationBudgetExceededRow item)
      else
        rows := rows.push row
        spent := spent + bytes
  pure rows

private def parseCachePolicy (raw : String) : ModuleQueryCachePolicy :=
  match raw.splitOn "\n" with
  | [fileIdentity, key, maxEntries, ttlMillis, maxBytes] =>
    {
      fileIdentity
      key
      maxEntries := (maxEntries.toNat?.getD 1).toUInt64
      ttlMillis := (ttlMillis.toNat?.getD 0).toUInt64
      maxBytes := (maxBytes.toNat?.getD 1).toUInt64
    }
  | _ =>
    { fileIdentity := "", key := raw, maxEntries := 1, ttlMillis := 0, maxBytes := 1 }

/-- Parse, elaborate, and answer one bounded module query. -/
@[export lean_rs_host_process_module_query]
def processModuleQuery
    (env : Environment) (source : String) (query : ModuleQuery)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize)
    : IO ModuleQueryOutcome := do
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let inputCtx := Parser.mkInputContext source fileLabel
  let (header, parserState, headerMessages) ← Lean.Parser.parseHeader inputCtx
  if headerMessages.hasErrors then
    return .headerParseFailed (← failureFromMessages headerMessages diagBytes fileLabel)

  let userImports := (Lean.Elab.headerToImports header (includeInit := false)).map (·.module.toString)
  let loadedModules := env.header.moduleNames.map (·.toString)
  let missing := userImports.filter (fun nm => ! loadedModules.contains nm)
  let wrapOutcome (result : ModuleQueryResult) : ModuleQueryOutcome :=
    if missing.isEmpty then .ok result userImports
    else .missingImports result userImports missing

  -- Incomplete import closure: skip body elaboration and degrade immediately
  -- (see `buildModuleSnapshot`). The parent renders `missing` as `needs_build`
  -- / `files_skipped` and ignores this empty payload, so a full failing
  -- elaboration would be wasted work on a project-scope scan.
  if !missing.isEmpty then
    return wrapOutcome (emptyResultFor query)

  let (commandEnv, initialMessages) ←
    if Lean.Elab.HeaderSyntax.isModule header && missing.isEmpty then
      try
        unsafe Lean.enableInitializersExecution
        Lean.Elab.processHeader header opts headerMessages inputCtx (mainModule := Name.anonymous)
      catch ex =>
        return wrapOutcome (match query with
          | .diagnostics => .diagnostics (LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel)
          | _ => emptyResultFor query)
    else
      pure (env, headerMessages)

  if initialMessages.hasErrors && missing.isEmpty then
    let result ←
      match query with
      | .diagnostics =>
        let failure ← failureFromMessages initialMessages diagBytes fileLabel
        pure (.diagnostics failure)
      | _ => pure (emptyResultFor query)
    return wrapOutcome result

  let mut commandState : Command.State := Command.mkState commandEnv initialMessages opts
  commandState := { commandState with infoState.enabled := true }
  if !namespaceContext.isEmpty then
    let head := commandState.scopes.headD { header := "", opts }
    commandState := { commandState with scopes := [{ head with currNamespace := namespaceContext.toName }] }
  try
    let headerSource := String.Pos.Raw.extract source 0 parserState.pos
    let bodySource := String.Pos.Raw.extract source parserState.pos source.rawEndPos
    let lineOffset := countNewlines headerSource
    let bodyInputCtx := Parser.mkInputContext bodySource fileLabel
    let st ← Lean.Elab.IO.processCommands bodyInputCtx { : Parser.ModuleParserState } commandState
    let finalCmdState := st.commandState
    let document := SourceDocument.fromSources source bodySource lineOffset
    let result ← resultFor query document finalCmdState.messages finalCmdState.infoState.trees diagBytes fileLabel
    return wrapOutcome result
  catch ex =>
    let failure := LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel
    return wrapOutcome (match query with
      | .diagnostics => .diagnostics failure
      | _ => emptyResultFor query)

/-- Parse, elaborate, and answer several bounded module selectors with one
    module elaboration. -/
@[export lean_rs_host_process_module_query_batch]
def processModuleQueryBatch
    (env : Environment) (source : String) (selectors : Array ModuleQuerySelector)
    (budgets : ModuleQueryOutputBudgets)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (_diagBytes : USize)
    : IO ModuleQueryBatchOutcome := do
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let inputCtx := Parser.mkInputContext source fileLabel
  let (header, parserState, headerMessages) ← Lean.Parser.parseHeader inputCtx
  if headerMessages.hasErrors then
    return .headerParseFailed (← failureFromMessages headerMessages budgets.perFieldBytes.toUSize fileLabel)

  let userImports := (Lean.Elab.headerToImports header (includeInit := false)).map (·.module.toString)
  let loadedModules := env.header.moduleNames.map (·.toString)
  let missing := userImports.filter (fun nm => ! loadedModules.contains nm)
  let wrapOutcome (result : ModuleQueryBatchEnvelope) : ModuleQueryBatchOutcome :=
    if missing.isEmpty then .ok result userImports
    else .missingImports result userImports missing

  -- Incomplete import closure: skip body elaboration and degrade immediately
  -- (see `buildModuleSnapshot`). The parent reads `missing` to degrade and
  -- ignores this empty payload.
  if !missing.isEmpty then
    return wrapOutcome (emptyBatchEnvelope selectors)

  let (commandEnv, initialMessages) ←
    if Lean.Elab.HeaderSyntax.isModule header && missing.isEmpty then
      try
        unsafe Lean.enableInitializersExecution
        Lean.Elab.processHeader header opts headerMessages inputCtx (mainModule := Name.anonymous)
      catch ex =>
        let failure := LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel
        return wrapOutcome (batchEnvelopeFromFailure selectors failure)
    else
      pure (env, headerMessages)

  if initialMessages.hasErrors && missing.isEmpty then
    let failure ← failureFromMessages initialMessages budgets.perFieldBytes.toUSize fileLabel
    return wrapOutcome (batchEnvelopeFromFailure selectors failure)

  let mut commandState : Command.State := Command.mkState commandEnv initialMessages opts
  commandState := { commandState with infoState.enabled := true }
  if !namespaceContext.isEmpty then
    let head := commandState.scopes.headD { header := "", opts }
    commandState := { commandState with scopes := [{ head with currNamespace := namespaceContext.toName }] }
  try
    let headerSource := String.Pos.Raw.extract source 0 parserState.pos
    let bodySource := String.Pos.Raw.extract source parserState.pos source.rawEndPos
    let lineOffset := countNewlines headerSource
    let bodyInputCtx := Parser.mkInputContext bodySource fileLabel
    let st ← Lean.Elab.IO.processCommands bodyInputCtx { : Parser.ModuleParserState } commandState
    let finalCmdState := st.commandState
    let document := SourceDocument.fromSources source bodySource lineOffset
    let result ←
      runBatchSelectors selectors document finalCmdState.messages finalCmdState.infoState.trees budgets fileLabel
        missing
    return wrapOutcome result
  catch ex =>
    let failure := LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel
    return wrapOutcome (batchEnvelopeFromFailure selectors failure)

private def projectCachedSnapshot (snapshot : ModuleSnapshot) (selectors : Array ModuleQuerySelector)
    (budgets : ModuleQueryOutputBudgets) (fileLabel : String) (status : ModuleQueryCacheStatus)
    (baseTimings : ModuleQueryTimings) (cache : ModuleSnapshotCacheState) :
    IO ModuleQueryBatchCachedOutcome := do
  let projectionStart ← IO.monoMsNow
  let envelope ←
    runBatchSelectorsWithCandidates selectors snapshot.document snapshot.messages snapshot.trees snapshot.candidates
      budgets fileLabel snapshot.missing
  let projectionMicros ← elapsedMicrosSince projectionStart
  let timings := { baseTimings with projectionMicros := u64 projectionMicros }
  let facts := cacheFacts status timings (batchEnvelopeBytes envelope) cache
  pure (wrapCachedOutcome snapshot envelope facts)

/-- Parse, elaborate, cache, and answer several bounded module selectors.
    The snapshot cache is private to this worker process and never crosses the
    FFI boundary. -/
@[export lean_rs_host_process_module_query_batch_cached]
def processModuleQueryBatchCached
    (env : Environment) (source : String) (selectors : Array ModuleQuerySelector)
    (budgets : ModuleQueryOutputBudgets)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize)
    (policyRaw : String)
    : IO ModuleQueryBatchCachedOutcome := do
  let policy := parseCachePolicy policyRaw
  let nowMs ← IO.monoMsNow
  let cache0 ← moduleSnapshotCache.get
  let (cache1, evictedBeforeLookup) := pruneCache policy nowMs cache0
  let sameFileBeforeBuild := hasFileIdentity policy.fileIdentity cache1.entries
  match findSnapshot? policy.key cache1.entries with
  | some snapshot =>
    let freshSnapshot := { snapshot with lastUsedMs := nowMs }
    let entries := upsertSnapshot freshSnapshot cache1.entries
    let cache2 := { cache1 with entries }
    moduleSnapshotCache.set cache2
    projectCachedSnapshot freshSnapshot selectors budgets fileLabel .hit
      { headerImportMicros := 0, elaborationMicros := 0, projectionMicros := 0, renderingMicros := 0 }
      cache2
  | none =>
    let status :=
      if sameFileBeforeBuild then .rebuilt
      else if evictedBeforeLookup then .evicted
      else .miss
    match ← buildModuleSnapshot env source namespaceContext fileLabel heartbeats diagBytes policy with
    | .error (failure, headerMicros) =>
      moduleSnapshotCache.set cache1
      pure <| headerFailureCachedOutcome failure status headerMicros cache1
    | .ok (snapshot, timings) =>
      let cacheWithSnapshot :=
        if snapshot.approxBytes > policy.maxBytes.toNat then
          cache1
        else
          let entries := upsertSnapshot snapshot cache1.entries
          let approxBytes := entries.foldl (init := 0) (fun bytes entry => bytes + entry.approxBytes)
          { entries, approxBytes }
      let (cache2, _) := pruneCache policy (← IO.monoMsNow) cacheWithSnapshot
      moduleSnapshotCache.set cache2
      projectCachedSnapshot snapshot selectors budgets fileLabel status timings cache2

@[export lean_rs_host_attempt_proof]
def attemptProof
    (env : Environment) (request : ProofAttemptRequest)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize)
    : IO ProofAttemptOutcome := do
  match ← buildModuleSnapshot env request.source namespaceContext fileLabel heartbeats diagBytes
      (oneShotPolicy request.source fileLabel) with
  | .error (failure, _) => pure <| .headerParseFailed failure
  | .ok (snapshot, _) =>
    let envelope ← attemptEnvelope env request namespaceContext fileLabel heartbeats diagBytes snapshot
    if snapshot.missing.isEmpty then
      pure <| .ok envelope snapshot.imports
    else
      pure <| .missingImports envelope snapshot.imports snapshot.missing

@[export lean_rs_host_verify_declaration]
def verifyDeclaration
    (env : Environment) (request : DeclarationVerificationRequest)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize)
    : IO DeclarationVerificationOutcome := do
  match ← buildModuleSnapshot env request.source namespaceContext fileLabel heartbeats diagBytes
      (oneShotPolicy request.source fileLabel) with
  | .error (failure, _) => pure <| .headerParseFailed failure
  | .ok (snapshot, _) =>
    let targetResult := resolveVerificationTarget snapshot request.target
    let (status, facts) ←
      match targetResult with
      | .target info => do
        let candidate? ← findTacticCandidate snapshot.document snapshot.trees
          info.bodySpan.startLine info.bodySpan.startColumn
        let (facts, degraded) ← verificationFacts request snapshot fileLabel (some info) candidate? #[] heartbeats
        pure (verificationStatus request.sorryPolicy degraded facts, facts)
      -- The name did not resolve. Three honest outcomes: an incomplete
      -- environment (missing imports) may define it elsewhere (`needsBuild`); a
      -- resource-exhausted elaboration could not be trusted to have searched
      -- (`timeout`/`budgetExceeded`); otherwise it is genuinely absent.
      | .notFound => do
        let (facts, _) ← verificationFacts request snapshot fileLabel none none #[] heartbeats
        let status :=
          if !snapshot.missing.isEmpty then .needsBuild
          else if diagnosticMentionsHeartbeat facts.diagnostics then .timeout
          else if diagnosticIndicatesResourceLimit facts.diagnostics then .budgetExceeded
          else .notFound
        pure (status, facts)
      -- Genuinely multiply-defined: attach the competing declarations so the
      -- verdict is actionable.
      | .ambiguous candidates => do
        let (facts, _) ← verificationFacts request snapshot fileLabel none none candidates heartbeats
        pure (.ambiguous, facts)
    if snapshot.missing.isEmpty then
      pure <| .ok status facts snapshot.imports
    else
      pure <| .missingImports status facts snapshot.imports snapshot.missing

@[export lean_rs_host_verify_declaration_batch]
def verifyDeclarationBatch
    (env : Environment) (request : DeclarationVerificationBatchRequest)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize)
    : IO DeclarationVerificationBatchOutcome := do
  match ← buildModuleSnapshot env request.source namespaceContext fileLabel heartbeats diagBytes
      (oneShotPolicy request.source fileLabel) with
  | .error (failure, _) => pure <| .headerParseFailed failure
  | .ok (snapshot, _) =>
    let rows ← verifyDeclarationRows request snapshot fileLabel heartbeats
    if snapshot.missing.isEmpty then
      pure <| .ok rows snapshot.imports
    else
      pure <| .missingImports rows snapshot.imports snapshot.missing

@[export lean_rs_host_clear_module_snapshot_cache]
def clearModuleSnapshotCache (_unit : Unit) : IO ModuleSnapshotCacheClearResult := do
  let cache ← moduleSnapshotCache.get
  moduleSnapshotCache.set {}
  pure { entriesCleared := u64 cache.entries.size, approxBytesCleared := u64 cache.approxBytes }

end LeanRsFixture.InfoTree
