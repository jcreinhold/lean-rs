import LeanRsInterop.Callback
import LeanRsInterop.Worker.Stream

/-! Roll-up module for reusable Lean/Rust interop helpers.

    Importing this module makes the generic callback ABI helpers available to
    the importing Lake library. Worker streaming helpers hide callback-envelope
    mechanics for downstream capability packages, but do not define row schemas
    or command policy. Host policy shims and downstream capability packages may
    depend on this package without importing `lean-rs-host-shims`.
 -/
