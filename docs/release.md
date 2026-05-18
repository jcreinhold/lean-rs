# Release Checklist

Human-runnable procedure for publishing a new version of the three `lean-rs` workspace crates to
crates.io. The supported Lean toolchain range, MSRV, and tested platforms for each release are
recorded in [`docs/version-matrix.md`](version-matrix.md). The narrative changelog is at the
repository root, [`CHANGELOG.md`](../CHANGELOG.md).

**Supported Lean range for v0.1.0:** Lean 4.29.1 (single point release). Re-confirm against the
version matrix before any release.

**Crate publish order is load-bearing:**

1. `lean-rs-sys`
2. `lean-toolchain` (depends on `lean-rs-sys`)
3. `lean-rs` (depends on `lean-rs-sys` and `lean-toolchain`)

`cargo publish` enforces the dependency direction via the crates.io index — the downstream
publishes will fail with a "no matching package" error until the upstream package is indexed.
Sequencing manually keeps the failure window short and the human in the loop.

## Step 1 — Pre-flight

The workspace gates must be clean before any packaging work:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

These are the same commands CI runs on `ubuntu-latest` and `macos-latest`. If any fails, stop; do
not attempt to package.

`cargo test` (single-process) is unsupported as the workspace gate — cumulative Lean state OOMs
the binary after ~150 tests. See [`docs/testing.md`](testing.md) for the rationale, the single-test
debug escape hatch, and the local override knobs.

## Step 2 — Public-API diff

For each of the three published crates, diff the current public surface against the committed
baseline at [`docs/api-review/`](api-review/):

```sh
cargo public-api --diff-git-checkouts <baseline-sha>..HEAD -p lean-rs-sys
cargo public-api --diff-git-checkouts <baseline-sha>..HEAD -p lean-toolchain
cargo public-api --diff-git-checkouts <baseline-sha>..HEAD -p lean-rs
```

The result must be a subset of the baseline (additions only) **or** the release is intentionally
breaking and a major version bump is part of this release. `cargo-public-api` is a developer
install (`cargo install cargo-public-api`) and not yet wired into CI; see the audit at
[`docs/api-review.md`](api-review.md) for the caveat.

If the diff is non-empty and intentional, regenerate the baselines in the same commit:

```sh
cargo public-api -p lean-rs-sys    --simplified > docs/api-review/lean-rs-sys-public.txt
cargo public-api -p lean-toolchain --simplified > docs/api-review/lean-toolchain-public.txt
cargo public-api -p lean-rs        --simplified > docs/api-review/lean-rs-public.txt
```

## Step 3 — Packaging gate (`cargo package`)

Produce the tarballs without uploading. Order matches the publish order:

```sh
cargo package -p lean-rs-sys
cargo package -p lean-toolchain
cargo package -p lean-rs
```

Each produces `target/package/<crate>-<version>.crate`. Inspect the contents:

```sh
for c in lean-rs-sys lean-toolchain lean-rs; do
  printf '\n== %s ==\n' "$c"
  tar tzf "target/package/${c}-$(cargo metadata --no-deps --format-version 1 | jq -r --arg c "$c" '.packages[]|select(.name==$c).version')".crate | sort
done
```

Each tarball must contain `Cargo.toml`, `Cargo.toml.orig`, `LICENSE-MIT`, `LICENSE-APACHE`,
`README.md`, the crate `src/`, `build.rs`, and the in-tree `tests/` sources. Must **not** contain
`target/`, `.lake/`, `*.olean`, editor swap files, or (for `lean-rs`) the `fuzz/` sub-crate.

## Step 4 — Dry-run gate (`cargo publish --dry-run`)

Use the workspace form — Cargo (≥ 1.90) builds a temporary local registry to satisfy intra-workspace
dependencies, so all three dry-runs verify cleanly without anything having to be on crates.io
first:

```sh
cargo publish --workspace --dry-run
```

The per-crate form is also available, but the two downstream crates will fail until the upstream
is actually published:

```sh
cargo publish -p lean-rs-sys    --dry-run    # always works in isolation
cargo publish -p lean-toolchain --dry-run    # fails until lean-rs-sys is on crates.io
cargo publish -p lean-rs        --dry-run    # fails until lean-rs-sys is on crates.io
```

The per-crate failure is the expected pre-publish state — `no matching package named lean-rs-sys
found … location searched: crates.io index`. Do not treat it as a regression.

### Recorded dry-run status — v0.1.0 (2026-05-18)

Captured on `aarch64-apple-darwin` against Lean 4.29.1, Rust 1.95.0 stable, `cargo 1.95.0`.

| Crate            | `cargo package` (workspace)                 | `cargo publish --dry-run` (workspace)       | `cargo publish --dry-run` (per-crate)                                                                       |
| ---------------- | ------------------------------------------- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `lean-rs-sys`    | OK — `Packaged 26 files, 138.1KiB`          | OK — `Uploading lean-rs-sys v0.1.0` (aborted due to dry run) | OK — same as workspace                                                                                       |
| `lean-toolchain` | OK — `Packaged 16 files, 65.0KiB`           | OK — `Uploading lean-toolchain v0.1.0` (aborted due to dry run) | expected failure: `error: failed to prepare local package for uploading … no matching package named lean-rs-sys found` |
| `lean-rs`        | OK — `Packaged 84 files, 678.2KiB`          | OK — `Uploading lean-rs v0.1.0` (aborted due to dry run) | expected failure: `error: failed to prepare local package for uploading … no matching package named lean-rs-sys found` |

The workspace dry-run is the recommended local gate; the per-crate failures above are the
documented pre-publish state, not a regression. When credentials are absent and the upstream
registry refuses any network call, `cargo package --workspace` remains the load-bearing local
gate.

## Step 5 — Live publish

Only after the pre-flight, diff, and packaging gates are clean. The live publish runs per-crate
(not `--workspace`) so that a single-crate failure does not leave a partial release. Wait between
steps for the registry to index the previous publish (typically <60s):

```sh
cargo publish -p lean-rs-sys
# wait for index
cargo publish -p lean-toolchain
# wait for index
cargo publish -p lean-rs
```

If any `cargo publish` fails:

- Stop. Do **not** re-run with `--allow-dirty` or attempt to overwrite the published version.
- crates.io versions are immutable. A failed publish that did upload but failed to index requires
  bumping the patch version and re-running from step 1.

## Step 6 — Tag and push

Only after all three `cargo publish` calls succeed:

```sh
git tag -s v0.1.0 -m "lean-rs v0.1.0"
git push origin v0.1.0
```

Do not tag earlier; a tag points at an immutable claim about what was published.

## Step 7 — Post-publish

- Update [`CHANGELOG.md`](../CHANGELOG.md) with the next `## [Unreleased]` section.
- Update the `RELEASE-READINESS` and `VERSION-COMPATIBILITY` contracts in
  `prompts/lean-rs/00-current-state.md` with the published versions and the tag SHA.
- Open the GitHub Release UI for the tag and paste the relevant `CHANGELOG.md` subsection.

## Authentication

`cargo publish` (without `--dry-run`) requires a crates.io token. The recommended setup is
`cargo login` once per machine using a scoped publish token from
<https://crates.io/settings/tokens>. The token is stored under `~/.cargo/credentials.toml`; do not
commit it. `cargo publish --dry-run` does not need credentials.
