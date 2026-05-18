# Version Matrix

Tested and supported configuration matrix for the four published `lean-rs` crates as of
**v0.1.0** (2026-05-18). The policy source is
[`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md);
release procedure is in [`docs/release.md`](release.md). The split into four crates is
recorded under `RD-2026-05-18-001` in
[`prompts/lean-rs/00-current-state.md`](../../prompts/lean-rs/00-current-state.md). The
multi-version support story is `RD-2026-05-18-002`.

**Tested support is not aspirational support.** The tables below list configurations that have
actually been built and verified locally and/or in CI. Configurations outside these tables are
unsupported, even if they happen to compile.

## Lean toolchain window

The supported window is captured in
[`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs); this table is
the human-readable mirror. Releases that ship a byte-identical `lean.h` share one entry.

| Lean versions               | `lean.h` SHA-256                                                   |
| --------------------------- | ------------------------------------------------------------------ |
| 4.26.0                      | `e0ea3efaccceb5b75c7e9e1ab92952c8aa85c3faee28ee949dfeb8ab428ad218` |
| 4.27.0                      | `42255d180910bb063d97c87cfb2a61550009ca9ceb6f495069c56bfaa6c92e13` |
| 4.28.0                      | `624726e5f1f10fd77cd95b8fe8f30389312e57c8fc98e6c2f1989289bdb5fb0e` |
| 4.28.1                      | `648ecfb615ef0222cd63b5f1bbbc379a06749bc0f5f4c2eb16ffca26fd18fe81` |
| 4.29.0                      | `671683950ef412474bede2c6a2b50aecf4f99bc29e1ddaf2222ee54ad4ffb91c` |
| 4.29.1                      | `2e481a0dac7215eb16123eaef97298ae5a6d0bd0c28c534c2818e2d2f2a28efc` |

Digests captured on `aarch64-apple-darwin` 2026-05-18; the same digest applies to
`x86_64-unknown-linux-gnu` builds of the same release (the header is platform-independent). CI
verifies every row × `{ubuntu-latest, macos-latest}` cell.

Extending the window is the [bump procedure](bump-toolchain.md): add a row to
`SUPPORTED_TOOLCHAINS`, add a CI matrix entry, run `scripts/test-all-toolchains.sh` locally,
PR. Untested versions are not supported even if they happen to compile.

The captured baseline build context (for the v0.1.0 release artefacts) is preserved in
[`docs/performance/baseline.md`](performance/baseline.md): commit, allocator, fixture digest,
and which Lean version each benchmark was captured against.

## `lean-rs-sys` symbol coverage

The `REQUIRED_SYMBOLS` allowlist in
[`crates/lean-rs-sys/src/lib.rs`](../crates/lean-rs-sys/src/lib.rs) is the authoritative list
of `LEAN_EXPORT`'d symbols the crate's `extern "C"` blocks declare. It is not duplicated here.

| Crate          | Allowlist size | Source                                                          |
| -------------- | -------------- | --------------------------------------------------------------- |
| `lean-rs-sys`  | 87 symbols     | `pub const REQUIRED_SYMBOLS` in `crates/lean-rs-sys/src/lib.rs` |

`tests/linkage.rs` in `lean-rs-sys` resolves every entry against `libleanshared` at link
time on every version × OS in the CI matrix; the parallel test in `lean-toolchain` imports
the same set via `lean_rs_sys::REQUIRED_SYMBOLS`, confirming the consumer surface stays
aligned. All 87 symbols are present in every release in the supported window
(`SupportedToolchain::missing_symbols` is empty for every entry).

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
- Lean toolchain versions outside the supported window above (e.g. release-candidate tags such
  as `4.30.0-rc2`; these get promoted to the window when they ship as stable).

## Cross-references

- Policy: [`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md)
- Bump procedure: [`docs/bump-toolchain.md`](bump-toolchain.md)
- Release procedure: [`docs/release.md`](release.md)
- Public-API baselines: [`docs/api-review/`](api-review/) and [`docs/api-review.md`](api-review.md)
- Perf baseline context: [`docs/performance/baseline.md`](performance/baseline.md)
