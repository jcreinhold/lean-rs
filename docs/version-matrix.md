# Version Matrix

Tested configurations for the four published `lean-rs` crates as of **v0.1.0** (2026-05-18).
Configurations outside these tables are unsupported, even if they happen to compile.

- Policy: [`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md)
- Release procedure: [`docs/release.md`](release.md)
- Bump procedure: [`docs/bump-toolchain.md`](bump-toolchain.md)

## Lean toolchain window

Authoritative source: [`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs).
Releases that ship a byte-identical `lean.h` share one entry. CI verifies every row ×
`{ubuntu-latest, macos-latest}` cell.

| Lean version | `lean.h` digest (12-char prefix) |
| --- | --- |
| 4.26.0 | `e0ea3efaccce…` |
| 4.27.0 | `42255d180910…` |
| 4.28.0 | `624726e5f1f1…` |
| 4.28.1 | `648ecfb615ef…` |
| 4.29.0 | `671683950ef4…` |
| 4.29.1 | `2e481a0dac72…` |

Digests captured on `aarch64-apple-darwin` 2026-05-18; identical on
`x86_64-unknown-linux-gnu` (the header is platform-independent). Full SHA-256 values live in
`supported.rs`.

Extending the window is the [bump procedure](bump-toolchain.md). Untested versions are not
supported.

## `lean-rs-sys` symbol coverage

`pub const REQUIRED_SYMBOLS` in [`crates/lean-rs-sys/src/lib.rs`](../crates/lean-rs-sys/src/lib.rs)
enumerates the **87** `LEAN_EXPORT`'d symbols the crate's `extern "C"` blocks declare.
`tests/linkage.rs` resolves every entry against `libleanshared` at link time on every
version × OS cell; the parallel test in `lean-toolchain` imports the same set via
`lean_rs_sys::REQUIRED_SYMBOLS`. All 87 symbols are present in every release in the window
(`SupportedToolchain::missing_symbols` is empty for every entry).

## Rust

| Field | Value |
| --- | --- |
| MSRV | `1.91` (from `[workspace.package].rust-version`) |
| Channel | `stable` (pinned by [`rust-toolchain.toml`](../rust-toolchain.toml)) |
| Captured at release | `rustc 1.95.0 (59807616e 2026-04-14)` |

The MSRV is the floor a downstream consumer can rely on; the CI release matrix runs on the
current stable.

## Platforms

| Platform | Triple | Status |
| --- | --- | --- |
| Ubuntu Latest (GitHub Actions) | `x86_64-linux-gnu` | supported, CI |
| macOS Latest (GitHub Actions) | `aarch64-apple-darwin` | supported, CI |

Explicitly unsupported (do not file as bugs without a compatibility-decision proposal):

- Windows (any toolchain). Adding requires a CI matrix entry, documented build flag for MSVC linking and the `lean-rs-sys` feature selection, and an update to [`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md).
- BSDs, embedded targets, WASM.
- Release-candidate Lean tags (e.g. `4.30.0-rc2`); promoted to the window when they ship stable.

## See also

For how to run the benchmarks and detect regressions, see
[`docs/performance.md`](performance.md). For the frozen public API of each crate, see
[`docs/api-review/`](api-review/).
