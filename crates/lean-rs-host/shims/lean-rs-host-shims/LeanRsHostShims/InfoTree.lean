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

/-- Query shape for one module-processing request. -/
inductive ModuleQuery where
  | diagnostics
  | typeAt (line : Nat) (column : Nat)
  | goalAt (line : Nat) (column : Nat)
  | references (name : String)
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
  deriving Inhabited

inductive ProofStateResult where
  | state (info : ProofStateInfo)
  | unavailable (message : String)
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
  | replaceSpan (span : ModuleSourceSpan)
  | insertAt (line column : Nat)
  | declarationBody (name : String)
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
  | unsupported
  deriving Inhabited

structure ProofAttemptRow where
  id : String
  status : ProofAttemptStatus
  diagnostics : LeanRsFixture.Elaboration.ElabFailure
  goals : Array RenderedInfo
  safeEdit : Option DeclarationTargetInfo
  outputTruncated : Bool
  deriving Inhabited

structure ProofAttemptEnvelope where
  candidates : Array ProofAttemptRow
  candidateLimit : Nat
  candidatesTruncated : Bool
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

inductive DeclarationVerificationStatus where
  | accepted
  | rejected
  | notFound
  | ambiguous
  | timeout
  | budgetExceeded
  | unsupported
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
  deriving Inhabited

inductive DeclarationVerificationOutcome where
  | ok (status : DeclarationVerificationStatus) (facts : DeclarationVerificationFacts) (imports : Array String)
  | missingImports
      (status : DeclarationVerificationStatus) (facts : DeclarationVerificationFacts)
      (imports missing : Array String)
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
      startLine := s.line, startColumn := s.column
      endLine := e.line, endColumn := e.column
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

private def emptyFailure : LeanRsFixture.Elaboration.ElabFailure :=
  { diagnostics := #[], truncated := .complete }

private def failureFromMessages (messages : MessageLog) (diagBytes : USize) (fileLabel : String) :
    IO LeanRsFixture.Elaboration.ElabFailure := do
  let (diags, trunc) ← LeanRsFixture.Elaboration.serializeMessages messages diagBytes fileLabel
  pure { diagnostics := diags, truncated := trunc }

private def renderGoal (ctx : ContextInfo) (mctx : MetavarContext) (mvarId : MVarId)
    (byteLimit : USize) (currentBytes : USize) : IO (String × USize × Bool) := do
  if currentBytes ≥ byteLimit then
    return ("<truncated>", currentBytes, true)
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

private def offsetSpan (lineOffset : Nat) (span : ModuleSourceSpan) : ModuleSourceSpan :=
  { span with startLine := span.startLine + lineOffset, endLine := span.endLine + lineOffset }

private def bodyLine (line lineOffset : Nat) : Nat :=
  if line > lineOffset then line - lineOffset else 0

private def runTypeAt (fileMap : FileMap) (trees : PersistentArray InfoTree) (line column lineOffset : Nat) :
    IO TypeAtResult := do
  let mut acc : TypeAtAcc := { line := bodyLine line lineOffset, column := column }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectTypeAt fileMap)
  match acc.best? with
  | none => pure .noTerm
  | some candidate =>
    let typeInfo ← candidate.ctx.runMetaM candidate.lctx do
      try
        let ty ← Meta.inferType candidate.expr
        pure (renderExprBounded ty)
      catch _ =>
        pure { value := "", truncated := false }
    pure <| .term (offsetSpan lineOffset candidate.span) (renderExprBounded candidate.expr) typeInfo
      (candidate.expectedType?.map renderExprBounded)

