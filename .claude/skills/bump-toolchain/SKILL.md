---
name: bump-toolchain
description: Add or remove a Lean release in lean-rs's supported toolchain window, including header digests, symbol/layout checks, CI head references, pins, and compatibility verification. Use for Lean toolchain or supported-window bumps.
---

# Bump the Lean toolchain window

Follow [docs/bump-toolchain.md](../../../docs/bump-toolchain.md), including its failure table. That document owns the
procedure and the cross-file checklist; do not maintain a second copy here.

The canonical window is `crates/lean-rs-abi/src/supported.rs`. A new header digest needs layout and symbol verification
before admission. Mirror accepted changes in the digest manifest, version matrix, CI references, and committed pins as
directed by the procedure. Do not paper over a failed check with version-specific wrappers or allowlists.

Run the focused verification required by the procedure and report the toolchain, digest, commands, and results. The full
supported-window sweep runs through CI's `full_matrix` dispatch. Commit the coherent bump with its verification
evidence; do not push unless requested.
