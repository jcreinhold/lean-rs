# lean-rs

Safe Rust bindings for hosting Lean 4 capabilities. The single safe front door of the `lean-rs` project: runtime
initialization, owned and borrowed object handles, typed first-order ABI conversions, module loading and exported
functions, semantic handles, bounded meta services, batching, and session pooling. Built on top of the in-tree
`lean-rs-sys` crate (raw FFI, published per `RD-2026-05-17-005`) and the workspace's
[`lean-toolchain`](../lean-toolchain/) crate.
See the [workspace README](../../README.md).
