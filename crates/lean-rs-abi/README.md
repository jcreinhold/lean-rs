# lean-rs-abi

Link-free Lean 4 ABI and toolchain metadata for the `lean-rs` workspace.

This crate owns the supported Lean toolchain window, required Lean runtime
symbol names, and build-time constants such as `LEAN_VERSION` and
`LEAN_HEADER_DIGEST`. It intentionally has no `links = "leanshared"` key, no
raw `extern "C"` declarations, and no linker directives. Use `lean-rs-sys`
when a crate actually calls the Lean C ABI.
