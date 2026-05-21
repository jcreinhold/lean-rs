import Lake
open System Lake DSL

/-! Generic Lean/Rust interop helpers for `lean-rs`.

    This package contains reusable ABI support shared by downstream Lean
    capabilities and higher-level host shims. It does not define theorem-prover
    host policy, declaration introspection, elaboration, or `MetaM` services.
    Those stay in `lean-rs-host-shims`. Worker streaming helpers in this
    package define callback-envelope mechanics, not downstream row schemas. -/
package «lean_rs_interop_shims»

input_file interop_callback.c where
  path := "c" / "interop_callback.c"
  text := true

target interop_callback.o pkg : FilePath := do
  let srcJob ← interop_callback.c.fetch
  let oFile := pkg.buildDir / "c" / "interop_callback.o"
  buildO oFile srcJob #[] #["-fPIC"] "cc" getLeanTrace

target libleanrsinterop_callback pkg : FilePath := do
  let ffiO ← interop_callback.o.fetch
  let name := nameToStaticLib "leanrsinterop_callback"
  buildStaticLib (pkg.staticLibDir / name) #[ffiO]

@[default_target]
lean_lib «LeanRsInterop» where
  defaultFacets := #[LeanLib.sharedFacet]
  moreLinkObjs := #[libleanrsinterop_callback]
