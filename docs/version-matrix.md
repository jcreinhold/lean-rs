# Version Matrix

Tested and supported configuration matrix for the three published `lean-rs` crates as of
**v0.1.0** (2026-05-18). The policy source is
[`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md);
release procedure is in [`docs/release.md`](release.md).

**Tested support is not aspirational support.** The tables below list configurations that have
actually been built and verified locally and/or in CI. Configurations outside these tables are
unsupported, even if they happen to compile.

## Lean toolchain

| Lean version | Triple                 | `lean --version`                                                                                       | `EXPECTED_HEADER_DIGEST` (SHA-256)                                 |
| ------------ | ---------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------ |
| 4.29.1       | `arm64-apple-darwin24` | `Lean (version 4.29.1, arm64-apple-darwin24.6.0, commit f72c35b3f637c8c6571d353742168ab66cc22c00, Release)` | `2e481a0dac7215eb16123eaef97298ae5a6d0bd0c28c534c2818e2d2f2a28efc` |

`leanprover/lean4:stable` resolves to v4.29.1 at release time. The single-contiguous-interval
policy collapses to one point release. Extending the range requires either a new CI matrix entry
for the additional version or a documented build flag covering it, **before** the claim is made;
see [`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md)
for the bump procedure.

The captured baseline build context is preserved in
[`docs/performance/baseline.md`](performance/baseline.md) (commit, allocator, fixture digest).

## `lean-rs-sys` symbol coverage

The `REQUIRED_SYMBOLS` allowlist in
[`crates/lean-rs-sys/src/lib.rs`](../crates/lean-rs-sys/src/lib.rs) is the authoritative list of
`LEAN_EXPORT`'d symbols the crate's `extern "C"` blocks declare. It is not duplicated here.

| Crate          | Allowlist size | Source                                                          |
| -------------- | -------------- | --------------------------------------------------------------- |
| `lean-rs-sys`  | 87 symbols     | `pub const REQUIRED_SYMBOLS` in `crates/lean-rs-sys/src/lib.rs` |

`tests/linkage.rs` in `lean-rs-sys` resolves every entry against `libleanshared` at link time. The
parallel test in `lean-toolchain` imports the same set via `lean_rs_sys::REQUIRED_SYMBOLS`,
confirming the consumer surface stays aligned.

## Rust

| Field   | Value                                                                                 |
| ------- | ------------------------------------------------------------------------------------- |
| MSRV    | `1.91` (from `[workspace.package].rust-version`)                                      |
| Channel | `stable` (pinned by [`rust-toolchain.toml`](../rust-toolchain.toml))                  |

Captured rustc at release time: `rustc 1.95.0 (59807616e 2026-04-14)`. The MSRV is the floor a
downstream consumer can rely on; the CI release matrix runs on the current stable.

## Platforms

| Platform                              | Triple                | Status        |
| ------------------------------------- | --------------------- | ------------- |
| Ubuntu Latest (GitHub Actions runner) | `x86_64-linux-gnu`    | supported, CI |
| macOS Latest (GitHub Actions runner)  | `aarch64-apple-darwin`| supported, CI |

Explicitly unsupported (do not file as bugs without a compatibility-decision proposal):

- Windows (any toolchain). Adding support requires a CI matrix entry, a documented build flag for
  MSVC linking and the `lean-rs-sys` feature selection, and an update to
  [`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md).
- BSDs, embedded targets, WASM.
- Lean toolchain versions outside the pinned point release in the Lean toolchain table above.

## Cross-references

- Policy: [`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md)
- Release procedure: [`docs/release.md`](release.md)
- Public-API baselines: [`docs/api-review/`](api-review/) and [`docs/api-review.md`](api-review.md)
- Perf baseline context: [`docs/performance/baseline.md`](performance/baseline.md)
