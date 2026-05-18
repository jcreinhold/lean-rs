import Lake
open System Lake DSL

package «lean_rs_fixture»

require «lean_rs_host_shims» from "../../lake/lean-rs-host-shims"

@[default_target]
lean_lib «LeanRsFixture» where
  defaultFacets := #[LeanLib.sharedFacet]
