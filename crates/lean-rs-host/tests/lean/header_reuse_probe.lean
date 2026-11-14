-- Appended to InfoTree.lean by the Rust regression test, so it can inspect
-- private cache state without adding any exported test API.
namespace LeanRsFixture.InfoTree
open Lean
#eval show IO Unit from do
  unsafe Lean.enableInitializersExecution
  let env ← Lean.importModules #[{ module := `Lean }] {} (loadExts := true)
  let source := "module\nimport Lean\n\ndef headerReuseA : Nat := 1\n"
  let policy := oneShotPolicy source "header-reuse.lean"
  let .ok (first, _) ← buildModuleSnapshot env source "" "header-reuse.lean" 100000 65536 policy
    | throw <| IO.userError "first snapshot failed"
  let .ok (warm, _) ← buildModuleSnapshot env (source ++ "def headerReuseB : Nat := 2\n")
      "" "header-reuse.lean" 100000 65536 policy (some first)
    | throw <| IO.userError "warm snapshot failed"
  unless unsafe ptrEq first.baseEnv warm.baseEnv do
    throw <| IO.userError "unchanged module header was imported again"
  unless (reusableIncrementalState? first warm.baseEnv 100000 "").isSome do
    throw <| IO.userError "header reuse did not preserve incremental state"
  let reject (result : Option (Environment × MessageLog)) (reason : String) := do
    unless result.isNone do throw <| IO.userError reason
  let failed := { first with headerMessages := first.headerMessages.add {
    fileName := first.fileLabel, pos := ⟨1, 0⟩, data := "transient import failure"
  } }
  reject (reusableHeader? failed env first.headerSource first.fileLabel 100000)
    "reused a failed header instead of retrying its import"
  reject (reusableHeader? first (env.setMainModule `Other) first.headerSource first.fileLabel 100000)
    "reused header from another session"
  reject (reusableHeader? first env (first.headerSource ++ "\n") first.fileLabel 100000)
    "reused changed header"
  reject (reusableHeader? first env first.headerSource first.fileLabel 200000)
    "reused changed header options"
  reject (reusableHeader? first env first.headerSource "other.lean" 100000)
    "reused changed file context"
end LeanRsFixture.InfoTree
