# Adding a Lean release to the supported window

Adding a version requires two verifications: the `lean.h` layout is
unchanged from an existing entry, and every symbol in `REQUIRED_SYMBOLS`
still resolves in the new `libleanshared`. The single source of truth is
[`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs);
the steps below either update that file or follow from it. Budget ~30
minutes plus CI time. End state: the workspace builds and tests cleanly
against the new release on every CI cell.

## Steps

### 1. Install the toolchain

```sh
elan toolchain install leanprover/lean4:vX.Y.Z
```

~500 MB. Skip if already installed.

### 2. Capture the `lean.h` SHA-256

```sh
shasum -a 256 \
  ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/include/lean/lean.h | cut -d' ' -f1
```

Compare against existing entries in [`SUPPORTED_TOOLCHAINS`](../crates/lean-rs-sys/src/supported.rs):

- **Digest matches an existing entry** → append `"X.Y.Z"` to that entry's
  `versions` array, then jump to step 4. The common case; Lean often
  ships point releases without touching `lean.h`.
- **New digest** → run step 3 first.

### 3. (New digest only) Verify layout + symbols

Two checks. Both must pass; either failure means do **not** silently add
the entry.

**Layout check.** The 10 `#[repr(C)]` struct definitions in
`lean-rs-sys/src/repr.rs` must be byte-identical to the new header's
relevant block:

```sh
scripts/check-lean-header.sh <existing-version> X.Y.Z
```

Empty output: layouts unchanged, safe to add. Non-empty: stop and revisit
[`02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md).

**Symbol check.** Every `REQUIRED_SYMBOLS` entry must resolve:

```sh
scripts/check-lean-symbols.sh X.Y.Z
```

Empty output: every required symbol resolves. Non-empty: a symbol
disappeared upstream. Either add it to the entry's `missing_symbols` list
(and update consumer call sites to tolerate absence), or file an upstream
issue and stop.

### 4. Update the `SUPPORTED_TOOLCHAINS` table

Edit [`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs):

- Add a new
  `SupportedToolchain { versions: &["X.Y.Z"], header_digest: "<digest>", missing_symbols: &[] }`
  entry in version order, **or** append `"X.Y.Z"` to an existing entry's
  `versions` array when the digest matched.
- Mirror the entry in
  [`crates/lean-rs-sys/digests/manifest.json`](../crates/lean-rs-sys/digests/manifest.json)
  and [`docs/version-matrix.md`](version-matrix.md).

### 5. Update the CI matrix

Edit [`.github/workflows/ci.yml`](../.github/workflows/ci.yml): add
`"X.Y.Z"` to `matrix.lean_version`. If `X.Y.Z` is the new highest
version, also update the head version in
[`.github/workflows/sanitizer.yml`](../.github/workflows/sanitizer.yml).

### 6. Run the local sweep

```sh
scripts/test-all-toolchains.sh
```

Iterates every version in `digests/manifest.json`, repoints the workspace
`lean-toolchain` files (root + `lake/lean-rs-host-shims/` +
`fixtures/lean/`), rebuilds the Lake packages, runs `cargo nextest run
--workspace`, and prints a per-version pass/fail summary. Restores the
original `lean-toolchain` files on exit (even on failure).

### 7. Commit and PR

Commit message: `Add Lean X.Y.Z to the supported toolchain window`. PR
description includes:

- The new digest (and whether it matched an existing entry).
- The step 6 summary.
- Any `missing_symbols` updates and why.

## When the bump fails

A test failure on the new version does **not** justify pinning around it
with brittle wrappers or version-specific allowlists. The right
resolution depends on what broke:

| Symptom                                                                                       | Cause                                                              | Action                                                                                                                                                                                                                                            |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Test fails on the new version, passes on every other version                                  | Upstream regression                                                | File an issue on the Lean repo with a minimal repro. Consider skipping the version (exclude from the table) pending the fix.                                                                                                                      |
| Lake's emitted artifact name or initializer-symbol shape changed                              | Naming/signature shift in Lake codegen                             | Extend the relevant probe (`crates/lean-rs-host/src/host/lake.rs` for dylib names; `crates/lean-rs/src/module/initializer.rs` for initializer symbols) to accept the new shape alongside the existing ones. Tests must pass on every window entry. |
| `repr.rs` no longer matches the header, or ownership/return conventions differ                | C ABI shift                                                        | Stop and discuss with maintainers before patching. Do not silently bump `EXPECTED_HEADER_DIGEST` without updating the mirrors.                                                                                                                    |
