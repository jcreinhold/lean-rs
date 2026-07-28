import Lake
open System Lake DSL

abbrev leanRsLeanOptions : Array LeanOption := #[
  ⟨`autoImplicit, false⟩,
  ⟨`maxSynthPendingDepth, .ofNat 3⟩,
  ⟨`pp.unicode.fun, true⟩,
]

package «lean_rs_fixture»

@[default_target]
lean_lib «LeanRsFixture» where
  leanOptions := leanRsLeanOptions
  defaultFacets := #[LeanLib.sharedFacet]

-- Kept out of the `LeanRsFixture` roll-up on purpose: its `initialize` block
-- registers an environment extension, which throws if it runs at capability
-- load time rather than inside `Lean.importModules`. See the module docstring.
-- It is still a `default_target` so a plain `lake build` (as CI runs) produces
-- its olean; "out of the roll-up" only means it is not a module of
-- `LeanRsFixture`.
@[default_target]
lean_lib «LeanRsFixtureLateExtension» where
  leanOptions := leanRsLeanOptions
  defaultFacets := #[LeanLib.sharedFacet]
