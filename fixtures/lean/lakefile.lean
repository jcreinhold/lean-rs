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
lean_lib «LeanRsFixtureLateExtension» where
  leanOptions := leanRsLeanOptions
  defaultFacets := #[LeanLib.sharedFacet]
