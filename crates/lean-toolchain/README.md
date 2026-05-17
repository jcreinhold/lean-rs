# lean-toolchain

Lean 4 toolchain discovery, fingerprinting, symbol allowlist, and build-script helpers for the `lean4-rs` project.
Sits above the external [`lean-sys`](https://crates.io/crates/lean-sys) crate and below [`lean-rs`](../lean-rs/). Owns
everything `lean-sys` doesn't (Lean version constant, header digest, typed fingerprint struct, curated allowlist,
link diagnostics, reusable build-script helpers). See the [workspace README](../../README.md).
