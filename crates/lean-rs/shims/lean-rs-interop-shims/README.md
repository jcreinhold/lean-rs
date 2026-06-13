# lean-rs-interop-shims

Package-owned Lean source payload for the generic `lean-rs` interop shims.

Downstream build scripts can call `materialize_source_package` to copy the `LeanRsInterop` Lake package into a
caller-owned build/cache root with a generated `lean-toolchain`, instead of assuming a sibling `lean-rs` checkout.
