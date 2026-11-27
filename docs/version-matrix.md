# Version matrix

The supported Lean releases are the explicit entries in
[`SUPPORTED_TOOLCHAINS`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-abi/src/supported.rs), currently 4.30.0 through 4.36.0 including the listed
release candidates. The interval is shorthand; an unlisted version is unsupported even if it compiles.

## Lean ABI

The table contains thirteen distinct `lean.h` digests. Releases with identical headers share an entry. The comments beside
those entries record the verified changes to atomic fields, refcount helpers, and the 4.35 array linearity marker; use
that source when extending the window rather than copying digest history into documentation.

[`REQUIRED_SYMBOLS`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-abi/src/symbols.rs) lists 91 exported symbols. Every supported entry has an empty
`missing_symbols` list. [`lean-rs-sys/tests/linkage.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/tests/linkage.rs) takes their addresses
to force link-time resolution; `lean-toolchain`'s version-window tests check the static support metadata.

The same window applies to bundled shims and worker capabilities. A worker child, its Lean runtime, and loaded
capability artifacts must agree on the supported toolchain. See the [interop release checks](release.md#interop-checks).

## Rust and platforms

| Setting | Supported value |
| --- | --- |
| Rust MSRV | `1.94`, from `[workspace.package].rust-version` |
| Rust channel used by CI | `stable`, from [`rust-toolchain.toml`](https://github.com/jcreinhold/lean-rs/blob/main/rust-toolchain.toml) |
| Linux | GitHub Actions `ubuntu-latest`, `x86_64-unknown-linux-gnu` |
| macOS | GitHub Actions `macos-latest`, `aarch64-apple-darwin` |

Windows, BSD, embedded, and WASM targets are unsupported. Adding a platform requires an explicit compatibility decision
and CI coverage. The opt-in managed native-reader completion check currently requires the macOS process-group census;
Linux qualification for that mode remains open, as recorded in the [0.10.0 changelog](https://github.com/jcreinhold/lean-rs/blob/main/CHANGELOG.md#0100).

## CI coverage

| Trigger | Lean versions | Platforms |
| --- | --- | --- |
| Ordinary code push or PR | Head (`4.36.0`) | Ubuntu and macOS |
| CI dispatch with `full_matrix: true` | Every listed version | Ubuntu and macOS |
| Release verification | Head | Ubuntu and macOS |

Documentation-only changes skip the Rust CI workflow and build the mdBook when `docs/**` changes. The workflow
definitions are authoritative: [CI](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/ci.yml), [release](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/release.yml), and
[docs](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/docs.yml). These configurations describe coverage, not a claim that every dispatch has
passed.

See the [compatibility policy](architecture/compatibility.md),
[toolchain change procedure](bump-toolchain.md), and [release checklist](release.md).
