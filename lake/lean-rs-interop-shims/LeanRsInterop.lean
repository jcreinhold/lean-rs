import LeanRsInterop.Callback

/-! Roll-up module for reusable Lean/Rust interop helpers.

    Importing this module makes the generic callback ABI helper available to
    the importing Lake library. Host policy shims and downstream capability
    packages may depend on this package without importing `lean-rs-host-shims`.
 -/
