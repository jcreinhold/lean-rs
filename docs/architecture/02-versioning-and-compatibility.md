# Versioning and Compatibility

This document records the supported Lean toolchain range, the pinned `lean-sys`
range, the header digest policy, the workspace crates' semver stance, and the
supported platform list. It is the single place to look when deciding whether a
given Lean toolchain, `lean-sys` release, or host OS is supported. Bumping any of
these is a compatibility decision, not a build fix.

## Supported Lean toolchain range

`lean-rs` is currently developed and tested against Lean **4.29.1** (the active
local toolchain at the time of the charter session). The concrete supported range
is pinned in code by prompt 04, which fills `lean-toolchain` with a `LEAN_VERSION`
const derived from `lean --version` and a symbol-allowlist link-time test that
asserts the curated set of `lean_*` symbols resolves under the chosen linking
strategy.

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

## Pinned `lean-sys` version range

`lean-sys >= 0.0.9, < 0.1`. This is the currently published `0.0.x` series; any
bump to `0.1` or above is a contract change because `lean-sys` itself follows
Cargo `0.x` semver (minor bumps may break API).

The split between this project and the upstream crate is:

- Generic improvements (a `LEAN_VERSION` const, `cargo:rerun-if-changed=lean.h`,
  an exposed prefix accessor, etc.) are upstreamed to `digama0/lean-sys` first
  and only relied on locally once they land. We do not maintain parallel copies
  in `lean-toolchain`.
- Project-specific surfaces — the typed `ToolchainFingerprint`, the curated
  `lean_*` symbol allowlist, the Lake fixture digest, the layered link
  diagnostics, the build-script helpers reusable from a downstream embedder's
  own `build.rs` — stay in `lean-toolchain`.

If `lean-sys` stalls or diverges from what we need, the recovery option is a
fork, recorded as a Replanning Delta. We do not silently re-implement the raw
FFI surface in `lean-toolchain`.

## Header digest

`lean-toolchain`'s build script (prompt 04) computes a SHA-256 digest over the
discovered `lean.h` and bakes the result into `LEAN_HEADER_DIGEST`. A mismatch
between the digest a published `lean-toolchain` was built against and the digest
discovered in a consumer's environment is a build-time error, not a runtime
warning — the consumer must either align their Lean toolchain or pick a
`lean-toolchain` release built against their version.

The digest's only job is to refuse to silently link against a different `lean.h`
than the one whose ABI we audited. It is not a security boundary.

## Crate semver

The workspace crates start at `0.0.0`. After prompt 04:

- `lean-rs` and `lean-toolchain` follow Cargo's `0.x` semver. Any `0.x` minor
  bump may break the public API; consumers should pin a single minor.
- `lean-rs-test-support` is `publish = false` and exempt from semver — it is a
  workspace-internal helper.
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
covering MSVC linking and `lean-sys` features, and an update to this file. Other
platforms (BSDs, embedded targets, WASM) are not supported.

## Bumping the Lean version: process

1. Open a PR that adds the new version to the CI matrix in `.github/workflows/ci.yml`.
2. Update `LEAN_VERSION` and re-run the symbol-allowlist link test (prompt 04
   wires this up).
3. If the link test passes and the `lean-toolchain` smoke tests still pass, edit
   this file and the `VERSION-COMPATIBILITY` entry in `00-current-state.md` to
   record the new supported range.
4. If the link test fails or any ABI assumption breaks, stop. File a Replanning
   Delta per `00-recovery-protocol.md`. Do not patch around the diff with
   brittle wrappers.
