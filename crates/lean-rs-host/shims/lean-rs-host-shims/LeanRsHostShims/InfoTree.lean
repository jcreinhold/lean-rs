import Lean
import LeanRsHostShims.Elaboration

/-! Capability category: project a single processed Lean source string
    into an FFI-safe info-tree projection. One optional `@[export]` shim
    that drives the standard `Lean.Elab.IO.processCommands` pipeline
    with info collection enabled, then walks every captured `InfoTree`
    and records four kinds of node:

    * `SerializableCommandInfo` — one per top-level command;
    * `SerializableTermInfo`    — one per `Elab.TermInfo`;
    * `SerializableTacticInfo`  — one per `Elab.TacticInfo`, with each
                                  goal pre-rendered through
                                  `Lean.Meta.ppGoal` under the
                                  diagnostic byte budget;
    * `SerializableNameRef`     — every binder / reference identifier
                                  observed in the term-info stream.

    Structure layout follows the `Elaboration.lean` discipline: every
    field is a `Nat`, `String`, `Option`, `Array`, nested structure, or
    nullary inductive, so the Rust side decodes through the standard
    `take_ctor_objects` / `lean_ctor_get_uint8` primitives. The single
    `Bool isBinder` field on `SerializableNameRef` decodes through the
    object-slot `bool::try_from_lean` codec used by `DeclarationFilter`. -/

namespace LeanRsFixture.InfoTree

open Lean Elab Meta

/-- Pre-rendered term node. `exprStr` and `typeStr` use `Expr.toString`
    (the cheap raw projection); callers wanting notation-aware text use
    the optional `meta_pp_expr` service against the same Expr. -/
structure SerializableTermInfo where
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  exprStr : String
  typeStr : String
  expectedTypeStr : Option String
  deriving Inhabited

/-- Pre-rendered tactic node. `goalsBefore` / `goalsAfter` are already
    pretty-printed inside the elaboration's MetaM context so callers
    never need to revive metavariables across the FFI boundary. -/
structure SerializableTacticInfo where
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  goalsBefore : Array String
  goalsAfter : Array String
  deriving Inhabited

/-- Identifier reference. `isBinder` distinguishes a binding-site
    occurrence (`true`) from a use-site reference (`false`). -/
structure SerializableNameRef where
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  name : String
  isBinder : Bool
  deriving Inhabited

/-- Command node. `declName` is set only when the elaborator looks like
    a declaration command (`elabDeclaration`, `elabTheorem`, …); other
    commands such as `#check` carry `none`. -/
structure SerializableCommandInfo where
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  declName : Option String
  deriving Inhabited

/-- The full projection returned by `processWithInfoTree`. Diagnostics
    reuse the host stack's `ElabFailure` shape so callers branch through
    the same `diagnostics` / `truncated` accessors as `kernel_check`. -/
structure ProcessedFile where
  commands : Array SerializableCommandInfo
  terms    : Array SerializableTermInfo
  tactics  : Array SerializableTacticInfo
  names    : Array SerializableNameRef
  diagnostics : LeanRsFixture.Elaboration.ElabFailure
  deriving Inhabited

private partial def firstIdent? : Syntax → Option Name
  | .ident _ _ n _ => some n
  | .node _ _ args =>
    args.foldl (init := none) fun acc s => acc <|> firstIdent? s
  | _ => none

private def extractDeclName? (ci : CommandInfo) : Option String :=
  let elabStr := toString ci.elaborator
  let isDecl :=
    elabStr.endsWith "elabDeclaration" ||
    elabStr.endsWith "elabTheorem" ||
    elabStr.endsWith "elabDef" ||
    elabStr.endsWith "elabInstance" ||
    elabStr.endsWith "elabExample" ||
    elabStr.endsWith "elabAxiom" ||
    elabStr.endsWith "elabOpaque"
  if isDecl then (firstIdent? ci.stx).map toString else none

private def rangeOfStx (fileMap : FileMap) (stx : Syntax) : Option (Nat × Nat × Nat × Nat) :=
  match stx.getRange? with
  | none => none
  | some ⟨sp, ep⟩ =>
    let s := fileMap.toPosition sp
    let e := fileMap.toPosition ep
    some (s.line, s.column, e.line, e.column)

private structure WalkAcc where
  commands : Array SerializableCommandInfo := #[]
  terms : Array SerializableTermInfo := #[]
  tactics : Array SerializableTacticInfo := #[]
  names : Array SerializableNameRef := #[]
  goalBytes : USize := 0

private def renderGoal (ctx : ContextInfo) (mctx : MetavarContext) (mvarId : MVarId)
    (byteLimit : USize) (currentBytes : USize) : IO (String × USize) := do
  if currentBytes ≥ byteLimit then
    return ("<truncated>", currentBytes)
  let s ← ctx.runMetaM .empty do
    setMCtx mctx
    try
      let fmt ← Meta.ppGoal mvarId
      pure fmt.pretty
    catch _ =>
      pure "<error rendering goal>"
  let newBytes := currentBytes + s.utf8ByteSize.toUSize
  return (s, newBytes)

