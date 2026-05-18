# Versioning and Compatibility

This document records the supported Lean toolchain window, the in-tree raw-FFI policy, the header digest policy, the
workspace crates' semver stance, and the supported platform list. It is the single place to look when deciding whether a
given Lean toolchain or host OS is supported. Bumping any of these is a compatibility decision, not a build fix.

## Supported Lean toolchain window

`lean-rs` supports a **contiguous window of Lean 4 stable releases**, enumerated in the
[`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-sys/src/supported.rs) table in `lean-rs-sys`. The table is the single
source of truth; this document mirrors it for narrative context. As of 2026-05-18:

| Lean versions (header-identical) | `lean.h` SHA-256 |
| --- | --- |
| `4.26.0` | `e0ea3efaccceb5b75c7e9e1ab92952c8aa85c3faee28ee949dfeb8ab428ad218` |
| `4.27.0` | `42255d180910bb063d97c87cfb2a61550009ca9ceb6f495069c56bfaa6c92e13` |
| `4.28.0` | `624726e5f1f10fd77cd95b8fe8f30389312e57c8fc98e6c2f1989289bdb5fb0e` |
| `4.28.1` | `648ecfb615ef0222cd63b5f1bbbc379a06749bc0f5f4c2eb16ffca26fd18fe81` |
| `4.29.0` | `671683950ef412474bede2c6a2b50aecf4f99bc29e1ddaf2222ee54ad4ffb91c` |
| `4.29.1` | `2e481a0dac7215eb16123eaef97298ae5a6d0bd0c28c534c2818e2d2f2a28efc` |

Lean does not always bump `lean.h` between point releases; rows with multiple `versions` entries share one digest.
Adding `4.30.0` (and onwards) is the [bump procedure](../bump-toolchain.md) — adding a row to `SUPPORTED_TOOLCHAINS`
and a CI matrix cell.

The lower bound is **4.26.0**, not earlier. A 2026-05-18 multi-toolchain sweep
([`scripts/test-all-toolchains.sh`](../../scripts/test-all-toolchains.sh)) covering 4.23.0 through 4.29.1 showed
that releases ≤ 4.25.x crash inside `lean_dec_ref_cold` from the L2 host stack (`lean-rs-host` session/meta tests
SIGSEGV in the Lean refcount cold path). The 6 releases at 4.26.0 and above all pass clean (242 tests each, 0
failures). Reopening the lower bound is recorded as the follow-up task in `RD-2026-05-18-002`.

Policy:

- The window is contiguous and CI-tested in full. Every named release runs the workspace test suite on both
  `ubuntu-latest` and `macos-latest`; see [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml).
- A consumer may build against any release in the window. The build script accepts any `lean.h` digest in the table;
  the runtime probe at `LeanRuntime::init` re-validates against the same table as a backstop.
- Layout assumptions encoded in [`lean-rs-sys/src/repr.rs`](../../crates/lean-rs-sys/src/repr.rs) are verified to be
  byte-identical across the entire window (the `lean_object` header struct definitions are unchanged across the table
  rows above).
- Two **Lake naming conventions** coexist within the window: Lean ≤ 4.26 emits
  `lib{LibName}.{dylib,so}` and `initialize_{MangledModule}`; Lean ≥ 4.27 emits
  `lib{escaped_package}_{LibName}.{dylib,so}` and `initialize_{MangledPackage}_{MangledModule}`. The
  [dylib loader](../../crates/lean-rs/src/module/library.rs) and the
  [Lake-project discovery](../../crates/lean-rs-host/src/host/lake.rs) probe both shapes so the Rust code is
  convention-agnostic.
- A change in Lean's C ABI — `lean_object` layout, the ownership convention on `lean_obj_arg` / `b_lean_obj_arg`, the
  initializer protocol — at any point in the window is a contract change, not a build fix. Follow
  [`00-recovery-protocol.md`](../../../prompts/lean-rs/00-recovery-protocol.md) and stop with a Replanning Delta.

## Raw FFI source

Raw `extern "C"` declarations for the curated subset of `lean.h`, the pure-Rust mirrors of `lean.h`'s `static inline`
refcount helpers, and the `REQUIRED_SYMBOLS` allowlist live in the **published** workspace crate `lean-rs-sys`
(`crates/lean-rs-sys/`, per `RD-2026-05-17-005`). Publication matches every peer `*-sys` crate and gives advanced users
a stable raw-FFI escape hatch without forking the workspace. `lean-toolchain` composes a typed fingerprint, fixture
digest, and layered link diagnostics on top of that and re-exports the version metadata plus the symbol allowlist so
callers have one entry point and the allowlist lives in exactly one place.

There is no external `lean-sys` dependency. `RD-2026-05-17` adopted `digama0/lean-sys`; `RD-2026-05-17-003` reverted
the decision so we don't have to manage upstream PRs for every metadata or discovery surface we need.
`RD-2026-05-17-005` then flipped `lean-rs-sys`'s publication status from internal to published, without changing the
in-tree authorship. See `prompts/lean-rs/00-current-state.md` for the full reasoning.

The split between `lean-rs-sys` and `lean-toolchain` is:

- `lean-rs-sys` owns everything that is "what the active Lean header says": the extern declarations split by semantic
  category, the pure-Rust refcount mirrors, the `REQUIRED_SYMBOLS` allowlist, the `SUPPORTED_TOOLCHAINS` window table,
  `LEAN_VERSION`, `LEAN_RESOLVED_VERSION`, `LEAN_HEADER_PATH`, `LEAN_HEADER_DIGEST`, and the `cargo:rustc-link-*` plus
  `cargo:rerun-if-changed=<lean.h>` directives. Public types are opaque (`lean_object` is `[u8; 0]` + phantom data);
  layout structs are `pub(crate)`. Lean's header layout is a `lean-rs-sys`-internal invariant pinned by the digest
  table.
- `lean-toolchain` owns everything composed on top: the typed `ToolchainFingerprint` (which exposes
  `is_supported()` against the window), the Lake fixture digest, the layered link diagnostics, the reusable
  build-script helpers for downstream embedders, and the `required_symbols()` accessor that returns
  `lean_rs_sys::REQUIRED_SYMBOLS` directly.

See [`05-raw-sys-design.md`](05-raw-sys-design.md) for the per-decision rationale behind `lean-rs-sys`'s shape.

## Header digest

`lean-rs-sys`'s build script computes a SHA-256 digest over the discovered `lean.h` and looks it up in
[`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-sys/src/supported.rs). A miss fails the build with a bounded diagnostic
naming the discovered digest and the full window; a hit emits `cargo:rustc-cfg=lean_v_X_Y_Z` (with dots converted to
underscores) so per-version divergences can be `#[cfg]`-gated, and bakes the resolved version into
`LEAN_RESOLVED_VERSION` for runtime inspection.

