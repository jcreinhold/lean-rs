import Lake
open System Lake DSL

abbrev leanRsLeanOptions : Array LeanOption := #[
  ⟨`autoImplicit, false⟩,
  ⟨`maxSynthPendingDepth, .ofNat 3⟩,
  ⟨`pp.unicode.fun, true⟩,
]

/-! Development mirror of the bundled host shim package.

    The packaged copy lives under `crates/lean-rs-host/shims/lean-rs-host-shims`
    and is the copy external consumers receive through the Rust crate. Keep this
    mirror in sync when editing host shim sources during workspace development. -/
package «lean_rs_host_shims»

require «lean_rs_interop_shims» from "../lean-rs-interop-shims"

input_file interop_callback.c where
  path := ".." / "lean-rs-interop-shims" / "c" / "interop_callback.c"
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
lean_lib «LeanRsHostShims» where
  leanOptions := leanRsLeanOptions
  defaultFacets := #[LeanLib.sharedFacet]
  moreLinkObjs := #[libleanrsinterop_callback]
