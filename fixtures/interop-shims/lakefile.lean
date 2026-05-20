import Lake
open Lake DSL

package «lean_rs_interop_consumer»

require «lean_rs_interop_shims» from "../../lake/lean-rs-interop-shims"

@[default_target]
lean_lib «LeanRsInteropConsumer» where
  defaultFacets := #[LeanLib.sharedFacet]
