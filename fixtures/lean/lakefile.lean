import Lake
open System Lake DSL

package «lean_rs_fixture»

@[default_target]
lean_lib «LeanRsFixture» where
  defaultFacets := #[LeanLib.sharedFacet]
