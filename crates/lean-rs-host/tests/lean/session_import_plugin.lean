import LeanRsHostShims.Environment

open Lean LeanRsFixture.Environment

def main (args : List String) : IO Unit := do
  let [root, image, marker] := args | throw <| IO.userError "expected module root, plugin and marker"
  let marker := System.FilePath.mk marker
  let deployment := (#[root], image, "initialize_QualifiedPlugin")
  let sentinel : Lean.SearchPath := [System.FilePath.mk "serving-plugin-session"]
  Lean.searchPathRef.set sentinel
  let (compared, _) ← sessionImportPluginV1 deployment #["QualifiedPlugin"] false 2 true none none true
  unless compared.isNone && (← Lean.searchPathRef.get) == sentinel && !(← marker.pathExists) do
    throw <| IO.userError "plugin comparison changed state"
  let (mismatched, _) ← sessionImportPluginV1 deployment #["QualifiedPlugin"] false 2 true none (some #[]) false
  unless mismatched.isNone && (← Lean.searchPathRef.get) == sentinel && !(← marker.pathExists) do
    throw <| IO.userError "mismatched roots ran native code"
  let wrong := (#[root], image, "unexported_plugin_initializer")
  let wrongSymbol ← (sessionImportPluginV1 wrong #["QualifiedPlugin"] false 2 true none none false).toBaseIO
  match wrongSymbol with
  | .ok _ => throw <| IO.userError "plugin import silently substituted an initializer"
  | .error e => unless e.toString.contains "unexported_plugin_initializer" && !(← marker.pathExists) do
      throw <| IO.userError s!"unexpected initializer failure: {e}"
  let failed ← (sessionImportPluginV1 deployment #["KnownMissingAfterPlugin"] false 2 true none none false).toBaseIO
  match failed with
  | .ok _ => throw <| IO.userError "missing module import succeeded"
  | .error e => unless e.toString.contains "KnownMissingAfterPlugin" && (← IO.FS.readFile marker) == "initialized" do
      throw <| IO.userError s!"expected a module failure after plugin initialization: {e}"
  let (some env, _) ← sessionImportPluginV1 deployment #["QualifiedPlugin"] false 2 true none none false
    | throw <| IO.userError "plugin import returned no environment"
  unless env.contains `qualifiedPluginWitness && (← IO.FS.readFile marker) == "initialized" do
    throw <| IO.userError "expected the compiled module and its native initializer"
