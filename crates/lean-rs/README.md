# lean-rs

Safe Rust bindings to the Lean 4 runtime: runtime start-up and thread attachment, owned and borrowed object handles,
typed conversion of first-order values, module loading and exported functions, handles for names, levels, expressions
and declarations, Lean-to-Rust callbacks, and one error type with stable diagnostic codes. It ships no Lean shims.
[`lean-rs-host`](https://docs.rs/lean-rs-host) adds sessions, elaboration, kernel checks and `MetaM` on top, and
[`lean-rs-sys`](https://docs.rs/lean-rs-sys) holds the raw bindings beneath.

Binding a generic export and calling it needs `unsafe`, since only the caller knows what the native code does. A custom
`LeanAbi` implementation must keep Lean's representation, ownership and runtime rules in both directions. Callbacks stay
in one process.

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
