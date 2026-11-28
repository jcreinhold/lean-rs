---
name: bump-toolchain
description: Add or remove a Lean release in lean-rs's supported toolchain window, covering header digests, symbol and layout checks, the newest-release references in CI and scripts, toolchain pins and the evidence to commit. Use for Lean toolchain or supported-window bumps.
---

# Change the supported Lean window

`SUPPORTED_TOOLCHAINS` in `crates/lean-rs-abi/src/supported.rs` lists the supported releases and their `lean.h` digests.
The version matrix in `README.md` restates the list for readers.

## Check a candidate

Install the candidate and compare its header with a supported release:

```sh
elan toolchain install leanprover/lean4:vX.Y.Z
shasum -a 256 ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/include/lean/lean.h
scripts/check-lean-header.sh <supported-version> X.Y.Z
scripts/check-lean-symbols.sh X.Y.Z
```

- The header script diffs the struct declarations that matter; it does not measure sizes, alignment or offsets. Check
  each change against the mirrors in `crates/lean-rs-sys/src/repr.rs` and the layout constants in
  `crates/lean-rs-abi/src/consts.rs`.
- Also review changed inline reference-count helpers and ownership conventions; an unchanged layout is not enough.
- A matching digest lets the candidate join an existing entry, but still run the symbol check against its library. Empty
  output means it found every required symbol.
- Stop on an unexplained change in layout, calling convention or symbols. Do not hide a failure behind version-specific
  wrappers or exceptions: every supported release must export every `REQUIRED_SYMBOLS` entry, and changing that rule
  means changing its callers and tests with it.

## Update the records

1. Add the version to the matching digest entry in `SUPPORTED_TOOLCHAINS`, or add a new entry in version order once the
   new digest passes. Say beside the entry why its header differs.
2. Mirror the version and digest in `crates/lean-rs-sys/digests/manifest.json`.
3. Add the version once to the `full_matrix` list in `.github/workflows/ci.yml`.
4. Update the version matrix in `README.md`: the list, the count of distinct headers, and the reasons for the floor if
   the floor moves.

When the candidate becomes the newest release, also move every reference to the old head:

- the fallback list in `jobs.check.strategy.matrix.lean_version` and the head-gated steps in `ci.yml`;
  `scripts/check_ci_default_lean.py` checks the fallback;
- `LEAN_VERSION_HEAD` in `release.yml`, `release-recover.yml`, `sanitizer.yml` and `compile-fail.yml`, the release test
  matrix and its head-gated steps, and `DEFAULT_LEAN_VERSION` and the head-gated steps in `scripts/prerelease.sh`;
- every tracked `lean-toolchain` file (`git ls-files '*lean-toolchain'`): the shim packages, the fixtures and the
  shipped-crate template;
- the generated-shim slot `lean-<head>`: the version check and directory in each shim `lakefile.lean`, the check in
  `crates/lean-toolchain/src/build_helpers.rs`, and the tests that name it;
- the qualification fixture under `fixtures/metadata-qualification/`, which writes the head's `lean-toolchain` and
  output key.

Find what remains with `git grep -nF '<old head>'`, leaving the support table, the manifest, the CI list and
`CHANGELOG.md`. Keep the committed pins at the head: the sanitizer and compile-fail workflows install that release
without repinning, so a stale pin mixes headers and library paths. `scripts/prerelease.sh` checks that the head
references agree.

## Validate

Select the new toolchain in a throwaway checkout, or restore any pins you change, and run:

```sh
cargo nextest run -p lean-rs-abi -p lean-toolchain -p lean-rs-sys
```

`crates/lean-toolchain/tests/version_window.rs` checks the baked fingerprint and the table;
`crates/lean-rs-sys/tests/linkage.rs` is the link-time symbol probe. The metadata tests alone neither load
`libleanshared` nor prove that a symbol exists.

Then run the checks in `AGENTS.md` on the new release. A CI run started with `full_matrix: true` tests the whole window
on Ubuntu and macOS; pushes and release builds test only the newest release.

For a local sweep, `scripts/test-all-toolchains.sh` takes explicit versions or reads the digest manifest. It repins
every tracked `lean-toolchain`, deletes Cargo and Lake outputs, rebuilds the fixtures, runs workspace nextest and
restores the pins; it keeps a snapshot to recover from an interrupted run. It **skips toolchains that are not
installed**, so read its summary: a clean exit does not prove the whole window ran. Install the versions first and use a
throwaway checkout.

In the commit message, record the candidate's digest, whether it joined an existing entry, the header and inline review,
the symbol-check result, and the releases and platforms actually tested. Look into failures on supported releases too:
adding a release does not excuse a failing one.

## Drop a release

Remove it from the table, the manifest, the CI list and the version matrix, and record why with the failed check or
test. Correcting a claim that never held is a patch release; dropping working support is breaking and bumps the minor
version while the crates are 0.x.

## Failures

| Failure | Next step |
| --- | --- |
| A new release fails while supported ones pass | Reduce the failure; report a regression upstream or leave the release unsupported. |
| Lake artifact names or initializer symbols changed | Extend the loader or build probe to the new shape, and keep the tests for supported releases. |
| Header layout or ownership and return conventions changed | Settle the ABI design before you change digests or mirrors. |
