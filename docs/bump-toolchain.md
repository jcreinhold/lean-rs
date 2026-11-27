# Change the supported Lean window

[`SUPPORTED_TOOLCHAINS`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-abi/src/supported.rs) owns the supported versions and header digests. The
[version matrix](version-matrix.md) describes the tested platforms; the
[compatibility policy](architecture/compatibility.md) governs ABI changes and version drops.

## Verify a candidate

Install the candidate and compare its header with a supported release:

```sh
elan toolchain install leanprover/lean4:vX.Y.Z
shasum -a 256 ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/include/lean/lean.h
scripts/check-lean-header.sh <existing-version> X.Y.Z
scripts/check-lean-symbols.sh X.Y.Z
```

The header script diffs the relevant struct declarations; it does not measure sizes, alignment, or offsets. Inspect any
change against the mirrors in [`repr.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/src/repr.rs) and the layout literals in `lean-rs-abi`.
Also review changed inline refcount helpers and ownership conventions: unchanged struct layout alone is insufficient. A
matching digest permits reusing an existing table entry, but still run the symbol check against the candidate library.
Empty symbol-check output means every required export was found.

Stop on an unexplained layout, calling-convention, or missing-symbol change. Do not hide failures behind
version-specific wrappers or allowlist exceptions. The current contract requires every `REQUIRED_SYMBOLS` entry on every
supported version; adding a `missing_symbols` exception requires changing that contract and its consumers and tests
together.

## Update the support records

1. Append the candidate version to the matching digest entry in `SUPPORTED_TOOLCHAINS`, or add an entry in version order
   after verifying a new digest. Record the reason for any ABI-relevant header change beside the entry.
2. Mirror the version and digest in
   [`crates/lean-rs-sys/digests/manifest.json`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/digests/manifest.json).
3. Add the version once to the `full_matrix` Lean version list in [CI](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/ci.yml). If the window or
   policy changed, update [version-matrix.md](version-matrix.md) and the affected support claims in crate READMEs and
   architecture docs.

When the candidate becomes the head, update these together:

- The fallback JSON list in `jobs.check.strategy.matrix.lean_version` and the head-gated steps in `ci.yml`.
  `scripts/check_ci_default_lean.py` checks the fallback against the supported window head.
- `LEAN_VERSION_HEAD` in `release.yml`, `release-recover.yml`, `sanitizer.yml`, and `compile-fail.yml`; the release
  verification matrix; and `DEFAULT_LEAN_VERSION` in [`scripts/prerelease.sh`](https://github.com/jcreinhold/lean-rs/blob/main/scripts/prerelease.sh).
- Every tracked `lean-toolchain` file. Find the pins with `git ls-files '*lean-toolchain'`; this includes bundled shims,
  fixtures, and the shipped-crate template.
- The generated-shim build slot `lean-<head>`: the version check and directory in each shim `lakefile.lean`, the
  matching check in `crates/lean-toolchain/src/build_helpers.rs`, and the tests that name the slot. Find them with
  `git grep -nF 'lean-<old head>'`.

Keep the committed pins equal to the head. The sanitizer and compile-fail workflows install that version without
repinning the workspace, so mismatched pins can mix headers and runtime-library search paths.

## Validate and record evidence

Select the new toolchain in a disposable checkout or restore any pins you change, then run the focused checks:

```sh
cargo nextest run -p lean-rs-abi -p lean-toolchain -p lean-rs-sys
```

`lean-toolchain/tests/version_window.rs` checks the baked fingerprint and support table. The actual link-time symbol
probe is [`lean-rs-sys/tests/linkage.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/tests/linkage.rs); the toolchain metadata tests alone do
not load `libleanshared` or prove symbol presence.

Run the [workspace gates](https://github.com/jcreinhold/lean-rs/blob/main/CONTRIBUTING.md) on the new version. Use CI's manual `full_matrix: true` dispatch for the
full supported window on Ubuntu and macOS. Ordinary push CI and release verification run the head on both platforms.

For a local sweep, [`scripts/test-all-toolchains.sh`](https://github.com/jcreinhold/lean-rs/blob/main/scripts/test-all-toolchains.sh) accepts explicit versions or
reads the digest manifest. It repins every tracked `lean-toolchain`, deletes Cargo and Lake build outputs, rebuilds the
fixture packages, and runs workspace nextest. It restores pins on exit and keeps a recovery snapshot for interrupted
runs. It **skips uninstalled toolchains**, so a successful exit does not prove the whole window ran; inspect the
summary. Use a disposable checkout and install the intended versions before a sweep.

Commit the candidate digest, whether it matched an existing entry, the header/inline review, symbol-probe result, and
the versions and platforms actually tested. Investigate failures on existing versions as well; adding a version does not
make a failing compatibility cell acceptable.

## Drop a version

Remove it from the support table, digest manifest, CI include list, and documentation. Record why and include the failed
probe or test evidence. Correcting a support claim that never worked is a patch; dropping working support is breaking
and bumps the minor version under the `0.x` policy.

## Diagnose a failure

| Failure | Next step |
| --- | --- |
| New version fails while existing versions pass | Minimize the failure; report an upstream regression or leave the candidate unsupported. |
| Lake artifact names or initializer symbols changed | Extend the loader/build probe to support the new shape and retain tests for existing versions. |
| Header layout or ownership/return conventions changed | Resolve the ABI design before changing accepted digests or mirrors. |