private def renderGoals (ctx : ContextInfo) (mctx : MetavarContext) (mvars : List MVarId)
    (byteLimit : USize) (start : USize) : IO (Array String × USize) := do
  let mut out : Array String := #[]
  let mut bytes := start
  for mvarId in mvars do
    let (s, newBytes) ← renderGoal ctx mctx mvarId byteLimit bytes
    out := out.push s
    bytes := newBytes
  return (out, bytes)

private def collectFromInfo (fileMap : FileMap) (byteLimit : USize)
    (ctx : ContextInfo) (info : Info) (acc : WalkAcc) : IO WalkAcc := do
  match info with
  | .ofCommandInfo ci =>
    match rangeOfStx fileMap ci.stx with
    | none => pure acc
    | some (sl, sc, el, ec) =>
      let node : SerializableCommandInfo :=
        { startLine := sl, startColumn := sc, endLine := el, endColumn := ec
          declName := extractDeclName? ci }
      pure { acc with commands := acc.commands.push node }
  | .ofTermInfo ti =>
    match rangeOfStx fileMap ti.stx with
    | none => pure acc
    | some (sl, sc, el, ec) =>
      let exprStr := toString ti.expr
      let typeStr ← ctx.runMetaM ti.lctx do
        try
          let ty ← Meta.inferType ti.expr
          pure (toString ty)
        catch _ => pure ""
      let termNode : SerializableTermInfo :=
        { startLine := sl, startColumn := sc, endLine := el, endColumn := ec
          exprStr := exprStr, typeStr := typeStr
          expectedTypeStr := ti.expectedType?.map toString }
      let mut acc := { acc with terms := acc.terms.push termNode }
      if ti.expr.isConst && ti.stx.isIdent then
        let nameNode : SerializableNameRef :=
          { startLine := sl, startColumn := sc, endLine := el, endColumn := ec
            name := toString ti.expr.constName!
            isBinder := ti.isBinder }
        acc := { acc with names := acc.names.push nameNode }
      else if ti.stx.isIdent && ti.isBinder then
        let nameNode : SerializableNameRef :=
          { startLine := sl, startColumn := sc, endLine := el, endColumn := ec
            name := toString ti.stx.getId
            isBinder := true }
        acc := { acc with names := acc.names.push nameNode }
      pure acc
  | .ofTacticInfo ti =>
    match rangeOfStx fileMap ti.stx with
    | none => pure acc
    | some (sl, sc, el, ec) =>
      let (gb, bytesAfterB) ← renderGoals ctx ti.mctxBefore ti.goalsBefore byteLimit acc.goalBytes
      let (ga, bytesAfterA) ← renderGoals ctx ti.mctxAfter ti.goalsAfter byteLimit bytesAfterB
      let node : SerializableTacticInfo :=
        { startLine := sl, startColumn := sc, endLine := el, endColumn := ec
          goalsBefore := gb, goalsAfter := ga }
      pure { acc with tactics := acc.tactics.push node, goalBytes := bytesAfterA }
  | _ => pure acc

private def walkTrees (fileMap : FileMap) (byteLimit : USize)
    (trees : PersistentArray Lean.Elab.InfoTree) : IO WalkAcc := do
  let mut acc : WalkAcc := {}
  for tree in trees do
    acc ← tree.foldInfoM (init := acc) (collectFromInfo fileMap byteLimit)
  pure acc

/-- Parse, elaborate, and project a Lean source string into its
    `InfoTree`. Heartbeat-bounded via `Lean.maxHeartbeats`. The
    diagnostic byte budget bounds both the message log AND the
    cumulative size of pre-rendered goal text. -/
@[export lean_rs_host_process_with_info_tree]
def processWithInfoTree
    (env : Environment) (source : String)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize)
    : IO ProcessedFile := do
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let inputCtx := Parser.mkInputContext source fileLabel
  let mut commandState : Command.State := Command.mkState env {} opts
  commandState := { commandState with infoState.enabled := true }
  if !namespaceContext.isEmpty then
    let head := commandState.scopes.headD { header := "", opts }
    commandState := { commandState with scopes := [{ head with currNamespace := namespaceContext.toName }] }
  try
    let st ← Lean.Elab.IO.processCommands inputCtx { : Parser.ModuleParserState } commandState
    let finalCmdState := st.commandState
    let (diags, trunc) ← LeanRsFixture.Elaboration.serializeMessages finalCmdState.messages diagBytes fileLabel
    let failure : LeanRsFixture.Elaboration.ElabFailure :=
      { diagnostics := diags, truncated := trunc }
    let acc ← walkTrees inputCtx.fileMap diagBytes finalCmdState.infoState.trees
    return {
      commands := acc.commands
      terms := acc.terms
      tactics := acc.tactics
      names := acc.names
      diagnostics := failure
    }
  catch ex =>
    let failure := LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel
    return {
      commands := #[], terms := #[], tactics := #[], names := #[]
      diagnostics := failure
    }

