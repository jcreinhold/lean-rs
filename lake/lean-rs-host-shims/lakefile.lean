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

@[default_target]
lean_lib «LeanRsHostShims» where
  defaultFacets := #[LeanLib.sharedFacet]