private def runTypeAtWith (fileMap : FileMap) (trees : PersistentArray InfoTree) (line column lineOffset : Nat)
    (limit : Nat) : IO TypeAtResult := do
  let mut acc : TypeAtAcc := { line := bodyLine line lineOffset, column := column }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectTypeAt fileMap)
  match acc.best? with
  | none => pure .noTerm
  | some candidate =>
    let typeInfo ← candidate.ctx.runMetaM candidate.lctx do
      try
        let ty ← Meta.inferType candidate.expr
        pure (renderExprBoundedWith limit ty)
      catch _ =>
        pure { value := "", truncated := false }
    pure <| .term (offsetSpan lineOffset candidate.span) (renderExprBoundedWith limit candidate.expr) typeInfo
      (candidate.expectedType?.map (renderExprBoundedWith limit))

private structure TacticCandidate where
  span : ModuleSourceSpan
  ctx : ContextInfo
  mctxBefore : MetavarContext
  goalsBefore : List MVarId
  mctxAfter : MetavarContext
  goalsAfter : List MVarId

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

private def runGoalAt (fileMap : FileMap) (trees : PersistentArray InfoTree) (line column lineOffset : Nat)
    (diagBytes : USize) : IO GoalAtResult := do
  let mut acc : GoalAtAcc := { line := bodyLine line lineOffset, column := column }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectGoalAt fileMap)
  match acc.best? with
  | none => pure .noTacticContext
  | some candidate =>
    let (before, bytesAfterBefore, truncBefore) ←
      renderGoals candidate.ctx candidate.mctxBefore candidate.goalsBefore diagBytes 0
    let (after, _, truncAfter) ←
      renderGoals candidate.ctx candidate.mctxAfter candidate.goalsAfter diagBytes bytesAfterBefore
    pure <| .goal (offsetSpan lineOffset candidate.span) before after (truncBefore || truncAfter)

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

private def collectReferences (fileMap : FileMap) (lineOffset : Nat) (_ctx : ContextInfo) (info : Info) (acc : ReferencesAcc) :
    IO ReferencesAcc := do
  if acc.truncated then
    pure acc
  else
    match info with
    | .ofTermInfo ti =>
      match rangeOfStx fileMap ti.stx with
      | none => pure acc
      | some span =>
        if ti.expr.isConst && ti.stx.isIdent then
          pure <| pushNameRef (offsetSpan lineOffset span) (toString ti.expr.constName!) ti.isBinder acc
        else if ti.stx.isIdent && ti.isBinder then
          pure <| pushNameRef (offsetSpan lineOffset span) (toString ti.stx.getId) true acc
        else
          pure acc
    | _ => pure acc

private def runReferences (fileMap : FileMap) (trees : PersistentArray InfoTree) (name : String) (lineOffset : Nat) :
    IO ReferencesResult := do
  let mut acc : ReferencesAcc := { target := name }
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectReferences fileMap lineOffset)
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

private def commandDeclaration? (fileMap : FileMap) (lineOffset : Nat) (ctx : ContextInfo) (stx : Syntax) :
    Option DeclarationCandidate := do
  let declStx ← catalogDeclarationSyntax? stx
  let nameStx := declIdNameStx declStx
  guard nameStx.isIdent
  let bodyStx ← declarationBodySyntax? declStx
  let declarationSpanBody ← rangeOfStx fileMap declStx
  let nameSpanBody ← rangeOfStx fileMap nameStx
  let bodySpanBody ← rangeOfStx fileMap bodyStx
  let shortName := nameStx.getId
  let namespaceName := ctx.currNamespace
  let fullName := fullDeclarationName namespaceName shortName
  let info : DeclarationTargetInfo := {
    shortName := shortName.toString
    declarationName := fullName.toString
    namespaceName := namespaceName.toString
    declarationKind := declarationKeyword declStx
    declarationSpan := offsetSpan lineOffset declarationSpanBody
    nameSpan := offsetSpan lineOffset nameSpanBody
    bodySpan := offsetSpan lineOffset bodySpanBody
  }
  some { info, bodySpanBodyCoords := bodySpanBody, declarationSpanBodyCoords := declarationSpanBody }

