import LeanRsHostShims.Environment

open Lean LeanRsFixture.Environment

def main (args : List String) : IO Unit := do
  let project :: dep :: shims := args | throw <| IO.userError "expected project, dependency and shim paths"
  let explicit := (project :: dep :: shims).toArray
  let reversed := (dep :: project :: shims).toArray
  let sysroot ← Lean.findSysroot
  let oracle (roots : Array String) := do
    Lean.initSearchPath sysroot (roots.toList.map System.FilePath.mk)
    return (← Lean.searchPathRef.get).toArray.map (·.toString)
  let reversedOrder ← oracle reversed
  let order ← oracle explicit
  let sentinel : Lean.SearchPath := [System.FilePath.mk "unrelated-serving-session"]
  Lean.searchPathRef.set sentinel
  let (some env, paths) ← sessionImportPathsV1 explicit #["OrderSelection"] false 0 false none none false
    | throw <| IO.userError "fresh import returned no environment"
  unless env.contains `fromProjectRoot && !env.contains `fromDependencyRoot && paths == order do throw <| IO.userError s!"unexpected import result: {paths}"

  unless (← Lean.searchPathRef.get).toArray.map (·.toString) == order do
    throw <| IO.userError "fresh import did not install the observed order"

  -- This name cannot import. Both a warm comparison and a rejected open must
  -- finish before import and leave the serving path alone.
  Lean.searchPathRef.set sentinel
  let check (requested expected : Array String) (compareOnly : Bool) := do
    let (env?, actual) ← sessionImportPathsV1 requested #["MissingModuleForPathCheck"] false 0 false
      none (some paths) compareOnly
    unless env?.isNone && actual == expected do throw <| IO.userError s!"unexpected comparison: {actual}"
    unless (← Lean.searchPathRef.get) == sentinel do
      throw <| IO.userError "comparison changed the serving search path"
  check explicit order true
  check reversed reversedOrder true
  check reversed reversedOrder false
  let (some fresh, freshPaths) ← sessionImportPathsV1 reversed #["OrderSelection"] false 0 false
      (some (false, false, "")) none false
    | throw <| IO.userError "fresh reversed import returned no environment"
  unless fresh.contains `fromDependencyRoot && !fresh.contains `fromProjectRoot && freshPaths == reversedOrder do
    throw <| IO.userError "fresh import did not select the dependency module"
