# lean-toolchain

Lean 4 toolchain discovery, fingerprinting, allowlist re-export, and build-script helpers for the `lean-rs`
project. Sits above the in-tree `lean-rs-sys` crate (raw FFI + header digest + symbol allowlist; published per
`RD-2026-05-17-005`) and below [`lean-rs`](../lean-rs/). Owns the typed `ToolchainFingerprint`, the Lake fixture
digest, the
layered link diagnostics, and the reusable build-script helpers downstream embedders can call from their own
`build.rs`. See the [workspace README](../../README.md).
