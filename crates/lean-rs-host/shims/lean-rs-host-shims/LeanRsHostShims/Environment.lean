import Lean
import Lean.Meta.Tactic.Simp.Attr
import LeanRsInterop.Callback

/-! Capability category: session-scoped environment queries. The Rust
    `LeanSession` looks up these environment `@[export]` symbols by name at
    `LeanCapabilities::load_capabilities` time and dispatches every query
    through the cached addresses. Path-layout knowledge (where the
    `.olean` files live for `Lean.importModules`) stays on the Rust side
    — the import shim only receives a ready-made search-path string. -/

namespace LeanRsFixture.Environment

open Lean

private def reportProgress? (handle trampoline : USize) (current total : Nat) : IO (Option UInt8) := do
  let status ← LeanRsInterop.Callback.Tick.call handle trampoline (UInt64.ofNat current) (UInt64.ofNat total)
  if status == 0 then
    pure none
  else
    pure (some status)

structure SourceRange where
  file : String
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  deriving Inhabited

structure DeclarationFilter where
  includePrivate : Nat
  includeGenerated : Nat
  includeInternal : Nat
  deriving Inhabited

inductive DeclarationNameMatch where
  | contains
  | suffix
  deriving Inhabited

inductive DeclarationSearchScope where
  | namespace
  | module
  deriving Inhabited

structure DeclarationSearchBias where
  scope : Nat
  scopePrefix : String
  isStrict : Nat
  weight : String
  deriving Inhabited

structure DeclarationSearchRequest where
  nameFragment : Option String
  nameMatch : Nat
  kind : Option String
  requiredConstants : Array String
  conclusionHead : Option String
  scopeBiases : Array DeclarationSearchBias
  limit : Nat
  filter : DeclarationFilter
  includeSource : Nat
  deriving Inhabited

structure DeclarationFlags where
  isPrivate : Nat
  isGenerated : Nat
  isInternal : Nat
  deriving Inhabited

structure DeclarationSearchRow where
  name : String
  kind : String
  module : Option String
  source : Option SourceRange
  matchReason : String
  score : String
  rank : Nat
  flags : DeclarationFlags
  deriving Inhabited

structure DeclarationSearchPruning where
  stage : String
  reason : String
  count : Nat
  deriving Inhabited

structure DeclarationSearchTimings where
  scanMicros : Nat
  rankMicros : Nat
  sourceMicros : Nat
  deriving Inhabited

structure DeclarationSearchFacts where
  declarationsScanned : Nat
  afterNameFilter : Nat
  afterKindFilter : Nat
  afterRequiredConstantsFilter : Nat
  afterConclusionFilter : Nat
  afterScopeFilter : Nat
  sourceLookups : Nat
  broadPruning : Array DeclarationSearchPruning
  truncated : Nat
  timings : DeclarationSearchTimings
  deriving Inhabited

structure DeclarationSearchResult where
  declarations : Array DeclarationSearchRow
  truncated : Nat
  facts : DeclarationSearchFacts
  deriving Inhabited

structure DeclarationInspectionFields where
  source : Nat
  statement : Nat
  docstring : Nat
  attributes : Nat
  flags : Nat
  /-- Requested statement rendering: `1` = notation-aware pretty (delaborated,
      `pp.universes false`), `0` = raw `Expr.toString`. Pretty falls back to raw
      when the pretty-printer cannot render the term. -/
  rendering : Nat
  deriving Inhabited

structure DeclarationInspectionBudgets where
  perFieldBytes : Nat
  totalBytes : Nat
  deriving Inhabited

structure DeclarationInspectionRequest where
  name : String
  fields : DeclarationInspectionFields
  budgets : DeclarationInspectionBudgets
  deriving Inhabited

structure DeclarationRenderedInfo where
  value : String
  truncated : Nat
  deriving Inhabited

structure DeclarationProofSearchFacts where
  isSimp : Nat
  isRwCandidate : Nat
  isInstance : Nat
  isClass : Nat
  className : Option String
  deriving Inhabited

structure DeclarationInspection where
  name : String
  kind : String
  module : Option String
  source : Option SourceRange
  statement : Option DeclarationRenderedInfo
  docstring : Option DeclarationRenderedInfo
  attributes : Array String
  proofSearch : DeclarationProofSearchFacts
  flags : DeclarationFlags
  /-- Rendering that actually produced `statement`: `1` = pretty, `0` = raw,
      `none` when no statement was requested. Lets the caller tell whether the
      pretty path fired or fell back to the raw term. -/
  statementRendering : Option Nat
  deriving Inhabited

inductive DeclarationInspectionResult where
  | found (declaration : DeclarationInspection)
  | notFound (name : String)
  | unsupported
  deriving Inhabited

