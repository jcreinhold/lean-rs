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
  out : String := ""
  bytes : Nat := 0
  truncated : Bool := false

private def appendBounded (s : String) (st : RenderState) : RenderState :=
  if st.truncated then
    st
  else
    let b := s.utf8ByteSize
    if st.bytes + b > renderByteLimit then
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

end LeanRsFixture.InfoTree
