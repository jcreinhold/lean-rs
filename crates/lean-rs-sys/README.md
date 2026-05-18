# lean-rs-sys

Raw FFI bindings for the Lean 4 C ABI. Sits at the bottom of the `lean-rs` workspace; everything above it (the
[`lean-toolchain`](../lean-toolchain/) build helpers and the [`lean-rs`](../lean-rs/) safe front door) ultimately
threads through this crate.

**Calling any function in this crate is `unsafe`.** Public types are opaque (`lean_object` is `[u8; 0]` plus phantom
markers, `!Send + !Sync + !Unpin`); downstream code reaches refcount, tag, and payload state only through
`pub unsafe fn` helpers, each of which carries a `# Safety` section naming the invariant the caller must uphold. Prefer
the safe layers in `lean-rs` for almost every use case; reach for this crate only when the safe surface is missing a
capability you need.

## Supported Lean range

The supported Lean toolchain range is pinned in code and recorded in the workspace's
[`docs/architecture/02-versioning-and-compatibility.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/02-versioning-and-compatibility.md)
and [`docs/version-matrix.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/version-matrix.md). The build
script computes a SHA-256 digest over the discovered `lean.h` and compares it against `EXPECTED_HEADER_DIGEST`; a
mismatch fails the build with bounded diagnostics naming both digests and the discovered header path. Extending the
range means updating `EXPECTED_HEADER_DIGEST`, the `REQUIRED_SYMBOLS` allowlist, and (if layout shifted) the
crate-private `LeanObjectRepr`, then re-running the linkage tests.

Lean's header layout is **not** part of this crate's public semver. The opaque types and `pub unsafe fn` surface are.

## Build environment

`build.rs` discovers Lean via `lean --print-prefix` (or `LEAN_PREFIX` when set), emits the appropriate
`cargo:rustc-link-*` directives, and exposes `LEAN_VERSION`, `LEAN_HEADER_PATH`, and `LEAN_HEADER_DIGEST` as build-time
environment variables consumed by `consts.rs`. The default features (`dynamic`, `mimalloc`) link against
`libleanshared`; the `static` feature is available but requires extending the link set beyond what `lean.h` alone
demands. See `build.rs` for the details.

## License

Dual-licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT), at your option.
