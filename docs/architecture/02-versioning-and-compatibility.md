# Versioning and Compatibility

This document records the supported Lean toolchain range, the in-tree raw-FFI policy, the header digest policy, the
workspace crates' semver stance, and the supported platform list. It is the single place to look when deciding whether a
given Lean toolchain or host OS is supported. Bumping any of these is a compatibility decision, not a build fix.

## Supported Lean toolchain range

`lean-rs` is currently developed and tested against Lean **4.29.1** (the active local toolchain at the time of the
charter session). The concrete supported range is pinned in code by prompt 04, which creates the `lean-rs-sys` crate
(published, per `RD-2026-05-17-005`) with a `LEAN_VERSION` const derived from `lean --version`, a SHA-256
`LEAN_HEADER_DIGEST` checked at build time against a hard-coded `EXPECTED_HEADER_DIGEST` (the digest the refcount
mirrors were authored against), and the `REQUIRED_SYMBOLS` allowlist that `tests/linkage.rs` exercises by taking the
address of each symbol at link time. Prompt 05 adds a parallel link-time allowlist test in `lean-toolchain` that imports
via `lean_rs_sys::*` so the same symbol set is verified through the consumer surface.

Policy:

- The supported range is a single contiguous interval of Lean versions, named here and recorded under the
    `VERSION-COMPATIBILITY` contract in `00-current-state.md`. Discontinuous support (e.g. "4.29.1 and 4.31.0 but not
    4.30.0") is not offered.
- Extending the range requires either a new CI matrix entry for the additional version or a documented build flag
    covering it, _before_ the claim is made. Untested versions are not supported even if they happen to compile.
- A change in Lean's C ABI — `lean_object` layout, the ownership convention on `lean_obj_arg` / `b_lean_obj_arg`, the
    initializer protocol — is a contract change, not a build fix. Follow `00-recovery-protocol.md` and stop with a
    Replanning Delta.

## Raw FFI source

Raw `extern "C"` declarations for the curated subset of `lean.h`, the pure-Rust mirrors of `lean.h`'s `static inline`
refcount helpers, and the `REQUIRED_SYMBOLS` allowlist live in the **published** workspace crate `lean-rs-sys`
(`crates/lean-rs-sys/`, per `RD-2026-05-17-005`). Publication matches every peer `*-sys` crate and gives advanced users
a stable raw-FFI escape hatch without forking the workspace. `lean-toolchain` composes a typed fingerprint, fixture
digest, and layered link diagnostics on top of that and re-exports the version metadata plus the symbol allowlist so
callers have one entry point and the allowlist lives in exactly one place.

There is no external `lean-sys` dependency. `RD-2026-05-17` adopted `digama0/lean-sys`; `RD-2026-05-17-003` reverted the
decision so we don't have to manage upstream PRs for every metadata or discovery surface we need. `RD-2026-05-17-005`
then flipped `lean-rs-sys`'s publication status from internal to published, without changing the in-tree authorship. See
`prompts/lean-rs/00-current-state.md` for the full reasoning.

The split between `lean-rs-sys` and `lean-toolchain` is:

- `lean-rs-sys` owns everything that is "what the active Lean header says": the extern declarations split by semantic
    category, the pure-Rust refcount mirrors, the `REQUIRED_SYMBOLS` allowlist, `LEAN_VERSION`, `LEAN_HEADER_PATH`,
    `LEAN_HEADER_DIGEST`, and the `cargo:rustc-link-*` plus `cargo:rerun-if-changed=<lean.h>` directives. Public types
    are opaque (`lean_object` is `[u8; 0] + PhantomData<(*mut u8, PhantomPinned)>`); layout structs are `pub(crate)`.
    Lean's header layout is a `lean-rs-sys`-internal invariant pinned by the digest check.
- `lean-toolchain` owns everything composed on top: the typed `ToolchainFingerprint`, the Lake fixture digest, the
    layered link diagnostics, the reusable build-script helpers for downstream embedders, and the `required_symbols()`
    accessor that returns `lean_rs_sys::REQUIRED_SYMBOLS` directly.

See [`04-raw-sys-design.md`](04-raw-sys-design.md) for the per-decision rationale behind `lean-rs-sys`'s shape.

## Header digest

`lean-rs-sys`'s build script (prompt 04) computes a SHA-256 digest over the discovered `lean.h` and compares it against
`EXPECTED_HEADER_DIGEST` — a constant hard-coded in `lib.rs`, the digest the refcount mirrors were authored against. A
mismatch fails the build with bounded diagnostics naming both digests and the discovered header path. The build-time
`LEAN_HEADER_DIGEST` (the _actually discovered_ digest) is also baked in as a `pub const` for diagnostic display;
`lean-toolchain` (prompt 05) re-exports it.

The digest's two jobs: (1) refuse to compile the Rust refcount mirrors against a `lean.h` whose layout we have not
audited; (2) refuse to silently link a consumer's binary against a different `libleanshared` than the one whose ABI the
published `lean-rs-sys` was authored for. It is not a security boundary.

## Crate semver

The workspace crates start at `0.1.0`. After prompt 05:

- `lean-rs`, `lean-toolchain`, and `lean-rs-sys` follow Cargo's `0.x` semver. Any `0.x` minor bump may break the public
    API; consumers should pin a single minor. `lean-rs-sys`'s public surface is intrinsically `unsafe` (it is the
    curated `extern "C"` view of `lean.h`); its semver promise is about _symbol names and signatures_, not about safe
    behavior. Lean's header layout is _not_ part of `lean-rs-sys`'s semver — the `LeanObjectRepr` struct is `pub(crate)`
    and may be updated to track Lean version bumps without breaking downstream code that uses the `pub unsafe fn`
    helpers.
