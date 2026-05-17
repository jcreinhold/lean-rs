# Versioning and Compatibility

This document records the supported Lean toolchain range, the in-tree raw-FFI
policy, the header digest policy, the workspace crates' semver stance, and the
supported platform list. It is the single place to look when deciding whether a
given Lean toolchain or host OS is supported. Bumping any of these is a
compatibility decision, not a build fix.

## Supported Lean toolchain range

`lean-rs` is currently developed and tested against Lean **4.29.1** (the active
local toolchain at the time of the charter session). The concrete supported range
is pinned in code by prompt 04, which creates the in-tree `lean-rs-sys` crate
with a `LEAN_VERSION` const derived from `lean --version`, a SHA-256 header
digest, and a signature-checked symbol allowlist that fails the build if any
required `lean_*` declaration is missing or has a mismatched signature. Prompt
05 adds a link-time variant of the allowlist in `lean-toolchain` that takes the
address of each required `lean_rs_sys::lean_*` item so the linker resolves it
under the chosen linking strategy.

Policy:

- The supported range is a single contiguous interval of Lean versions, named
  here and recorded under the `VERSION-COMPATIBILITY` contract in
  `00-current-state.md`. Discontinuous support (e.g. "4.29.1 and 4.31.0 but not
  4.30.0") is not offered.
- Extending the range requires either a new CI matrix entry for the additional
  version or a documented build flag covering it, *before* the claim is made.
  Untested versions are not supported even if they happen to compile.
- A change in Lean's C ABI — `lean_object` layout, the ownership convention on
  `lean_obj_arg` / `b_lean_obj_arg`, the initializer protocol — is a contract
  change, not a build fix. Follow `00-recovery-protocol.md` and stop with a
  Replanning Delta.

## Raw FFI source

Raw `extern "C"` declarations for the curated subset of `lean.h`, the
hand-written refcount inline helpers, and the signature-checked symbol
allowlist live in the in-tree workspace crate `lean-rs-sys`
(`crates/lean-rs-sys/`, `publish = false`). `lean-toolchain` composes a typed
fingerprint, fixture digest, and layered link diagnostics on top of that and
re-exports the version metadata so callers have one entry point.

There is no external `lean-sys` dependency. `RD-2026-05-17` adopted
`digama0/lean-sys`; `RD-2026-05-17-003` reverted the decision so we don't have
to manage upstream PRs for every metadata or discovery surface we need. See
`prompts/lean-rs/00-current-state.md` for the full reasoning.

The split between `lean-rs-sys` and `lean-toolchain` is:

- `lean-rs-sys` owns everything that is "what the active Lean header says":
  the extern declarations, the curated symbol allowlist with signature checks,
  `LEAN_VERSION`, `LEAN_HEADER_PATH`, `LEAN_HEADER_DIGEST`, and the
  `cargo:rustc-link-*` plus `cargo:rerun-if-changed=<lean.h>` directives.
- `lean-toolchain` owns everything composed on top: the typed
  `ToolchainFingerprint`, the Lake fixture digest, the layered link
  diagnostics, the reusable build-script helpers for downstream embedders.

## Header digest

`lean-rs-sys`'s build script (prompt 04) computes a SHA-256 digest over the
discovered `lean.h` and bakes the result into `LEAN_HEADER_DIGEST`;
`lean-toolchain` (prompt 05) re-exports it. A mismatch between the digest a
published `lean-toolchain` was built against and the digest discovered in a
consumer's environment is a build-time error, not a runtime warning — the
consumer must either align their Lean toolchain or pick a `lean-toolchain`
release built against their version.

The digest's only job is to refuse to silently link against a different `lean.h`
than the one whose ABI we audited. It is not a security boundary.

## Crate semver

The workspace crates start at `0.1.0`. After prompt 05:

- `lean-rs` and `lean-toolchain` follow Cargo's `0.x` semver. Any `0.x` minor
  bump may break the public API; consumers should pin a single minor.
- `lean-rs-sys` and `lean-rs-test-support` are `publish = false` and exempt
  from semver — they are workspace-internal helpers.
- Items inside `lean-rs`'s `pub(crate)` modules (`runtime`, `abi`, `module`,
  `host`, `batch`, `error`) are *not* part of the public API. They can be
  renamed, moved, or collapsed without a minor bump as long as the curated
  re-exports at the crate root keep their shape.

Stabilization to `1.0` requires the `RELEASE-READINESS` contract; it is not
implicit and is not requested by this document.

## Supported platforms

Supported and CI-tested:

- `ubuntu-latest` (x86_64 GNU/Linux).
- `macos-latest` (Apple Silicon).

Rust toolchain: `stable`, pinned by `rust-toolchain.toml` at the repo root.

Windows is an explicit non-goal at this stage. Adding it is itself a
compatibility decision: it requires a CI matrix entry, a documented build flag
covering MSVC linking and the `lean-rs-sys` feature selection, and an update
to this file. Other platforms (BSDs, embedded targets, WASM) are not
supported.

## Bumping the Lean version: process

1. Open a PR that adds the new version to the CI matrix in `.github/workflows/ci.yml`.
2. Re-run `lean-rs-sys`'s signature-checked allowlist (prompt 04) and
   `lean-toolchain`'s link-time allowlist (prompt 05) against the new Lean
   header. Extend the extern declarations in `lean-rs-sys` if a symbol's name
   or signature has shifted.
3. If both allowlist tests pass and the `lean-toolchain` smoke tests still
   pass, edit this file and the `VERSION-COMPATIBILITY` entry in
   `00-current-state.md` to record the new supported range.
4. If either allowlist test fails or any ABI assumption breaks, stop. File a
   Replanning Delta per `00-recovery-protocol.md`. Do not patch around the
   diff with brittle wrappers.