private def collectDeclarations (fileMap : FileMap) (lineOffset : Nat) (ctx : ContextInfo) (info : Info)
    (acc : Array DeclarationCandidate) : IO (Array DeclarationCandidate) := do
  match info with
  | .ofCommandInfo ci =>
    match commandDeclaration? fileMap lineOffset ctx ci.stx with
    | some candidate => pure (acc.push candidate)
    | none => pure acc
  | _ => pure acc

private def declarationCandidates (fileMap : FileMap) (trees : PersistentArray InfoTree) (lineOffset : Nat) :
    IO (Array DeclarationCandidate) := do
  let mut out : Array DeclarationCandidate := #[]
  for tree in trees do
    out ← tree.foldInfoM (init := out) (collectDeclarations fileMap lineOffset)
  pure out

private def declarationTargetByName (candidates : Array DeclarationCandidate) (name : String) :
    DeclarationTargetResult :=
  let matched := candidates.filter fun candidate =>
    candidate.info.declarationName == name || candidate.info.shortName == name
  match matched with
  | #[] => .notFound
  | #[candidate] => .target candidate.info
  | many => .ambiguous (many.map (·.info))

private def declarationAt? (candidates : Array DeclarationCandidate) (line column lineOffset : Nat) :
    Option DeclarationCandidate :=
  let bodyCursorLine := bodyLine line lineOffset
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

private def declarationTargetByPosition (candidates : Array DeclarationCandidate) (line column lineOffset : Nat) :
    DeclarationTargetResult :=
  match declarationAt? candidates line column lineOffset with
  | some candidate => .target candidate.info
  | none => .notFound

private def selectorDeclarationTarget (candidates : Array DeclarationCandidate) (name? : Option String)
    (line? column? : Option Nat) (lineOffset : Nat) : DeclarationTargetResult :=
  match name?, line?, column? with
  | some name, _, _ => declarationTargetByName candidates name
  | none, some line, some column => declarationTargetByPosition candidates line column lineOffset
  | _, _, _ => .notFound