- `lean-rs-test-support` is `publish = false` and exempt from semver — it is a workspace-internal helper.
- Items inside `lean-rs`'s `pub(crate)` modules (`runtime`, `abi`) and the internal helper modules under `module/` and
    `host/` are _not_ part of the public API. They can be renamed, moved, or collapsed without a minor bump as long as
    the curated re-exports at the crate root keep their shape.

Stabilization to `1.0` requires the `RELEASE-READINESS` contract; it is not implicit and is not requested by this
document.

## Supported platforms

Supported and CI-tested:

- `ubuntu-latest` (x86_64 GNU/Linux).
- `macos-latest` (Apple Silicon).

Rust toolchain: `stable`, pinned by `rust-toolchain.toml` at the repo root.

Windows is an explicit non-goal at this stage. Adding it is itself a compatibility decision: it requires a CI matrix
entry, a documented build flag covering MSVC linking and the `lean-rs-sys` feature selection, and an update to this
file. Other platforms (BSDs, embedded targets, WASM) are not supported.

## Bumping the Lean version: process

1. Open a PR that adds the new version to the CI matrix in `.github/workflows/ci.yml`.
1. Build `lean-rs-sys` against the new Lean header. If the `EXPECTED_HEADER_DIGEST` build-time check fires, audit the
    lean.h diff:
    - If only symbol names or signatures changed, extend `extern "C"` declarations in `lean-rs-sys/src/*.rs` and update
        `EXPECTED_HEADER_DIGEST` and the `REQUIRED_SYMBOLS` list accordingly.
    - If `lean_object` or any subclass layout changed, update `LeanObjectRepr` (and the subclass reprs) in
        `lean-rs-sys/src/repr.rs`, bump `EXPECTED_HEADER_DIGEST`, and re-publish `lean-rs-sys` with a new minor (the
        public opaque types are unchanged, but the bundled mirrors target a different layout).
1. Re-run `lean-rs-sys`'s `tests/linkage.rs` and `lean-toolchain`'s parallel linkage test against the new Lean runtime.
    Every entry in `REQUIRED_SYMBOLS` must resolve.
1. If both linkage tests pass and the smoke tests still pass, edit this file and the `VERSION-COMPATIBILITY` entry in
    `00-current-state.md` to record the new supported range.
1. If either linkage test fails or any ABI assumption breaks, stop. File a Replanning Delta per
    `00-recovery-protocol.md`. Do not patch around the diff with brittle wrappers.
