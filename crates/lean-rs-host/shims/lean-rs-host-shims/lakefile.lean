import Lake
open System Lake DSL

/-! Bundled Lake package shipping the 28 mandatory + 6 optional
    `lean_rs_host_*` `@[export]` Lean shims that the `lean-rs-host` Rust
    crate loads at runtime.

    Consumers do not require this package from their own lakefile. The Rust
    host crate builds this crate-owned package on demand, opens the produced
    dylib globally, and adds the produced `.olean` directory to session import
    search paths. -/
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
  defaultFacets := #[LeanLib.sharedFacet]
  moreLinkObjs := #[libleanrsinterop_callback]
