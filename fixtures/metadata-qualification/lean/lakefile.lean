import Lake
open System Lake DSL

abbrev leanRsLeanOptions : Array LeanOption := #[
  ⟨`autoImplicit, false⟩,
  ⟨`maxSynthPendingDepth, .ofNat 3⟩,
  ⟨`pp.unicode.fun, true⟩,
]

def leanRsBuildDir : FilePath :=
  if Lean.versionString == "4.36.0" then
    ".lake" / "lean-rs-generated" / "lean-4.36.0"
  else
    defaultBuildDir

package «lean_rs_interop_consumer» where
  buildDir := leanRsBuildDir

require «lean_rs_interop_shims» from "../../../crates/lean-rs-host/shims/lean-rs-interop-shims" with
  (Lean.mkNameMap String).insert `leanRsGeneratedShims "true"

@[default_target]
lean_lib «LeanRsInteropConsumer» where
  leanOptions := leanRsLeanOptions
  defaultFacets := #[LeanLib.sharedFacet]
