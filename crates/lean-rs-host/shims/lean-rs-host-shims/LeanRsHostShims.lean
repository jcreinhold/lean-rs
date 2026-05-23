import LeanRsHostShims.Environment
import LeanRsHostShims.Elaboration
import LeanRsHostShims.Meta
import LeanRsHostShims.InfoTree
import LeanRsHostShims.Interop

/-! Roll-up module for the `LeanRsHostShims` Lake library. Importing this
    module pulls in all 28 mandatory + 6 optional `@[export]
    lean_rs_host_*` shims that the `lean-rs-host` Rust crate's
    `LeanCapabilities::load_capabilities` resolves, plus the test-only
    interop callback spike export. External consumers of `lean-rs-host`
    import this from their capability `lean_lib` so the shim object files
    get linked into their compiled dylib's shared facet.

    The shim contract is documented in
    `docs/lean-rs-host-capability-contract.md` upstream; the per-symbol
    Lean signatures live in the three submodules
    (`Environment` / `Elaboration` / `Meta`). -/
