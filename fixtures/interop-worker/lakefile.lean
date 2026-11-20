import Lake
open System Lake DSL

def workerBuildDir : FilePath :=
  if Lean.versionString == "4.36.0" then
    ".lake" / "lean-rs-generated" / "lean-4.36.0"
  else
    defaultBuildDir

/-! Build the callback fixture's original sources with the host's interop provider.
The standalone callback fixture keeps its own dependency and build directory. -/
package «lean_rs_interop_consumer» where
  buildDir := workerBuildDir

require «lean_rs_interop_shims» from "../../crates/lean-rs-host/shims/lean-rs-interop-shims" with
  (Lean.mkNameMap String).insert `leanRsGeneratedShims "true"

@[default_target]
lean_lib «LeanRsInteropConsumer» where
  srcDir := "../interop-shims"
  leanOptions := #[
    ⟨`autoImplicit, false⟩,
    ⟨`maxSynthPendingDepth, .ofNat 3⟩,
    ⟨`pp.unicode.fun, true⟩,
  ]
  defaultFacets := #[LeanLib.sharedFacet]
