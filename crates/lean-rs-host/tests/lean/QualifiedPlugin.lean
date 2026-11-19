import Lean
def qualifiedPluginWitness : Nat := 73

initialize
  if let some path ← IO.getEnv "LEAN_RS_TEST_PLUGIN_MARKER" then
    IO.FS.writeFile path "initialized"
