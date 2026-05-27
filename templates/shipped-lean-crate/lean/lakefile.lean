import Lake
open Lake DSL

abbrev leanRsLeanOptions : Array LeanOption := #[
  ⟨`autoImplicit, false⟩,
  ⟨`maxSynthPendingDepth, .ofNat 3⟩,
  ⟨`pp.unicode.fun, true⟩,
]

package «ship_lean_demo»

@[default_target]
lean_lib «ShipLeanDemo» where
  leanOptions := leanRsLeanOptions
  defaultFacets := #[LeanLib.sharedFacet]
