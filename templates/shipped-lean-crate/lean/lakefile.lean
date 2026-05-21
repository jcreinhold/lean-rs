import Lake
open Lake DSL

package «ship_lean_demo»

@[default_target]
lean_lib «ShipLeanDemo» where
  defaultFacets := #[LeanLib.sharedFacet]