structure ImportStats where
  directImportNames : Array String
  effectiveModuleCount : UInt64
  compactedRegionCount : UInt64
  memoryMappedRegionCount : UInt64
  importedBytes : UInt64
  importedConstantCount : UInt64
  extensionCount : UInt64
  totalImportedExtensionEntries : UInt64
  importLevel : String
  importAll : Bool
  loadExts : Bool
  deriving Inhabited

private def u64 (n : Nat) : UInt64 :=
  UInt64.ofNat n

private def importLevelFromCode? : UInt8 → Option OLeanLevel
  | 0 => some .exported
  | 1 => some .server
  | 2 => some .private
  | _ => none

private def mkImportOptions (profiler traceProfiler : Bool) (traceProfilerOutput : String) : Options :=
  let opts := Options.empty
  let opts := opts.setBool `profiler profiler
  let opts := opts.setBool `trace.profiler traceProfiler
  if traceProfilerOutput.isEmpty then
    opts
  else
    opts.set `trace.profiler.output traceProfilerOutput

private def runSessionImport (searchPaths : Array String) (importNames : Array String)
    (importAll loadExts : Bool) (level : OLeanLevel) (opts : Options) : IO Environment := do
  let sysroot ← Lean.findSysroot
  Lean.initSearchPath sysroot (searchPaths.toList.map System.FilePath.mk)
  let imports := importNames.map fun n => { module := n.toName, importAll := importAll : Import }
  unsafe Lean.enableInitializersExecution
  Lean.importModules imports opts 0 (loadExts := loadExts) (level := level)

@[export lean_rs_host_env_import_stats]
def envImportStats (env : Environment) (importLevel : String) (loadExts : Bool) : IO ImportStats := do
  let mut extensionNames : Array Name := #[]
  let mut extensionEntries : Nat := 0
  for data in env.header.moduleData do
    for (name, entries) in data.entries do
      extensionEntries := extensionEntries + entries.size
      unless extensionNames.contains name do
        extensionNames := extensionNames.push name
  pure {
    directImportNames := env.header.imports.map (·.module.toString)
    effectiveModuleCount := u64 env.header.modules.size
    compactedRegionCount := u64 env.header.regions.size
    memoryMappedRegionCount := u64 (env.header.regions.filter (·.isMemoryMapped) |>.size)
    importedBytes := env.header.regions.foldl (init := 0) fun acc region =>
      acc + UInt64.ofNat region.size.toNat
    importedConstantCount := u64 <| env.constants.fold (init := 0) fun acc _ _ => acc + 1
    extensionCount := u64 extensionNames.size
    totalImportedExtensionEntries := u64 extensionEntries
    importLevel
    importAll := env.header.imports.all (·.importAll)
    loadExts
  }

/-- Initialise the Lean search path and import the named modules into a
    fresh environment. The Rust caller passes the resolved
    `.lake/build/lib/lean` search-path entries (one per Lake package
    whose `.olean`s the import list may reach into — minimally the
    user's own capability package; for `lean-rs-host` consumers also
    the `lean-rs-host-shims` package). The shim does not need to know
    the Lake layout; it just unions whatever entries Rust provides. -/
@[export lean_rs_host_session_import]
def sessionImport (searchPaths : Array String) (importNames : Array String) : IO Environment := do
  -- `loadExts := true` activates scoped environment extensions on
  -- import — including the parser extension. Without it, the
  -- imported environment has only the bootstrap-level builtin
  -- parsers (~7 trailing parsers in the `term` category), so
  -- `Lean.Elab.Frontend.process` / `Parser.runParserCategory`
  -- cannot parse declarations that use library-defined trailing
  -- operators like `+`, `=`, `∧`. With it set, the prompt-15
  -- `elaborate` / `kernel_check` shims see the full operator set
  -- the prelude defines.
  --
  -- Lean 4.30 enforces what earlier releases tolerated: `loadExts := true`
  -- requires `enableInitializersExecution` to have been called first, or
  -- `importModules` throws `IO.userError`. The flag is idempotent and
  -- present on every toolchain in the supported window (4.26+), so we
  -- call it unconditionally rather than version-branching.
  runSessionImport searchPaths importNames false true .private Options.empty

@[export lean_rs_host_session_import_profile]
def sessionImportProfile (searchPaths : Array String) (importNames : Array String)
    (importAll : Bool) (levelCode : UInt8) (loadExts profiler traceProfiler : Bool)
    (traceProfilerOutput : String) : IO Environment := do
  let some level := importLevelFromCode? levelCode |
    throw <| IO.userError s!"unsupported Lean import profiling level code: {levelCode}"
  runSessionImport searchPaths importNames importAll loadExts level <|
    mkImportOptions profiler traceProfiler traceProfilerOutput

@[export lean_rs_host_session_import_profile_progress]
def sessionImportProfileProgress (searchPaths : Array String) (importNames : Array String)
    (importAll : Bool) (levelCode : UInt8) (handle trampoline : USize) : IO (Except UInt8 Environment) := do
  let some level := importLevelFromCode? levelCode |
    throw <| IO.userError s!"unsupported Lean import profile level code: {levelCode}"
  if let some status ← reportProgress? handle trampoline 0 importNames.size then
    return .error status
  let env ← runSessionImport searchPaths importNames importAll true level Options.empty
  if let some status ← reportProgress? handle trampoline importNames.size importNames.size then
    return .error status
  return .ok env

@[export lean_rs_host_session_import_progress]
def sessionImportProgress (searchPaths : Array String) (importNames : Array String)
    (handle trampoline : USize) : IO (Except UInt8 Environment) := do
  if let some status ← reportProgress? handle trampoline 0 importNames.size then
    return .error status
  let env ← sessionImport searchPaths importNames
  if let some status ← reportProgress? handle trampoline importNames.size importNames.size then
    return .error status
  return .ok env

/-- Convert a dotted Rust string into a `Lean.Name`. Pure (no IO);
    `Lean.Name.toName` parses the dotted form (`"Foo.Bar"` ⇒
    `Name.mkStr (Name.mkStr Name.anonymous "Foo") "Bar"`). -/
@[export lean_rs_host_name_from_string]
def nameFromString (s : String) : Name :=
  s.toName

/-- Render a `Lean.Name` as its dotted-string form. Pure (no IO).
    The result is diagnostic text — not a semantic key. Use Lean-side
    equality (or hashing) when you need to compare names. -/
@[export lean_rs_host_name_to_string]
def nameToString (n : Name) : String :=
  n.toString

/-- Render an `Expr` via `Expr.toString`. Pure (no `MetaM`, no `IO`).
    Cheap, deterministic, ugly: walks the syntax tree directly without
    consulting an elaboration context. Useful for indexing, logging, and
    diagnostic dumps. For the form a Lean user reads — notation,
    binders, pretty whitespace — route through the optional
    `lean_rs_host_meta_pp_expr` service instead. -/
@[export lean_rs_host_env_expr_to_string_raw]
def exprToStringRaw (e : Expr) : String :=
  toString e

/-- Look up `name` in `env` and return its declaration, if present and
    convertible. Constants that don't round-trip to `Declaration`
    (constructors, recursors, inductive types, the `Quot` builtin) yield
    `none`; callers that need to know which kind a name is should use
    [`envDeclarationKind`] instead. -/
@[export lean_rs_host_env_query_declaration]
def envQueryDeclaration (env : Environment) (name : Name) : IO (Option Declaration) := do
  match env.find? name with
  | none    => pure none
  | some ci =>
    match ci with
    | .axiomInfo  v => pure (some (.axiomDecl v))
    | .defnInfo   v => pure (some (.defnDecl v))
    | .thmInfo    v => pure (some (.thmDecl v))
    | .opaqueInfo v => pure (some (.opaqueDecl v))
    | _             => pure none

@[export lean_rs_host_env_list_declarations]
def envListDeclarations (env : Environment) : IO (Array Name) := do
  pure <| env.constants.fold (init := #[]) fun acc name _ => acc.push name

private def isGeneratedName (name : Name) : Bool :=
  (name.hasMacroScopes || name.hasNum) && !isPrivateName name

private def isInternalName (name : Name) : Bool :=
  name.isInternalDetail && !isPrivateName name && !isGeneratedName name

private def keepDeclaration (filter : DeclarationFilter) (name : Name) : Bool :=
  (filter.includePrivate != 0 || !isPrivateName name)
    && (filter.includeGenerated != 0 || !isGeneratedName name)
    && (filter.includeInternal != 0 || !isInternalName name)

@[export lean_rs_host_env_list_declarations_filtered]
def envListDeclarationsFiltered (env : Environment) (filter : DeclarationFilter) : IO (Array Name) := do
  let env := if filter.includePrivate != 0 then env.setExporting false else env
  pure <| env.constants.fold (init := #[]) fun acc name _ =>
    if keepDeclaration filter name then acc.push name else acc

@[export lean_rs_host_env_list_declarations_filtered_progress]
def envListDeclarationsFilteredProgress (env : Environment) (filter : DeclarationFilter)
    (handle trampoline : USize) : IO (Except UInt8 (Array Name)) := do
  let env := if filter.includePrivate != 0 then env.setExporting false else env
  let mut out := #[]
  let mut seen := 0
  for (name, _) in env.constants.toList do
    seen := seen + 1
    if keepDeclaration filter name then
      out := out.push name
    if seen % 1024 == 0 then
      if let some status ← reportProgress? handle trampoline out.size 0 then
        return .error status
  if let some status ← reportProgress? handle trampoline out.size 0 then
    return .error status
  return .ok out

private def moduleSourcePath (moduleName : Name) : System.FilePath :=
  System.FilePath.mk <| moduleName.toString.replace "." System.FilePath.pathSeparator.toString ++ ".lean"

private def declarationModule? (env : Environment) (declName : Name) : Option Name :=
  match env.getModuleIdxFor? declName with
  | none => none
  | some moduleIdx => some env.allImportedModuleNames[moduleIdx.toNat]!

private def findDeclarationFile? (sourceRoots : Array String) (moduleName : Name) : IO (Option String) := do
  let rel := moduleSourcePath moduleName
  for root in sourceRoots do
    let candidate := System.FilePath.mk root / rel
    if ← candidate.pathExists then
      return some (← IO.FS.realPath candidate).toString
  return none

private def findDeclarationRangesInEnv? (env : Environment) (name : Name) : IO (Option DeclarationRanges) := do
  let coreCtx : Core.Context := { fileName := "<declaration-source-range>", fileMap := default, options := {} }
  let coreState : Core.State := { env }
  let action : CoreM (Option DeclarationRanges) := findDeclarationRanges? name
  let eio : EIO Exception (Option DeclarationRanges) := (action coreCtx).run' coreState
  match ← eio.toBaseIO with
  | .ok range? => pure range?
  | .error ex =>
    let msg ← ex.toMessageData.toString
    throw <| IO.userError msg

@[export lean_rs_host_env_declaration_source_range]
def envDeclarationSourceRange (env : Environment) (name : Name) (sourceRoots : Array String)
    : IO (Option SourceRange) := do
  if (env.find? name).isNone then
    return none
  let some ranges ← findDeclarationRangesInEnv? env name
    | return none
  let moduleName? := declarationModule? env name
  let moduleLabel := moduleName?.map Name.toString |>.getD "<unknown>"
  let file ← match moduleName? with
    | none => pure moduleLabel
    | some moduleName =>
      pure <| (← findDeclarationFile? sourceRoots moduleName).getD moduleLabel
  let range := ranges.range
  pure <| some {
    file
    startLine := range.pos.line
    startColumn := range.pos.column + 1
    endLine := range.endPos.line
    endColumn := range.endPos.column + 1
  }

@[export lean_rs_host_env_declaration_type]
def envDeclarationType (env : Environment) (name : Name) : IO (Option Expr) := do
  pure <| (env.find? name).map ConstantInfo.type

@[export lean_rs_host_env_declaration_type_bulk]
def envDeclarationTypeBulk (env : Environment) (names : Array String)
    : IO (Array (Option Expr)) := do
  if names.isEmpty then
    pure #[]
  else
    let first := names[0]!
    if names.all (· == first) then
      let type? ← envDeclarationType env first.toName
      pure <| Array.replicate names.size type?
    else
      let mut cache : Std.HashMap String (Option Expr) := {}
      let mut out := #[]
      for name in names do
        match cache.get? name with
        | some type? => out := out.push type?
        | none =>
          let type? ← envDeclarationType env name.toName
          cache := cache.insert name type?
          out := out.push type?
      pure out

@[export lean_rs_host_env_declaration_type_bulk_progress]
def envDeclarationTypeBulkProgress (env : Environment) (names : Array String)
    (handle trampoline : USize) : IO (Except UInt8 (Array (Option Expr))) := do
  let mut cache : Std.HashMap String (Option Expr) := {}
  let mut out := #[]
  let mut idx := 0
  for name in names do
    match cache.get? name with
    | some type? => out := out.push type?
    | none =>
      let type? ← envDeclarationType env name.toName
      cache := cache.insert name type?
      out := out.push type?
    idx := idx + 1
    if let some status ← reportProgress? handle trampoline idx names.size then
      return .error status
  return .ok out

@[export lean_rs_host_env_declaration_kind]
def envDeclarationKind (env : Environment) (name : Name) : IO String := do
  match env.find? name with
  | none                 => pure "missing"
  | some (.axiomInfo  _) => pure "axiom"
  | some (.defnInfo   _) => pure "definition"
  | some (.thmInfo    _) => pure "theorem"
  | some (.opaqueInfo _) => pure "opaque"
  | some (.quotInfo   _) => pure "quot"
  | some (.inductInfo _) => pure "inductive"
  | some (.ctorInfo   _) => pure "constructor"
  | some (.recInfo    _) => pure "recursor"

@[export lean_rs_host_env_declaration_kind_bulk]
def envDeclarationKindBulk (env : Environment) (names : Array String)
    : IO (Array String) := do
  if names.isEmpty then
    pure #[]
  else
    let first := names[0]!
    if names.all (· == first) then
      let kind ← envDeclarationKind env first.toName
      pure <| Array.replicate names.size kind
    else
      let mut cache : Std.HashMap String String := {}
      let mut out := #[]
      for name in names do
        match cache.get? name with
        | some kind => out := out.push kind
        | none =>
          let kind ← envDeclarationKind env name.toName
          cache := cache.insert name kind
          out := out.push kind
      pure out

@[export lean_rs_host_env_declaration_kind_bulk_progress]
def envDeclarationKindBulkProgress (env : Environment) (names : Array String)
    (handle trampoline : USize) : IO (Except UInt8 (Array String)) := do
  let mut cache : Std.HashMap String String := {}
  let mut out := #[]
  let mut idx := 0
  for name in names do
    match cache.get? name with
    | some kind => out := out.push kind
    | none =>
      let kind ← envDeclarationKind env name.toName
      cache := cache.insert name kind
      out := out.push kind
    idx := idx + 1
    if let some status ← reportProgress? handle trampoline idx names.size then
      return .error status
  return .ok out

@[export lean_rs_host_env_declaration_name]
def envDeclarationName (_env : Environment) (name : Name) : IO String := do
  pure name.toString

@[export lean_rs_host_env_declaration_name_bulk]
def envDeclarationNameBulk (_env : Environment) (names : Array String)
    : IO (Array String) := do
  if names.isEmpty then
    pure #[]
  else
    let first := names[0]!
    if names.all (· == first) then
      let rendered := first.toName.toString
      pure <| Array.replicate names.size rendered
    else
      let mut cache : Std.HashMap String String := {}
      let mut out := #[]
      for name in names do
        match cache.get? name with
        | some rendered => out := out.push rendered
        | none =>
          let rendered := name.toName.toString
          cache := cache.insert name rendered
          out := out.push rendered
      pure out

@[export lean_rs_host_env_declaration_name_bulk_progress]
def envDeclarationNameBulkProgress (_env : Environment) (names : Array String)
    (handle trampoline : USize) : IO (Except UInt8 (Array String)) := do
  let mut cache : Std.HashMap String String := {}
  let mut out := #[]
  let mut idx := 0
  for name in names do
    match cache.get? name with
    | some rendered => out := out.push rendered
    | none =>
      let rendered := name.toName.toString
      cache := cache.insert name rendered
      out := out.push rendered
    idx := idx + 1
    if let some status ← reportProgress? handle trampoline idx names.size then
      return .error status
  return .ok out

private structure DeclarationSearchCandidate where
  name : Name
  nameString : String
  kind : String
  moduleName : Option String
  matchReason : String
  score : Int
  flags : DeclarationFlags
  deriving Inhabited

private def elapsedMicrosSince (startMs : Nat) : IO Nat := do
  let nowMs ← IO.monoMsNow
  pure ((nowMs - startMs) * 1000)

private def clampSearchLimit (limit : Nat) : Nat :=
  max 1 (min limit 100)

private def isBoundaryPrefix (pref value : String) : Bool :=
  value == pref || value.startsWith (pref ++ ".")

private def declarationKindOf : ConstantInfo → String
  | .axiomInfo  _ => "axiom"
  | .defnInfo   _ => "definition"
  | .thmInfo    _ => "theorem"
  | .opaqueInfo _ => "opaque"
  | .quotInfo   _ => "quot"
  | .inductInfo _ => "inductive"
  | .ctorInfo   _ => "constructor"
  | .recInfo    _ => "recursor"

private partial def forallConclusion : Expr → Expr
  | .forallE _ _ body _ => forallConclusion body
  | e => e

private def conclusionHead? (type : Expr) : Option Name :=
  match (forallConclusion type).getAppFn with
  | .const name _ => some name
  | _ => none

private def requiredConstantsPresent (required : Array String) (type : Expr) : Bool :=
  let used := type.getUsedConstantsAsSet
  required.all (fun name => used.contains name.toName)

private def matchesNameFragment (request : DeclarationSearchRequest) (name : String) : Bool :=
  match request.nameFragment with
  | none => true
  | some fragment =>
    if fragment.isEmpty then
      true
    else
      let haystack := name.toLower
      let needle := fragment.toLower
      if request.nameMatch == 1 then
        haystack.endsWith needle
      else
        haystack.contains needle

private def matchesStrictScope (request : DeclarationSearchRequest) (name : String) (moduleName : Option String) :
    Bool :=
  request.scopeBiases.all fun bias =>
    if bias.isStrict == 0 then
      true
    else
      if bias.scope == 1 then
        moduleName.any (fun moduleName => isBoundaryPrefix bias.scopePrefix moduleName)
      else
        isBoundaryPrefix bias.scopePrefix name

private def scopeBiasScore (request : DeclarationSearchRequest) (name : String) (moduleName : Option String) :
    Int :=
  request.scopeBiases.foldl (init := 0) fun score bias =>
    let doesMatch :=
      if bias.scope == 1 then
        moduleName.any (fun moduleName => isBoundaryPrefix bias.scopePrefix moduleName)
      else
        isBoundaryPrefix bias.scopePrefix name
    if doesMatch then score + (bias.weight.toInt?.getD 0) else score

private def isBroadDeclarationSearch (request : DeclarationSearchRequest) : Bool :=
  request.nameFragment.all String.isEmpty
    && request.requiredConstants.isEmpty
    && request.conclusionHead.isNone
    && request.scopeBiases.all (fun bias => bias.isStrict == 0)

private def searchMatchReason (request : DeclarationSearchRequest) : String := Id.run do
  let mut reasons := #[]
  if request.nameFragment.any (fun fragment => !fragment.isEmpty) then
    reasons := reasons.push "name"
  if request.kind.isSome then
    reasons := reasons.push "kind"
  if !request.requiredConstants.isEmpty then
    reasons := reasons.push "required_constants"
  if request.conclusionHead.isSome then
    reasons := reasons.push "conclusion_head"
  if request.scopeBiases.any (fun bias => bias.isStrict != 0) then
    reasons := reasons.push "strict_scope"
  if reasons.isEmpty then
    "broad"
  else
    String.intercalate "," reasons.toList

private def declarationSearchBaseScore (request : DeclarationSearchRequest) (name : String) (kind : String)
    (moduleName : Option String) : Int := Id.run do
  let mut score : Int := 0
  if request.nameFragment.any (fun fragment => !fragment.isEmpty && name.toLower == fragment.toLower) then
    score := score + 100
  if request.nameFragment.any (fun fragment => !fragment.isEmpty && name.toLower.endsWith fragment.toLower) then
    score := score + 40
  if request.kind == some kind then
    score := score + 20
  if !request.requiredConstants.isEmpty then
    score := score + 30
  if request.conclusionHead.isSome then
    score := score + 30
  score + scopeBiasScore request name moduleName

private def candidateLess (a b : DeclarationSearchCandidate) : Bool :=
  a.score > b.score || (a.score == b.score && a.nameString < b.nameString)

@[export lean_rs_host_env_search_declarations]
def envSearchDeclarations (env : Environment) (request : DeclarationSearchRequest) (sourceRoots : Array String)
    : IO DeclarationSearchResult := do
  let scanStart ← IO.monoMsNow
  let limit := clampSearchLimit request.limit
  let required := request.requiredConstants
  let conclusionHead := request.conclusionHead.map String.toName
  let reason := searchMatchReason request
  let mut scanned := 0
  let mut afterName := 0
  let mut afterKind := 0
  let mut afterRequired := 0
  let mut afterConclusion := 0
  let mut afterScope := 0
  let mut candidates := #[]
  for (name, info) in env.constants.toList do
    scanned := scanned + 1
    unless keepDeclaration request.filter name do
      continue
    let nameString := name.toString
    unless matchesNameFragment request nameString do
      continue
    afterName := afterName + 1
    let kind := declarationKindOf info
    if request.kind.any (· != kind) then
      continue
    afterKind := afterKind + 1
    unless requiredConstantsPresent required info.type do
      continue
    afterRequired := afterRequired + 1
    if conclusionHead.any (fun wanted => conclusionHead? info.type != some wanted) then
      continue
    afterConclusion := afterConclusion + 1
    let moduleName := (declarationModule? env name).map Name.toString
    unless matchesStrictScope request nameString moduleName do
      continue
    afterScope := afterScope + 1
    let flags : DeclarationFlags := {
      isPrivate := if isPrivateName name then 1 else 0
      isGenerated := if isGeneratedName name then 1 else 0
      isInternal := if isInternalName name then 1 else 0
    }
    candidates := candidates.push {
      name
      nameString
      kind
      moduleName
      matchReason := reason
      score := declarationSearchBaseScore request nameString kind moduleName
      flags
    }
  let scanMicros ← elapsedMicrosSince scanStart
  let rankStart ← IO.monoMsNow
  let ranked := candidates.qsort candidateLess
  let truncated := ranked.size > limit
  let selected := ranked.extract 0 (min ranked.size limit)
  let rankMicros ← elapsedMicrosSince rankStart
  let sourceStart ← IO.monoMsNow
  let mut rows := #[]
  let mut sourceLookups := 0
  for idx in [:selected.size] do
    let candidate := selected[idx]!
    let source ←
      if request.includeSource != 0 then
        sourceLookups := sourceLookups + 1
        envDeclarationSourceRange env candidate.name sourceRoots
      else
        pure none
    rows := rows.push {
      name := candidate.nameString
      kind := candidate.kind
      module := candidate.moduleName
      source
      matchReason := candidate.matchReason
      score := toString candidate.score
      rank := idx + 1
      flags := candidate.flags
    }
  let sourceMicros ← elapsedMicrosSince sourceStart
  let broadPruning :=
    if isBroadDeclarationSearch request && truncated then
      #[{
        stage := "limit"
        reason := "broad_search_limit"
        count := ranked.size - selected.size
      }]
    else
      #[]
  let timings : DeclarationSearchTimings := {
    scanMicros
    rankMicros
    sourceMicros
  }
  let facts : DeclarationSearchFacts := {
    declarationsScanned := scanned
    afterNameFilter := afterName
    afterKindFilter := afterKind
    afterRequiredConstantsFilter := afterRequired
    afterConclusionFilter := afterConclusion
    afterScopeFilter := afterScope
    sourceLookups := sourceLookups
    broadPruning
    truncated := if truncated then 1 else 0
    timings
  }
  pure {
    declarations := rows
    truncated := if truncated then 1 else 0
    facts
  }

private def natOfBool (value : Bool) : Nat :=
  if value then 1 else 0

private def takeUtf8Bytes (limit : Nat) (text : String) : String × Bool := Id.run do
  if text.utf8ByteSize <= limit then
    return (text, false)
  let mut out := ""
  let mut bytes := 0
  for c in text.toList do
    let cBytes := c.toString.utf8ByteSize
    if bytes + cBytes <= limit then
      out := out.push c
      bytes := bytes + cBytes
  (out, true)

private def boundText (text : String) (perFieldBytes totalRemaining : Nat) : DeclarationRenderedInfo × Nat :=
  let limit := min perFieldBytes totalRemaining
  let (value, fieldTruncated) := takeUtf8Bytes limit text
  let truncated := fieldTruncated || text.utf8ByteSize > totalRemaining
  ({ value, truncated := natOfBool truncated }, value.utf8ByteSize)

/-- Pretty-print an `Expr` via `Lean.PrettyPrinter.ppExpr` with `pp.universes`
    disabled, so a declaration's statement reads like an editor `hover` rather
    than a fully-elaborated term with every universe and instance argument
    spelled out. Heartbeat-bounded; returns `none` when the pretty-printer
    raises (deeply nested term under the budget, or any rendering error) so the
    caller can fall back to the raw `Expr.toString` form. -/
private def ppExprBounded (env : Environment) (expr : Expr) (heartbeats : UInt64) : IO (Option String) := do
  let opts : Options := Lean.maxHeartbeats.set ({} : Options) heartbeats.toNat
  let opts := opts.setBool `pp.universes false
  let coreCtx : Core.Context := { fileName := "<declaration-inspection>", fileMap := default, options := opts }
  let coreState : Core.State := { env }
  let metaAction : Meta.MetaM String := do
    let fmt ← Lean.PrettyPrinter.ppExpr expr
    pure fmt.pretty
  let coreAction : CoreM String := metaAction.run' {} {}
  match ← ((coreAction coreCtx).run' coreState).toBaseIO with
  | .ok rendered => pure (some rendered)
  | .error _ => pure none

private def isSimpDeclarationCore (declName : Name) : CoreM Bool := do
  let some ext ← Lean.Meta.getSimpExtension? `simp | return false
  let thms ← ext.getTheorems
  return thms.isLemma (.decl declName true false) ||
    thms.isLemma (.decl declName false false) ||
    thms.isLemma (.decl declName true true) ||
    thms.isLemma (.decl declName false true) ||
    thms.isDeclToUnfold declName

private def runCoreBool (env : Environment) (action : CoreM Bool) : IO Bool := do
  let coreCtx : Core.Context := { fileName := "<declaration-inspection>", fileMap := default, options := {} }
  let coreState : Core.State := { env }
  match ← ((action coreCtx).run' coreState).toBaseIO with
  | .ok value => pure value
  | .error _ => pure false

private def isSimpDeclaration (env : Environment) (declName : Name) : IO Bool :=
  runCoreBool env (isSimpDeclarationCore declName)

private def isInstanceDeclaration (env : Environment) (declName : Name) : Bool :=
  Lean.Meta.isInstanceCore env declName

private def isClassDeclaration (env : Environment) (declName : Name) : Bool :=
  Lean.isClass env declName

private def rwCandidateHead (head : Option Name) : Bool :=
  head == some ``Eq || head == some ``Iff

private def proofSearchFacts (env : Environment) (declName : Name) (type : Expr) : IO DeclarationProofSearchFacts := do
  let isSimp ← isSimpDeclaration env declName
  let isClass := isClassDeclaration env declName
  let head := conclusionHead? type
  let className :=
    if isClass then
      some declName.toString
    else
      head.bind fun name => if isClassDeclaration env name then some name.toString else none
  pure {
    isSimp := natOfBool isSimp
    isRwCandidate := natOfBool (rwCandidateHead head)
    isInstance := natOfBool (isInstanceDeclaration env declName)
    isClass := natOfBool isClass
    className
  }

private def reducibilityAttribute? : ConstantInfo → Option String
  | .defnInfo info =>
    match info.hints with
    | .abbrev => some "abbrev"
    | .opaque => some "irreducible"
    | .regular _ => none
  | .opaqueInfo _ => some "opaque"
  | _ => none

private def inspectionAttributes (info : ConstantInfo) (facts : DeclarationProofSearchFacts) : Array String := Id.run do
  let mut attrs := #[]
  if facts.isSimp != 0 then
    attrs := attrs.push "simp"
  if facts.isRwCandidate != 0 then
    attrs := attrs.push "rw"
  if facts.isInstance != 0 then
    attrs := attrs.push "instance"
  if facts.isClass != 0 then
    attrs := attrs.push "class"
  if let some attr := reducibilityAttribute? info then
    attrs := attrs.push attr
  attrs

@[export lean_rs_host_env_inspect_declaration]
def envInspectDeclaration (env : Environment) (request : DeclarationInspectionRequest) (sourceRoots : Array String)
    (heartbeats : UInt64) : IO DeclarationInspectionResult := do
  let declName := request.name.toName
  let some info := env.find? declName
    | return .notFound request.name
  let canonicalName := declName.toString
  let kind := declarationKindOf info
  let moduleName := (declarationModule? env declName).map Name.toString
  let source ←
    if request.fields.source != 0 then
      envDeclarationSourceRange env declName sourceRoots
    else
      pure none
  let flags : DeclarationFlags := {
    isPrivate := natOfBool (isPrivateName declName)
    isGenerated := natOfBool (isGeneratedName declName)
    isInternal := natOfBool (isInternalName declName)
  }
  let facts ← proofSearchFacts env declName info.type
  let attributes :=
    if request.fields.attributes != 0 then
      inspectionAttributes info facts
    else
      #[]
  let mut spent := 0
  let mut statement := none
  let mut statementRendering := none
  if request.fields.statement != 0 then
    -- Pretty (notation-aware, `pp.universes false`) when requested and the
    -- pretty-printer succeeds; otherwise the raw fully-elaborated term.
    let (text, usedRendering) ←
      if request.fields.rendering != 0 then
        match ← ppExprBounded env info.type heartbeats with
        | some pretty => pure (pretty, 1)
        | none => pure (toString info.type, 0)
      else
        pure (toString info.type, 0)
    let (rendered, used) := boundText text request.budgets.perFieldBytes
      (request.budgets.totalBytes - spent)
    spent := spent + used
    statement := some rendered
    statementRendering := some usedRendering
  let mut docstring := none
  if request.fields.docstring != 0 then
    match ← findDocString? env declName false with
    | none => pure ()
    | some doc =>
      let (rendered, used) := boundText doc request.budgets.perFieldBytes
        (request.budgets.totalBytes - spent)
      spent := spent + used
      docstring := some rendered
  pure <| .found {
    name := canonicalName
    kind
    module := moduleName
    source
    statement
    docstring
    attributes
    proofSearch := facts
    flags := if request.fields.flags != 0 then flags else default
    statementRendering
  }

/-- Bulk variant of [`envQueryDeclaration`]: a single IO traversal that
    folds the singular lookup across `names`. One Lean traversal, one
    `MessageLog`-less FFI crossing. Iteration semantics are identical to a
    Rust-side fold over the singular path. -/
@[export lean_rs_host_env_query_declarations_bulk]
def envQueryDeclarationsBulk (env : Environment) (names : Array Name)
    : IO (Array (Option Declaration)) := do
  names.mapM (envQueryDeclaration env)

@[export lean_rs_host_env_query_declarations_bulk_progress]
def envQueryDeclarationsBulkProgress (env : Environment) (names : Array Name)
    (handle trampoline : USize) : IO (Except UInt8 (Array (Option Declaration))) := do
  let mut out := #[]
  let mut idx := 0
  for name in names do
    let decl? ← envQueryDeclaration env name
    out := out.push decl?
    idx := idx + 1
    if let some status ← reportProgress? handle trampoline idx names.size then
      return .error status
  return .ok out

end LeanRsFixture.Environment
