# Adding a Lean release to the supported window

Checklist for extending `lean-rs`'s supported toolchain window with a new Lean release. Takes
~30 minutes plus CI time. The end state is a workspace that builds + tests cleanly against the
new release on every CI cell.

The single source of truth for the supported window is
[`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs). Everything
below either updates that file or follows from it.

## Steps

### 1. Install the toolchain locally

```sh
elan toolchain install leanprover/lean4:vX.Y.Z
```

This downloads ~500 MB. Skip if already installed.

### 2. Capture the `lean.h` SHA-256

```sh
shasum -a 256 ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/include/lean/lean.h \
  | cut -d' ' -f1
```

Compare against the existing entries in [`SUPPORTED_TOOLCHAINS`](../crates/lean-rs-sys/src/supported.rs):

- **If the digest matches an existing entry**, append `"X.Y.Z"` to that entry's `versions`
  array. This is the common case: Lean often ships point releases without touching `lean.h`.
- **If the digest is new**, you also need step 3.

### 3. (If the digest is new) Verify layout + symbol compatibility

Quick check — the load-bearing assertions for adding a new digest entry:

```sh
# (a) The 10 #[repr(C)] struct definitions in lean-rs-sys/src/repr.rs must be byte-identical
# to the active release's header. Diff the relevant lines:
diff \
  <(awk '/^typedef struct lean_object \{|^} lean_(ctor|array|sarray|string|closure|ref|thunk|task|promise|external)_object/{p=1} p{print} /^} lean_external_object/{p=0}' \
    ~/.elan/toolchains/leanprover--lean4---v$EXISTING_VERSION/include/lean/lean.h) \
  <(awk '/^typedef struct lean_object \{|^} lean_(ctor|array|sarray|string|closure|ref|thunk|task|promise|external)_object/{p=1} p{print} /^} lean_external_object/{p=0}' \
    ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/include/lean/lean.h)

# Empty diff = layouts unchanged = safe to add as another window entry. Non-empty
# diff = file a Replanning Delta per docs/architecture/02-versioning-and-compatibility.md;
# do not silently add the entry.

# (b) Every REQUIRED_SYMBOLS entry must resolve in the new libleanshared:
awk '/^pub const REQUIRED_SYMBOLS/,/^];/' crates/lean-rs-sys/src/lib.rs \
  | grep -oE '"lean_[a-z0-9_]+"' | tr -d '"' | sort -u > /tmp/required.txt
nm -gU ~/.elan/toolchains/leanprover--lean4---vX.Y.Z/lib/lean/libleanshared.dylib \
  | awk '{print $NF}' | sed 's/^_//' | grep -E '^lean_' | sort -u > /tmp/syms.txt
comm -23 /tmp/required.txt /tmp/syms.txt   # expect empty

# If non-empty: a symbol disappeared upstream. Either add it to that entry's
# missing_symbols list (and update consumer call sites to tolerate absence), or
# file a Replanning Delta.
```

### 4. Update the `SUPPORTED_TOOLCHAINS` table

Edit [`crates/lean-rs-sys/src/supported.rs`](../crates/lean-rs-sys/src/supported.rs):

- Add a new `SupportedToolchain { versions: &["X.Y.Z"], header_digest: "<digest>", missing_symbols: &[] }`
  entry in version order, OR append `"X.Y.Z"` to an existing entry's `versions` array (when
  the digest matches).
- Update the mirror entries in
  [`crates/lean-rs-sys/digests/manifest.json`](../crates/lean-rs-sys/digests/manifest.json) and
  [`docs/version-matrix.md`](version-matrix.md).

### 5. Update the CI matrix

Edit [`.github/workflows/ci.yml`](../.github/workflows/ci.yml): add `"X.Y.Z"` to the
`matrix.lean_version` list. If `X.Y.Z` is the new highest version, also update the head version
in [`.github/workflows/sanitizer.yml`](../.github/workflows/sanitizer.yml).

### 6. Run the local sweep

```sh
scripts/test-all-toolchains.sh
```

This iterates every version in `digests/manifest.json`, repoints the workspace
`lean-toolchain` files (root + `lake/lean-rs-host-shims/` + `fixtures/lean/`), rebuilds the
Lake packages, runs `cargo nextest run --workspace`, and prints a per-version pass/fail
summary. The script restores the original `lean-toolchain` files on exit (even on failure).

### 7. Commit + PR

Commit message convention: `Add Lean X.Y.Z to the supported toolchain window`. PR description
includes:

- The new digest (and whether it matched an existing entry).
- Output of step 6 (passed/failed columns).
- Any `missing_symbols` updates and why.

## When the bump fails

If the local sweep surfaces a test failure on the new version, **do not pin around it** with
brittle wrappers or version-specific test allowlists. Two acceptable resolutions:

1. **Real regression upstream** — file an issue on the Lean repo with a minimal repro; consider
   whether to skip the version pending an upstream fix (i.e. exclude it from the table for
   now).
2. **Naming-convention or signature change** — extend the relevant probe (e.g. the dylib
   filename probe in `crates/lean-rs-host/src/host/lake.rs`, the initializer-symbol probe in
   `crates/lean-rs/src/module/initializer.rs`) to handle the new shape alongside the existing
   ones. Tests must pass against every version in the window.

For shifts in the C ABI itself (layout, ownership conventions, header digest with non-additive
diff), follow [`00-recovery-protocol.md`](../../prompts/lean-rs/00-recovery-protocol.md) and
file a Replanning Delta. Do not silently bump `EXPECTED_HEADER_DIGEST` without updating the
mirrors.
