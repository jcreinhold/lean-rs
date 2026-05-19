# Adding a Lean release to the supported window

Extends `lean-rs`'s supported toolchain window with a new Lean release. ~30 minutes plus CI
time. End state: workspace builds and tests cleanly against the new release on every CI cell.

Single source of truth is
[`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs). Everything
below either updates that file or follows from it.

## Steps

### 1. Install the toolchain

```sh
elan toolchain install leanprover/lean4:vX.Y.Z
```

~500 MB download. Skip if already installed.

### 2. Capture the `lean.h` SHA-256

```sh
shasum -a 256 \
  ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/include/lean/lean.h | cut -d' ' -f1
```

Compare against the existing entries in [`SUPPORTED_TOOLCHAINS`](../crates/lean-rs-sys/src/supported.rs):

- **Digest matches an existing entry** → append `"X.Y.Z"` to that entry's `versions` array, then jump to step 4. Common case; Lean often ships point releases without touching `lean.h`.
- **New digest** → do step 3 first.

### 3. (New digest only) Verify layout + symbol compatibility

Two checks. Both must pass; either failure means do **not** silently add the entry.

**Layout check.** The 10 `#[repr(C)]` struct definitions in `lean-rs-sys/src/repr.rs` must be
byte-identical to the active release's header. Extract the relevant block from each header and
diff:

```sh
EXTRACT='/^typedef struct lean_object \{|^} lean_(ctor|array|sarray|string|closure|ref|thunk|task|promise|external)_object/{p=1} p{print} /^} lean_external_object/{p=0}'

diff \
  <(awk "$EXTRACT" ~/.elan/toolchains/leanprover--lean4---v$EXISTING_VERSION/include/lean/lean.h) \
  <(awk "$EXTRACT" ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/include/lean/lean.h)
```

Empty diff = layouts unchanged, safe to add as another window entry. Non-empty diff = stop
and revisit the supported-window policy in
[`docs/architecture/02-versioning-and-compatibility.md`](architecture/02-versioning-and-compatibility.md)
before proceeding.

**Symbol check.** Every `REQUIRED_SYMBOLS` entry must resolve in the new `libleanshared`:

```sh
awk '/^pub const REQUIRED_SYMBOLS/,/^];/' crates/lean-rs-sys/src/lib.rs \
  | grep -oE '"lean_[a-z0-9_]+"' | tr -d '"' | sort -u > /tmp/required.txt

nm -gU ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/lib/lean/libleanshared.dylib \
  | awk '{print $NF}' | sed 's/^_//' | grep -E '^lean_' | sort -u > /tmp/syms.txt

comm -23 /tmp/required.txt /tmp/syms.txt   # expect empty
```

Non-empty output = a symbol disappeared upstream. Two paths: add it to the entry's
`missing_symbols` list (and update consumer call sites to tolerate absence), or file an
upstream issue and stop.

### 4. Update the `SUPPORTED_TOOLCHAINS` table

Edit [`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs):

- Add a new `SupportedToolchain { versions: &["X.Y.Z"], header_digest: "<digest>", missing_symbols: &[] }` entry in version order, **or** append `"X.Y.Z"` to an existing entry's `versions` array when the digest matched.
- Update the mirror entries in [`crates/lean-rs-sys/digests/manifest.json`](../crates/lean-rs-sys/digests/manifest.json) and [`docs/version-matrix.md`](version-matrix.md).

### 5. Update the CI matrix

Edit [`.github/workflows/ci.yml`](../.github/workflows/ci.yml): add `"X.Y.Z"` to
`matrix.lean_version`. If `X.Y.Z` is the new highest version, also update the head version in
[`.github/workflows/sanitizer.yml`](../.github/workflows/sanitizer.yml).

### 6. Run the local sweep

```sh
scripts/test-all-toolchains.sh
```

Iterates every version in `digests/manifest.json`, repoints the workspace `lean-toolchain` files
(root + `lake/lean-rs-host-shims/` + `fixtures/lean/`), rebuilds the Lake packages, runs
`cargo nextest run --workspace`, and prints a per-version pass/fail summary. The script restores
the original `lean-toolchain` files on exit (even on failure).

### 7. Commit and PR

Commit message: `Add Lean X.Y.Z to the supported toolchain window`. PR description includes:

- New digest (and whether it matched an existing entry).
- Output of step 6 (passed/failed columns).
- Any `missing_symbols` updates and why.

## When the bump fails

A test failure on the new version does **not** justify pinning around it with brittle wrappers
or version-specific test allowlists. The right resolution depends on what broke:

| Failure shape | Action |
| --- | --- |
| Test fails on the new version, passes on every other version in the window | Real upstream regression. File an issue on the Lean repo with a minimal repro; consider skipping the version (exclude from the table) pending the fix. |
| Naming or signature change in Lake's emitted artifacts (dylib filename, initializer symbol shape) | Extend the relevant probe (`crates/lean-rs-host/src/host/lake.rs` for the dylib filename; `crates/lean-rs/src/module/initializer.rs` for initializer symbols) to handle the new shape alongside existing ones. Tests must pass against every version in the window. |
| C ABI shift (layout, ownership conventions, non-additive header diff) | Stop and discuss with maintainers before patching around the diff. Do not silently bump `EXPECTED_HEADER_DIGEST` without updating the mirrors. |
