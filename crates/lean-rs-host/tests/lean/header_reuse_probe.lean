-- Appended to InfoTree.lean by the Rust regression test, so it can inspect
-- private cache state without adding any exported test API.
namespace LeanRsFixture.InfoTree
open Lean
def runHeaderReuseProbe : IO Unit := do
  unsafe Lean.enableInitializersExecution
  let env ← Lean.importModules #[{ module := `Lean }] {} (loadExts := true)
  configureHeaderImportBudget 1000000000 1
  let source := "module\nimport Lean\n\ndef headerReuseA : Nat := 1\n"
  let policy := oneShotPolicy source "header-reuse.lean"
  let .ok (first, _) ← buildModuleSnapshot env source "" "header-reuse.lean" 100000 65536 "{}" policy
    | throw <| IO.userError "first snapshot failed"
  let .ok (warm, _) ← buildModuleSnapshot env (source ++ "def headerReuseB : Nat := 2\n")
      "" "header-reuse.lean" 100000 65536 "{}" policy (some first)
    | throw <| IO.userError "warm snapshot failed"
  unless unsafe ptrEq first.baseEnv warm.baseEnv do
    throw <| IO.userError "unchanged module header was imported again"
  unless (reusableIncrementalState? first warm.baseEnv 100000 "" "{}").isSome do
    throw <| IO.userError "header reuse did not preserve incremental state"
  let reject (result : Option (Environment × MessageLog)) (reason : String) := do
    unless result.isNone do throw <| IO.userError reason
  let some firstHeader := (← headerImportState.get).headers[0]?
    | throw <| IO.userError "missing retained header"
  let failed := { firstHeader with messages := firstHeader.messages.add {
    fileName := first.fileLabel, pos := ⟨1, 0⟩, data := "transient import failure"
  } }
  reject (reusableHeader? failed env first.headerSource first.fileLabel 100000 "{}")
    "reused a failed header instead of retrying its import"
  reject (reusableHeader? firstHeader (env.setMainModule `Other) first.headerSource first.fileLabel 100000 "{}")
    "reused header from another session"
  reject (reusableHeader? firstHeader env (first.headerSource ++ "\n") first.fileLabel 100000 "{}")
    "reused changed header"
  reject (reusableHeader? firstHeader env first.headerSource first.fileLabel 200000 "{}")
    "reused changed header options"
  reject (reusableHeader? firstHeader env first.headerSource first.fileLabel 100000 "{\"autoImplicit\":false}")
    "reused header with a changed option map"
  unless (reusableIncrementalState? first first.baseEnv 100000 "" "{\"autoImplicit\":false}").isNone do
    throw <| IO.userError "reused commands with changed options"
  reject (reusableHeader? firstHeader env first.headerSource "other.lean" 100000 "{}")
    "reused changed file context"
  let mut bounded : Array CachedModuleHeader := #[]
  for i in [:12] do
    bounded := rememberModuleHeader bounded { firstHeader with
      inputEnv := env.setMainModule (Name.num `Session i) }
  unless bounded.size == 8 do throw <| IO.userError "header cache exceeded global bound"
  bounded := rememberModuleHeader bounded firstHeader
  unless bounded.size == 8 do throw <| IO.userError "header insertion broke bound"
  bounded := rememberModuleHeader bounded firstHeader
  unless bounded.size == 8 do throw <| IO.userError "warm header duplicated cache entry"
  let before ← headerImportState.get
  let _ ← clearModuleSnapshotCache ()
  let .ok (afterClear, _) ← buildModuleSnapshot env source "" "header-reuse.lean" 100000 65536 "{}" policy
    | throw <| IO.userError "snapshot eviction lost header"
  unless unsafe ptrEq first.baseEnv afterClear.baseEnv do
    throw <| IO.userError "snapshot clear forced a fresh import"
  unless (← headerImportState.get).imports == before.imports do
    throw <| IO.userError "warm header charged an import"
  configureHeaderImportBudget 1000000000 1
  let .ok (_, _) ← buildModuleSnapshot env source "" "header-reuse.lean" 100000 65536 "{}" policy
    | throw <| IO.userError "warm header blocked above budget"
  let changed ← buildModuleSnapshot env (source.replace "import Lean" "public import Lean")
    "" "header-reuse.lean" 100000 65536 "{}" policy
  unless changed matches .error _ do throw <| IO.userError "fresh header exceeded reached budget"
  unless (← headerImportState.get).recycleRequested do
    throw <| IO.userError "budget did not request recycle"
  unless (← headerImportState.get).imports == before.imports do
    throw <| IO.userError "blocked header imported anyway"
  configureHeaderImportBudget 0 0
  clearModuleHeaderCache env
  unless (← headerImportState.get).headers.isEmpty do
    throw <| IO.userError "session eviction retained header"
  unless (← headerImportState.get).bytes == before.bytes do
    throw <| IO.userError "session eviction subtracted retained bytes"
  let badSource := "module\nimport MissingHeaderRegressionModule\n"
  let badInput := Parser.mkInputContext badSource "missing-header.lean"
  let (badHeader, _, badMessages) ← Lean.Parser.parseHeader badInput
  let (_, importErrors) ← resolveModuleHeader env badHeader {} badMessages badInput
    badSource "missing-header.lean" 100000 "{}"
  unless importErrors.hasErrors do throw <| IO.userError "missing import had no error"
  unless (← headerImportState.get).accountingUnknown do
    throw <| IO.userError "failed import did not taint accounting"
  let unknown ← buildModuleSnapshot env source "" "header-reuse.lean" 100000 65536 "{}" policy
  unless unknown matches .error _ do throw <| IO.userError "unknown import accounting permitted work"
  unless (← headerImportState.get).recycleRequested do
    throw <| IO.userError "unknown accounting did not request recycle"
end LeanRsFixture.InfoTree
