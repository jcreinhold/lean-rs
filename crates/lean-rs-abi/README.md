# lean-rs-abi

The Lean 4 ABI facts that the `lean-rs` crates share: the supported toolchain window, the runtime symbols lean-rs
requires, and `lean.h` layout constants such as object tags and allocator limits. The crate has no build script,
`extern` declarations or linker directives and probes no installed toolchain, so it builds without Lean.
[`lean-toolchain`](https://docs.rs/lean-toolchain) reports the live toolchain's identity, and
[`lean-rs-sys`](https://docs.rs/lean-rs-sys) calls the C ABI.

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
