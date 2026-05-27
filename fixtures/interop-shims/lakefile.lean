import Lake
open Lake DSL

abbrev leanRsLeanOptions : Array LeanOption := #[
  ⟨`autoImplicit, false⟩,
  ⟨`maxSynthPendingDepth, .ofNat 3⟩,
  ⟨`pp.unicode.fun, true⟩,
]

package «lean_rs_interop_consumer»

require «lean_rs_interop_shims» from "../../crates/lean-rs/shims/lean-rs-interop-shims"

@[default_target]
lean_lib «LeanRsInteropConsumer» where
  leanOptions := leanRsLeanOptions
  defaultFacets := #[LeanLib.sharedFacet]
