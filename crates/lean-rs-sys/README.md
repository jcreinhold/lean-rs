# lean-rs-sys

Raw FFI bindings for the Lean 4 C ABI. Sits at the bottom of the `lean-rs` workspace; everything above it (the
[`lean-toolchain`](../lean-toolchain/) build helpers and the [`lean-rs`](../lean-rs/) safe front door) ultimately
threads through this crate.

Lean's header layout is **not** part of this crate's public semver. The semver promise covers the opaque public types,
the `pub unsafe fn` surface, and the `SUPPORTED_TOOLCHAINS` table. The `LeanObjectRepr` layout struct is `pub(crate)`
and may be updated to track Lean version bumps without breaking downstream code that uses the `pub unsafe fn` helpers.

**Calling any function in this crate is `unsafe`.** Public types are opaque (`lean_object` is `[u8; 0]` plus phantom
markers, `!Send + !Sync + !Unpin`); downstream code reaches refcount, tag, and payload state only through
`pub unsafe fn` helpers, each of which carries a `# Safety` section naming the invariant the caller must uphold. Prefer
the safe layers in `lean-rs` for almost every use case; reach for this crate only when the safe surface is missing a
capability you need.

## Supported Lean window

Currently **4.26.0 through 4.29.1**; the authoritative list lives in
[`crates/lean-rs-sys/src/supported.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/src/supported.rs).
The build script computes a SHA-256 digest over the discovered `lean.h` and accepts any digest matching an entry in the
[`SUPPORTED_TOOLCHAINS`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/src/supported.rs) table.
Releases that ship a byte-identical `lean.h` share one entry. A miss fails the build with a bounded diagnostic naming
the discovered digest and the full window.

The build script emits `cargo:rustc-cfg=lean_v_X_Y_Z` for the matched entry's resolved version, so downstream code can
`#[cfg]`-gate per-version divergences. As of v0.1.0 no divergence requires gating: layout structs are byte-identical and
all 87 `REQUIRED_SYMBOLS` entries are present across the entire window.

Bumping the window is the [bump procedure](https://github.com/jcreinhold/lean-rs/blob/main/docs/bump-toolchain.md): add
a row to `SUPPORTED_TOOLCHAINS`, add a CI matrix entry, run the local sweep (`scripts/test-all-toolchains.sh`), open a
PR.

## Build environment

`build.rs` discovers Lean via `lean --print-prefix` (or `LEAN_SYSROOT` when set), emits the appropriate
`cargo:rustc-link-*` directives, and exposes `LEAN_VERSION`, `LEAN_HEADER_PATH`, and `LEAN_HEADER_DIGEST` as build-time
environment variables consumed by `consts.rs`. The default features (`dynamic`, `mimalloc`) link against
`libleanshared`; the `static` feature is available but requires extending the link set beyond what `lean.h` alone
demands. The `metadata-only` feature is for crates such as `lean-toolchain` that need the supported-window metadata from
build scripts without linking the build-script binary to `libleanshared`. See `build.rs` for details.

When `DOCS_RS=1`, the build script emits documentation-only metadata for the latest supported Lean window entry and
deliberately skips Lean discovery and link directives. docs.rs does not install Lean, so published API docs must not
require a local toolchain.

## License

Dual-licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT license](../../LICENSE-MIT),
at your option.