private def collectLocalContextMeta (limit : Nat) (lctx : LocalContext) : MetaM (Array LocalInfo) :=
  lctx.foldlM (init := #[]) fun acc localDecl => do
    if localDecl.isImplementationDetail then
      pure acc
    else
      let value := localDecl.value?.map (renderExprBoundedWith limit)
      pure <| acc.push {
        name := localDecl.userName.toString
        binderInfo := (repr localDecl.binderInfo).pretty
        typeStr := renderExprBoundedWith limit localDecl.type
        value
      }

private def collectTermLocals (limit : Nat) (ctx : ContextInfo) (lctx : LocalContext) : IO (Array LocalInfo) :=
  ctx.runMetaM lctx (collectLocalContextMeta limit lctx)

private def collectGoalLocals (limit : Nat) (ctx : ContextInfo) (mctx : MetavarContext) (goals : List MVarId) :
    IO (Array LocalInfo) := do
  let ctx := { ctx with mctx := mctx }
  ctx.runMetaM {} do
    match goals with
    | goal :: _ =>
      goal.withContext do
        let decl ← goal.getDecl
        collectLocalContextMeta limit decl.lctx
    | [] => pure #[]

private def runProofState (fileMap : FileMap) (trees : PersistentArray InfoTree)
    (candidates : Array DeclarationCandidate) (line column lineOffset : Nat) (budgets : ModuleQueryOutputBudgets) :
    IO ProofStateResult := do
  let mut goalAcc : GoalAtAcc := { line := bodyLine line lineOffset, column := column }
  for tree in trees do
    goalAcc ← tree.foldInfoM (init := goalAcc) (collectGoalAt fileMap)
  let safeEdit := (declarationAt? candidates line column lineOffset).map (·.info)
  match goalAcc.best? with
  | some candidate =>
    let (before, bytesAfterBefore, truncBefore) ←
      renderGoals candidate.ctx candidate.mctxBefore candidate.goalsBefore budgets.perFieldBytes.toUSize 0
    let (after, _, truncAfter) ←
      renderGoals candidate.ctx candidate.mctxAfter candidate.goalsAfter budgets.perFieldBytes.toUSize bytesAfterBefore
    let locals ← collectGoalLocals budgets.perFieldBytes candidate.ctx candidate.mctxBefore candidate.goalsBefore
    let declName := candidate.ctx.parentDecl?.map (·.toString)
    pure <| .state {
      declarationName := declName
      namespaceName := candidate.ctx.currNamespace.toString
      safeEdit
      span := offsetSpan lineOffset candidate.span
      goalsBefore := before
      goalsAfter := after
      locals
      expectedType := none
      truncated := truncBefore || truncAfter
    }
  | none =>
    let mut termAcc : TypeAtAcc := { line := bodyLine line lineOffset, column := column }
    for tree in trees do
      termAcc ← tree.foldInfoM (init := termAcc) (collectTypeAt fileMap)
    match termAcc.best? with
    | none => pure <| .unavailable "no tactic or term context covers the requested cursor"
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
      let locals ← collectTermLocals budgets.perFieldBytes candidate.ctx candidate.lctx
      let declName := candidate.ctx.parentDecl?.map (·.toString)
      pure <| .state {
        declarationName := declName
        namespaceName := candidate.ctx.currNamespace.toString
        safeEdit
        span := offsetSpan lineOffset candidate.span
        goalsBefore := #[]
        goalsAfter := #[]
        locals
        expectedType
        truncated := expectedType.any (·.truncated)
      }

private def emptyResultFor (query : ModuleQuery) : ModuleQueryResult :=
  match query with
  | .diagnostics => .diagnostics emptyFailure
  | .typeAt .. => .typeAt .noTerm
  | .goalAt .. => .goalAt .noTacticContext
  | .references .. => .references { references := #[], truncated := false }

private def countNewlines (s : String) : Nat :=
  s.foldl (init := 0) fun n c => if c == '\n' then n + 1 else n

private def offsetDiagnosticPos (lineOffset : Nat)
    (pos : LeanRsFixture.Elaboration.DiagnosticPos) :
    LeanRsFixture.Elaboration.DiagnosticPos :=
  { pos with
    line := pos.line + lineOffset
    endLine := pos.endLine.map (· + lineOffset) }

private def offsetDiagnostic (lineOffset : Nat)
    (diagnostic : LeanRsFixture.Elaboration.Diagnostic) :
    LeanRsFixture.Elaboration.Diagnostic :=
  { diagnostic with position := diagnostic.position.map (offsetDiagnosticPos lineOffset) }

private def offsetFailure (lineOffset : Nat)
    (failure : LeanRsFixture.Elaboration.ElabFailure) :
    LeanRsFixture.Elaboration.ElabFailure :=
  if lineOffset == 0 then
    failure
  else
    { failure with diagnostics := failure.diagnostics.map (offsetDiagnostic lineOffset) }

private def resultFor (query : ModuleQuery) (fileMap : FileMap) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (diagBytes : USize) (fileLabel : String) (lineOffset : Nat) :
    IO ModuleQueryResult := do
  match query with
  | .diagnostics =>
    let failure ← failureFromMessages messages diagBytes fileLabel
    pure <| .diagnostics (offsetFailure lineOffset failure)
  | .typeAt line column =>
    pure <| .typeAt (← runTypeAt fileMap trees line column lineOffset)
  | .goalAt line column =>
    pure <| .goalAt (← runGoalAt fileMap trees line column lineOffset diagBytes)
  | .references name =>
    pure <| .references (← runReferences fileMap trees name lineOffset)

private def selectorId : ModuleQuerySelector → String
  | .diagnostics id => id
  | .proofState id .. => id
  | .typeAt id .. => id
  | .references id .. => id
  | .declarationTarget id .. => id
  | .surroundingDeclaration id .. => id

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
  | .unavailable message => message.utf8ByteSize
  | .state info =>
    (match info.declarationName with | some name => name.utf8ByteSize | none => 0) +
      info.namespaceName.utf8ByteSize +
      (match info.safeEdit with | some target => declarationTargetInfoBytes target | none => 0) +
      spanBytes info.span +
      info.goalsBefore.foldl (init := 0) (· + ·.utf8ByteSize) +
      info.goalsAfter.foldl (init := 0) (· + ·.utf8ByteSize) +
      info.locals.foldl (init := 0) (fun bytes localInfo => bytes + localInfoBytes localInfo) +
      (match info.expectedType with | some expected => renderedBytes expected | none => 0)

private def declarationTargetBytes : DeclarationTargetResult → Nat
  | .target info => declarationTargetInfoBytes info
  | .notFound => 0
  | .ambiguous candidates =>
    candidates.foldl (init := 0) fun bytes candidate => bytes + declarationTargetInfoBytes candidate

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

private def batchResultFor (selector : ModuleQuerySelector) (fileMap : FileMap) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (candidates : Array DeclarationCandidate)
    (budgets : ModuleQueryOutputBudgets) (fileLabel : String) (lineOffset : Nat) :
    IO ModuleQueryBatchResult := do
  match selector with
  | .diagnostics _ =>
    let failure ← failureFromMessages messages budgets.perFieldBytes.toUSize fileLabel
    pure <| .diagnostics (offsetFailure lineOffset failure)
  | .proofState _ line column =>
    pure <| .proofState (← runProofState fileMap trees candidates line column lineOffset budgets)
  | .typeAt _ line column =>
    pure <| .typeAt (← runTypeAtWith fileMap trees line column lineOffset budgets.perFieldBytes)
  | .references _ name =>
    pure <| .references (← runReferences fileMap trees name lineOffset)
  | .declarationTarget _ name? line? column? =>
    pure <| .declarationTarget (selectorDeclarationTarget candidates name? line? column? lineOffset)
  | .surroundingDeclaration _ line column =>
    let result :=
      match declarationAt? candidates line column lineOffset with
      | some candidate => .declaration candidate.info
      | none => .none
    pure <| .surroundingDeclaration result

private def emptyBatchResultFor (selector : ModuleQuerySelector) : ModuleQueryBatchResult :=
  match selector with
  | .diagnostics _ => .diagnostics emptyFailure
  | .proofState _ .. => .proofState (.unavailable "module processing did not reach the requested context")
  | .typeAt _ .. => .typeAt .noTerm
  | .references _ .. => .references { references := #[], truncated := false }
  | .declarationTarget _ .. => .declarationTarget .notFound
  | .surroundingDeclaration _ .. => .surroundingDeclaration .none

private def batchEnvelopeFromFailure (selectors : Array ModuleQuerySelector)
    (failure : LeanRsFixture.Elaboration.ElabFailure) : ModuleQueryBatchEnvelope :=
  let items := selectors.map fun selector =>
    match selector with
    | .diagnostics id => .ok id (.diagnostics failure)
    | _ => .ok (selectorId selector) (emptyBatchResultFor selector)
  { items, totalTruncated := false }

private def runBatchSelectorsWithCandidates (selectors : Array ModuleQuerySelector) (fileMap : FileMap) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (candidates : Array DeclarationCandidate)
    (budgets : ModuleQueryOutputBudgets) (fileLabel : String) (lineOffset : Nat) :
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
        let result ← batchResultFor selector fileMap messages trees candidates budgets fileLabel lineOffset
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

private def runBatchSelectors (selectors : Array ModuleQuerySelector) (fileMap : FileMap) (messages : MessageLog)
    (trees : PersistentArray InfoTree) (budgets : ModuleQueryOutputBudgets) (fileLabel : String) (lineOffset : Nat) :
    IO ModuleQueryBatchEnvelope := do
  let candidates ← declarationCandidates fileMap trees lineOffset
  runBatchSelectorsWithCandidates selectors fileMap messages trees candidates budgets fileLabel lineOffset

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
  fileMap : FileMap
  messages : MessageLog
  trees : PersistentArray InfoTree
  candidates : Array DeclarationCandidate
  imports : Array String
  missing : Array String
  lineOffset : Nat
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
  try
    let headerSource := String.Pos.Raw.extract source 0 parserState.pos
    let bodySource := String.Pos.Raw.extract source parserState.pos source.rawEndPos
    let lineOffset := countNewlines headerSource
    let bodyInputCtx := Parser.mkInputContext bodySource fileLabel
    let st ← Lean.Elab.IO.processCommands bodyInputCtx { : Parser.ModuleParserState } commandState
    let elabMicros ← elapsedMicrosSince elabStart
    let finalCmdState := st.commandState
    let candidates ← declarationCandidates bodyInputCtx.fileMap finalCmdState.infoState.trees lineOffset
    let approxBytes := approxSnapshotBytes source finalCmdState.messages finalCmdState.infoState.trees candidates
    let nowMs ← IO.monoMsNow
    let snapshot : ModuleSnapshot := {
      fileIdentity := policy.fileIdentity
      key := policy.key
      fileMap := bodyInputCtx.fileMap
      messages := finalCmdState.messages
      trees := finalCmdState.infoState.trees
      candidates
      imports := userImports
      missing
      lineOffset
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

private def proofCandidateLimit : Nat := 8

private def oneShotPolicy (source fileLabel : String) : ModuleQueryCachePolicy :=
  {
    fileIdentity := fileLabel
    key := source
    maxEntries := 1
    ttlMillis := 0
    maxBytes := 1
  }

private def sourceRawOffset? (source : String) (line column : Nat) : Option String.Pos.Raw :=
  if line == 0 then
    none
  else
    let lines := source.splitOn "\n"
    if line > lines.length then
      none
    else
      let before := lines.take (line - 1)
      let base := before.foldl (init := 0) fun bytes text => bytes + text.utf8ByteSize + 1
      some ⟨base + column⟩

private def replaceRawSpan (source replacement : String) (start stop : String.Pos.Raw) : String :=
  let pfx := String.Pos.Raw.extract source 0 start
  let suffix := String.Pos.Raw.extract source stop source.rawEndPos
  pfx ++ replacement ++ suffix

private def replaceSourceSpan? (source replacement : String) (span : ModuleSourceSpan) : Option String :=
  match sourceRawOffset? source span.startLine span.startColumn,
      sourceRawOffset? source span.endLine span.endColumn with
  | some start, some stop => some (replaceRawSpan source replacement start stop)
  | _, _ => none

private def insertAt? (source insertion : String) (line column : Nat) : Option String :=
  match sourceRawOffset? source line column with
  | some pos => some (replaceRawSpan source insertion pos pos)
  | none => none

private def extractSourceSpan? (source : String) (span : ModuleSourceSpan) : Option String :=
  match sourceRawOffset? source span.startLine span.startColumn,
      sourceRawOffset? source span.endLine span.endColumn with
  | some start, some stop => some (String.Pos.Raw.extract source start stop)
  | _, _ => none

private def resolveEditTarget (snapshot : ModuleSnapshot) (edit : ProofEditTarget) :
    Except String (ModuleSourceSpan × Option DeclarationTargetInfo) :=
  match edit with
  | .replaceSpan span => .ok (span, none)
  | .insertAt line column =>
    .ok ({ startLine := line, startColumn := column, endLine := line, endColumn := column }, none)
  | .declarationBody name =>
    match declarationTargetByName snapshot.candidates name with
    | .target info => .ok (info.bodySpan, some info)
    | .notFound => .error s!"declaration `{name}` was not found"
    | .ambiguous _ => .error s!"declaration `{name}` is ambiguous"

private def overlaySource? (source : String) (edit : ProofEditTarget) (span : ModuleSourceSpan) (text : String) :
    Option String :=
  match edit with
  | .insertAt .. => insertAt? source text span.startLine span.startColumn
  | _ => replaceSourceSpan? source text span

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
  | .unavailable _ => (#[], false)

private def attemptRowBytes (row : ProofAttemptRow) : Nat :=
  row.id.utf8ByteSize + failureBytes row.diagnostics +
    row.goals.foldl (init := 0) (fun bytes goal => bytes + renderedBytes goal) +
    match row.safeEdit with
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
    (heartbeats : UInt64) (diagBytes : USize) (span : ModuleSourceSpan) (safeEdit : Option DeclarationTargetInfo)
    (candidate : ProofCandidate) : IO ProofAttemptRow := do
  let some overlay := overlaySource? request.source request.edit span candidate.text
    | return {
        id := candidate.id, status := .failed,
        diagnostics := LeanRsFixture.Elaboration.singleErrorFailure "proof edit target is outside the source text" fileLabel,
        goals := #[], safeEdit, outputTruncated := false
      }
  match ← buildModuleSnapshot env overlay namespaceContext fileLabel heartbeats diagBytes (oneShotPolicy overlay fileLabel) with
  | .error (failure, _) =>
    return {
      id := candidate.id,
      status := if diagnosticMentionsHeartbeat failure then .timeout else .failed,
      diagnostics := failure,
      goals := #[],
      safeEdit,
      outputTruncated := false
    }
  | .ok (snapshot, _) =>
    let failure ← failureFromMessages snapshot.messages request.budgets.perFieldBytes.toUSize fileLabel
    let cursorLine := span.startLine
    let cursorColumn := span.startColumn
    let proofState ←
      runProofState snapshot.fileMap snapshot.trees snapshot.candidates cursorLine cursorColumn
        snapshot.lineOffset request.budgets
    let (goals, proofStateTruncated) := proofStateGoalsAfter proofState request.budgets.perFieldBytes
    let status := classifyAttempt failure goals
    return {
      id := candidate.id,
      status,
      diagnostics := failure,
      goals,
      safeEdit,
      outputTruncated := proofStateTruncated ||
        match failure.truncated with
        | LeanRsFixture.Elaboration.Truncation.truncated => true
        | LeanRsFixture.Elaboration.Truncation.complete => false
    }

private def candidateCapRows (candidates : Array ProofCandidate) : Array ProofCandidate × Bool :=
  (candidates.extract 0 (min candidates.size proofCandidateLimit), candidates.size > proofCandidateLimit)

private def attemptEnvelope
    (env : Environment) (request : ProofAttemptRequest) (namespaceContext fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize) (base : ModuleSnapshot) :
    IO ProofAttemptEnvelope := do
  let (candidates, candidatesTruncated) := candidateCapRows request.candidates
  match resolveEditTarget base request.edit with
  | .error message =>
    let rows := candidates.map fun candidate => {
      id := candidate.id,
      status := ProofAttemptStatus.failed,
      diagnostics := LeanRsFixture.Elaboration.singleErrorFailure message fileLabel,
      goals := #[],
      safeEdit := none,
      outputTruncated := false
    }
    pure { candidates := rows, candidateLimit := proofCandidateLimit, candidatesTruncated }
  | .ok (span, safeEdit) =>
    let mut rows : Array ProofAttemptRow := #[]
    let mut spent : Nat := 0
    for candidate in candidates do
      if spent ≥ request.budgets.totalBytes then
        rows := rows.push {
          id := candidate.id,
          status := .budgetExceeded,
          diagnostics := emptyFailure,
          goals := #[],
          safeEdit,
          outputTruncated := true
        }
      else
        let row ← attemptCandidate env request namespaceContext fileLabel heartbeats diagBytes span safeEdit candidate
        let bytes := attemptRowBytes row
        if spent + bytes > request.budgets.totalBytes then
          rows := rows.push { row with status := .budgetExceeded, outputTruncated := true }
        else
          rows := rows.push row
          spent := spent + bytes
    pure { candidates := rows, candidateLimit := proofCandidateLimit, candidatesTruncated }

private def containsSubstr (text needle : String) : Bool :=
  (text.splitOn needle).length > 1

private def resolveVerificationTarget (snapshot : ModuleSnapshot) (target : DeclarationVerificationTarget) :
    DeclarationTargetResult :=
  match target with
  | .name name => declarationTargetByName snapshot.candidates name
  | .span span => declarationTargetByPosition snapshot.candidates span.startLine span.startColumn snapshot.lineOffset

private def verificationFacts
    (request : DeclarationVerificationRequest) (snapshot : ModuleSnapshot) (fileLabel : String)
    (target? : Option DeclarationTargetInfo) : IO DeclarationVerificationFacts := do
  let failure ← failureFromMessages snapshot.messages request.budgets.perFieldBytes.toUSize fileLabel
  let bodyText :=
    match target? with
    | some target => (extractSourceSpan? request.source target.bodySpan).getD ""
    | none => ""
  let containsSorry := containsSubstr bodyText "sorry"
  let containsAdmit := containsSubstr bodyText "admit"
  let containsSorryAx := containsSubstr request.source "sorryAx"
  let unresolvedGoals ←
    match target? with
    | none => pure #[]
    | some target => do
      let proofState ← runProofState snapshot.fileMap snapshot.trees snapshot.candidates
        target.bodySpan.startLine target.bodySpan.startColumn snapshot.lineOffset request.budgets
      let (goals, _) := proofStateGoalsAfter proofState request.budgets.perFieldBytes
      pure goals
  let axioms :=
    if request.reportAxioms != 0 && (containsSorry || containsAdmit || containsSorryAx) then
      #["sorryAx"]
    else
      #[]
  pure {
    target := target?,
    diagnostics := failure,
    unresolvedGoals,
    containsSorry,
    containsAdmit,
    containsSorryAx,
    axioms,
    axiomsTruncated := false,
    outputTruncated :=
      match failure.truncated with
      | LeanRsFixture.Elaboration.Truncation.truncated => true
      | LeanRsFixture.Elaboration.Truncation.complete => false
  }

private def verificationStatus (policy : Nat) (facts : DeclarationVerificationFacts) :
    DeclarationVerificationStatus :=
  if diagnosticMentionsHeartbeat facts.diagnostics then
    .timeout
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
    let result ← resultFor query bodyInputCtx.fileMap finalCmdState.messages finalCmdState.infoState.trees diagBytes fileLabel lineOffset
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
    let result ←
      runBatchSelectors selectors bodyInputCtx.fileMap finalCmdState.messages finalCmdState.infoState.trees
        budgets fileLabel lineOffset
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
    runBatchSelectorsWithCandidates selectors snapshot.fileMap snapshot.messages snapshot.trees snapshot.candidates
      budgets fileLabel snapshot.lineOffset
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
        let facts ← verificationFacts request snapshot fileLabel (some info)
        pure (verificationStatus request.sorryPolicy facts, facts)
      | .notFound => do
        let facts ← verificationFacts request snapshot fileLabel none
        pure (.notFound, facts)
      | .ambiguous _ => do
        let facts ← verificationFacts request snapshot fileLabel none
        pure (.ambiguous, facts)
    if snapshot.missing.isEmpty then
      pure <| .ok status facts snapshot.imports
    else
      pure <| .missingImports status facts snapshot.imports snapshot.missing

@[export lean_rs_host_clear_module_snapshot_cache]
def clearModuleSnapshotCache (_unit : Unit) : IO ModuleSnapshotCacheClearResult := do
  let cache ← moduleSnapshotCache.get
  moduleSnapshotCache.set {}
  pure { entriesCleared := u64 cache.entries.size, approxBytesCleared := u64 cache.approxBytes }

end LeanRsFixture.InfoTree
