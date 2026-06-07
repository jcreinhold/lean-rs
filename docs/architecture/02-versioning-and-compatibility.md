# Versioning and Compatibility

The supported Lean toolchain window, the in-tree raw-FFI policy, the header-digest policy, crate semver, and supported
platforms. The single place to look when deciding whether a given Lean toolchain or host OS is supported. Each entry is
a compatibility commitment; bumping any of them requires a versioned proposal, not a build fix.

## Supported Lean toolchain window

`lean-rs` supports a **contiguous window of Lean 4 stable releases**, plus the leading release candidate while it is
being qualified, enumerated in the [`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-abi/src/supported.rs) table. The table
is the single source of truth; this document mirrors it for narrative context. As of 2026-05-30:

| Lean versions (header-identical) | `lean.h` SHA-256 (prefix) |
| --- | --- |
| 4.26.0 | `e0ea3efaccce…` |
| 4.27.0 | `42255d180910…` |
| 4.28.0 | `624726e5f1f1…` |
| 4.28.1 | `648ecfb615ef…` |
| 4.29.0 | `671683950ef4…` |
| 4.29.1 | `2e481a0dac72…` |
| 4.30.0 | `5a25125970f4…` |
| 4.31.0-rc1 | `99ef35d69709…` |

Digests are shown as 12-character prefixes; the full SHA-256 for each row lives in
[`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-abi/src/supported.rs), which the build script hash-checks against.

Lean does not always bump `lean.h` between point releases; rows that share a header share a digest. Extending the window
is the [bump procedure](../bump-toolchain.md).

**Lower bound: 4.26.0.** A 2026-05-18 multi-toolchain sweep
([`scripts/test-all-toolchains.sh`](../../scripts/test-all-toolchains.sh)) covered 4.23.0 through 4.29.1. The six
releases from 4.26.0 onwards pass clean (242 tests each, 0 failures); releases ≤ 4.25.x SIGSEGV inside
`lean_dec_ref_cold` from service-layer tests (`lean-rs-host` session/meta). The 4.30.0 row replaced the 4.30.0-rc2 row
on 2026-05-26 after the standard layout-probe + symbol-probe gate passed against the final release. The 4.31.0-rc1 row
was added on 2026-05-30 after the same layout-probe + symbol-probe gate passed against it (`lean.h` byte-identical in
the relevant block to 4.30.0; all 87 `REQUIRED_SYMBOLS` resolve); it will be swapped for the 4.31.0 row once that ships.

**Policy.**

- The window is contiguous and CI-tested in full. Every named release runs the workspace suite on `ubuntu-latest` and
  `macos-latest`; see [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml).
- A consumer may build against any release in the window. The build script accepts any `lean.h` digest in the table;
  `LeanRuntime::init` re-validates against the same table as a backstop.
- Layout assumptions in [`lean-rs-sys/src/repr.rs`](../../crates/lean-rs-sys/src/repr.rs) are verified byte-identical
  across the entire window.
- A change in Lean's C ABI—`lean_object` layout, ownership convention on `lean_obj_arg` / `b_lean_obj_arg`, the
  initializer protocol—at any point in the window is a contract change. Stop; do not paper over the ABI change with
  brittle wrappers.

**Lake naming conventions.** Two coexist in the window. The [dylib loader](../../crates/lean-rs/src/module/library.rs)
and the [Lake-project discovery](../../crates/lean-rs-host/src/host/lake.rs) probe both so Rust code is
convention-agnostic.

| Lean range | Dylib filename | Initializer symbol |
| --- | --- | --- |
| ≤ 4.26 | `lib{LibName}.{dylib,so}` | `initialize_{MangledModule}` |
| ≥ 4.27 | `lib{escaped_package}_{LibName}.{dylib,so}` | `initialize_{MangledPackage}_{MangledModule}` |

## Raw FFI source

Raw `extern "C"` declarations for the curated subset of `lean.h` and the pure-Rust mirrors of `lean.h`'s `static inline`
refcount helpers live in the published workspace crate `lean-rs-sys` (`crates/lean-rs-sys/`). Link-free ABI metadata,
including the `REQUIRED_SYMBOLS` allowlist and supported-window table, lives in `lean-rs-abi`
(`crates/lean-rs-abi/`). Publication matches every peer `*-sys` crate and gives advanced users a stable raw-FFI escape
hatch without forcing metadata-only consumers to link Lean.

There is no external `lean-sys` dependency. The split between `lean-rs-abi`, `lean-rs-sys`, and `lean-toolchain`:

- **`lean-rs-abi`** owns link-free ABI/toolchain metadata: the `REQUIRED_SYMBOLS` allowlist,
  `SUPPORTED_TOOLCHAINS` window table, `LEAN_VERSION`, `LEAN_RESOLVED_VERSION`, `LEAN_HEADER_PATH`, and
  `LEAN_HEADER_DIGEST`.
- **`lean-rs-sys`** owns runtime FFI: extern declarations split by category, the pure-Rust refcount mirrors, opaque
  public types, crate-private layout structs, and the `cargo:rustc-link-*` directives for dynamic/static Lean runtime
  linking. It re-exports `lean-rs-abi` metadata for compatibility.
- **`lean-toolchain`** owns everything composed on top: the typed `ToolchainFingerprint` (which exposes
  `is_supported()`), the Lake fixture digest, layered link diagnostics, reusable build-script helpers, and
  `required_symbols()` returning `lean_rs_abi::REQUIRED_SYMBOLS` so the allowlist lives in one place.

See [`05-raw-sys-design.md`](05-raw-sys-design.md) for the rationale behind `lean-rs-sys`.

## Header digest

`lean-rs-abi`'s build script computes a SHA-256 over the discovered `lean.h` and looks it up in
[`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-abi/src/supported.rs). A miss fails the build with a bounded diagnostic
naming the discovered digest and the full window; a hit emits `cargo:rustc-cfg=lean_v_X_Y_Z` (dots → underscores) so
per-version divergences can be `#[cfg]`-gated, and bakes the resolved version into `LEAN_RESOLVED_VERSION` for runtime
inspection.

The digest's two jobs: (1) refuse to compile the Rust refcount mirrors against a `lean.h` whose layout has not been
audited; (2) refuse to silently link a consumer's binary against a different `libleanshared` than the one whose ABI the
published `lean-rs-sys` was authored for. It is not a security boundary.

## Crate semver

The workspace crates start at `0.1.0` and follow Cargo's `0.x` semver: any minor bump may break the public API;
consumers should pin a single minor.

**`lean-rs-sys`.** The public surface is intrinsically `unsafe`—the curated `extern "C"` view of `lean.h`. The semver
promise is about *symbol names and signatures* and the `SUPPORTED_TOOLCHAINS` window, **not** about safe behaviour.
Lean's header layout is **not** part of this surface—`LeanObjectRepr` is `pub(crate)` and may be updated to track Lean
version bumps without breaking downstream code that uses the `pub unsafe fn` helpers.

**`lean-toolchain`, `lean-rs`, `lean-rs-host`, the worker crates.** Standard `0.x` semver over the curated re-exports at
each crate root. Items inside `lean-rs`'s `pub(crate)` modules (`runtime`, `abi`) and the internal helper modules under
`module/` and `host/` are **not** part of the public API; they can be renamed, moved, or collapsed without a minor bump
as long as the curated re-exports keep their shape. `lean-rs-host` also depends on its bundled host shim package's
`@[export]` contract. The worker crates's semver surface is its supervisor, capability-builder, typed-command, row,
diagnostic, timeout, and restart-policy API; private protocol frame shapes are not public API.

**Lean shim packages.** Same toolchain window. `lean-rs` bundles `lean-rs-interop-shims` for generic callback ABI
helpers. `lean-rs-host` bundles `lean-rs-host-shims` plus the generic helper it needs for host progress.

Stabilization to `1.0` requires the `RELEASE-READINESS` contract and is not implicit.

## Supported platforms

Supported and CI-tested:

- `ubuntu-latest` (x86_64 GNU/Linux).
- `macos-latest` (Apple Silicon).

Rust toolchain: `stable`, pinned by `rust-toolchain.toml` at the repo root.

Windows is an explicit non-goal at this stage. Adding it is itself a compatibility decision: a CI matrix entry, a
documented build flag covering MSVC linking and the `lean-rs-sys` feature selection, and an update to this file. Other
platforms (BSDs, embedded, WASM) are not supported.

## Bumping the Lean version: process

The canonical procedure lives in [`docs/bump-toolchain.md`](../bump-toolchain.md). In summary:

1. `elan toolchain install leanprover/lean4:vX.Y.Z`.
2. Capture the new `lean.h` SHA-256 (the bump-toolchain doc has a one-liner).
3. Add a row to [`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-abi/src/supported.rs) (or extend an existing
   header-identical row).
4. Add the version to the matrix in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml).
5. Run `scripts/test-all-toolchains.sh` locally; commit; open PR.

If any ABI assumption breaks at the new release—`lean_object` layout shifts, a `REQUIRED_SYMBOLS` entry disappears, the
Lake naming convention changes again—stop. File a Stop and discuss before patching around the diff with brittle
wrappers.
