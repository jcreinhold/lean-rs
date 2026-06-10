# lean-rs-abi

Link-free Lean 4 ABI and toolchain metadata for the `lean-rs` workspace.

This crate owns the supported Lean toolchain window, the required Lean runtime
symbol names, and the static `lean.h` layout constants (object tags, allocator
ceilings). It is purely static: no build script, no `links = "leanshared"` key,
no raw `extern "C"` declarations, no linker directives, and no probe of an
installed toolchain — so it builds with no Lean present. Live toolchain identity
(`LEAN_VERSION`, `LEAN_HEADER_PATH`, `LEAN_HEADER_DIGEST`,
`LEAN_RESOLVED_VERSION`) lives in `lean-toolchain`; use `lean-rs-sys` when a
crate actually calls the Lean C ABI.