/-- Outcome of `processModuleWithInfoTree`. Distinguishes a clean
    header parse + processed body, a clean header whose imports are
    not all present in the open env (soft failure — the body still
    elaborates against whatever the env carries), and a header that
    did not parse at all (in which case the body is never processed). -/
inductive ProcessModuleOutcome where
  | ok
      (file : ProcessedFile)
      (imports : Array String)
  | missingImports
      (file : ProcessedFile)
      (imports : Array String)
      (missing : Array String)
  | headerParseFailed
      (diagnostics : LeanRsFixture.Elaboration.ElabFailure)
  deriving Inhabited

/-- Parse the header of a Lean source via `Lean.Parser.parseHeader`,
    then resume `IO.processCommands` from the parser state the parser
    produced. The single shared `InputContext.fileMap` covers the whole
    source, so every position the elaborator records in info-tree nodes
    is already in the original file's line/column coordinates — no Rust
    offset arithmetic. Imports parsed from the header are returned as
    strings; those that do not appear in the session's open env are
    reported under `.missingImports` as a soft failure. A header parse
    error (e.g., `import 123`) short-circuits before any body
    elaboration runs. -/
@[export lean_rs_host_process_module_with_info_tree]
def processModuleWithInfoTree
    (env : Environment) (source : String)
    (namespaceContext : String) (fileLabel : String)
    (heartbeats : UInt64) (diagBytes : USize)
    : IO ProcessModuleOutcome := do
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let inputCtx := Parser.mkInputContext source fileLabel
  let (header, parserState, headerMessages) ← Lean.Parser.parseHeader inputCtx
  if headerMessages.hasErrors then
    let (diags, trunc) ←
      LeanRsFixture.Elaboration.serializeMessages headerMessages diagBytes fileLabel
    return .headerParseFailed { diagnostics := diags, truncated := trunc }
  -- Project the user-written imports only (omit the implicit `Init` the
  -- elaborator inserts; downstream consumers care about what the user
  -- typed, not what the elaborator adds for free).
  -- Check user-written imports against the env's full transitive
  -- module closure (`moduleNames`), not just its direct imports —
  -- otherwise `import Lean` would be flagged as missing whenever the
  -- session only directly imports a module that transitively pulls in
  -- `Lean`.
  let userImports := (Lean.Elab.headerToImports header (includeInit := false)).map (·.module.toString)
  let loadedModules := env.header.moduleNames.map (·.toString)
  let missing := userImports.filter (fun nm => ! loadedModules.contains nm)
  let buildFile (acc : WalkAcc) (failure : LeanRsFixture.Elaboration.ElabFailure) : ProcessedFile :=
    { commands := acc.commands, terms := acc.terms, tactics := acc.tactics
      names := acc.names, diagnostics := failure }
  let wrapOutcome (file : ProcessedFile) : ProcessModuleOutcome :=
    if missing.isEmpty then .ok file userImports
    else .missingImports file userImports missing
  let (commandEnv, initialMessages) ←
    if Lean.Elab.HeaderSyntax.isModule header && missing.isEmpty then
      -- Module-system files need the same header-elaborated environment
      -- Lean's frontend uses: `import all` visibility and public/private
      -- current-file checks depend on `env.header.isModule`.
      unsafe Lean.enableInitializersExecution
      Lean.Elab.processHeader header opts headerMessages inputCtx (mainModule := Name.anonymous)
    else
      pure (env, headerMessages)
  if initialMessages.hasErrors && missing.isEmpty then
    let (diags, trunc) ←
      LeanRsFixture.Elaboration.serializeMessages initialMessages diagBytes fileLabel
    return wrapOutcome (buildFile {} { diagnostics := diags, truncated := trunc })
  let mut commandState : Command.State := Command.mkState commandEnv initialMessages opts
  commandState := { commandState with infoState.enabled := true }
  if !namespaceContext.isEmpty then
    let head := commandState.scopes.headD { header := "", opts }
    commandState := { commandState with scopes := [{ head with currNamespace := namespaceContext.toName }] }
  try
    let st ← Lean.Elab.IO.processCommands inputCtx parserState commandState
    let finalCmdState := st.commandState
    let (diags, trunc) ←
      LeanRsFixture.Elaboration.serializeMessages finalCmdState.messages diagBytes fileLabel
    let failure : LeanRsFixture.Elaboration.ElabFailure :=
      { diagnostics := diags, truncated := trunc }
    let acc ← walkTrees inputCtx.fileMap diagBytes finalCmdState.infoState.trees
    return wrapOutcome (buildFile acc failure)
  catch ex =>
    let failure := LeanRsFixture.Elaboration.singleErrorFailure (toString ex) fileLabel
    let empty : WalkAcc := {}
    return wrapOutcome (buildFile empty failure)

end LeanRsFixture.InfoTree
