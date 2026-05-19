import Lake
open System Lake DSL

/-! Lake package shipping the 18 mandatory + 4 optional `lean_rs_host_*`
    `@[export]` Lean shims that the `lean-rs-host` Rust crate's
    `LeanCapabilities::load_capabilities` resolves at runtime.

    External consumers of `lean-rs-host` add this package to their own
    `lakefile.lean` via `require lean_rs_host_shims from git "…" @ "v0.1.0"`
    (or `from "…"` with a local path during development). Their capability
    `lean_lib` then imports `LeanRsHostShims`; Lake's `sharedFacet` links the
    transitively-required shim object files into the consumer's compiled
    dylib so the `@[export] lean_rs_host_*` symbols are visible to the
    host-side `dlsym` resolver. -/
package «lean_rs_host_shims»

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
lean_lib «LeanRsHostShims» where
  defaultFacets := #[LeanLib.sharedFacet]
  moreLinkObjs := #[libleanrsinterop_callback]