The digest's two jobs: (1) refuse to compile the Rust refcount mirrors against a `lean.h` whose layout has not been
audited; (2) refuse to silently link a consumer's binary against a different `libleanshared` than the one whose ABI
the published `lean-rs-sys` was authored for. It is not a security boundary.

## Crate semver

The workspace crates start at `0.1.0`. After prompt 05:

- `lean-rs`, `lean-toolchain`, and `lean-rs-sys` follow Cargo's `0.x` semver. Any `0.x` minor bump may break the public
  API; consumers should pin a single minor. `lean-rs-sys`'s public surface is intrinsically `unsafe` (it is the
  curated `extern "C"` view of `lean.h`); its semver promise is about *symbol names and signatures* and the
  `SUPPORTED_TOOLCHAINS` window, *not* about safe behavior. Lean's header layout is *not* part of `lean-rs-sys`'s
  semver — the `LeanObjectRepr` struct is `pub(crate)` and may be updated to track Lean version bumps without breaking
  downstream code that uses the `pub unsafe fn` helpers.
- `lean-rs-host` follows the same `0.x` policy; its surface depends on `lean-rs-sys`'s window, and on the
  [`lean-rs-host-shims`](../../lake/lean-rs-host-shims/) Lake package that ships the `@[export]` shim contract.
- Items inside `lean-rs`'s `pub(crate)` modules (`runtime`, `abi`) and the internal helper modules under `module/`
  and `host/` are *not* part of the public API. They can be renamed, moved, or collapsed without a minor bump as long
  as the curated re-exports at the crate root keep their shape.

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

The canonical procedure lives in [`docs/bump-toolchain.md`](../bump-toolchain.md). In summary:

1. `elan toolchain install leanprover/lean4:vX.Y.Z`.
2. Capture the new `lean.h` SHA-256 (the bump-toolchain doc has a one-liner).
3. Add a row (or extend an existing row, when header-identical with an existing entry) to
   [`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-sys/src/supported.rs).
4. Add the version to the matrix in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml).
5. Run `scripts/test-all-toolchains.sh` locally; commit; open PR.

If any ABI assumption breaks at the new release — `lean_object` layout shifts, an entry in `REQUIRED_SYMBOLS`
disappears, the Lake naming convention changes again — stop. File a Replanning Delta per
[`00-recovery-protocol.md`](../../../prompts/lean-rs/00-recovery-protocol.md). Do not patch around the diff with
brittle wrappers.
