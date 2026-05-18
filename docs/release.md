# Release Checklist

The `lean-rs` workspace publishes via a single GitHub Actions workflow,
[`.github/workflows/release.yml`](../.github/workflows/release.yml). Pushing a `v<semver>` git
tag fires the workflow, which runs the full pre-flight gate set, the public-API diff, the
workspace publish dry-run, and the four-crate live publish in dependency order, then opens a
GitHub Release whose body is the matching `## [<version>]` section of
[`CHANGELOG.md`](../CHANGELOG.md).

This document is the **human checklist** for the steps before the tag push. The mechanical
steps after the tag push are owned by the workflow.

**Supported Lean window for v0.1.0:** Lean 4.26.0 through 4.29.1, with every release in the
window CI-tested on `{ubuntu-latest, macos-latest}`. Adding the next release follows the
[bump procedure](bump-toolchain.md). Re-confirm against the
[version matrix](version-matrix.md) and `crates/lean-rs-sys/src/supported.rs` before any
release.

**Crate publish order is load-bearing** (per `RD-2026-05-18-001`):

1. `lean-rs-sys`
2. `lean-toolchain` (depends on `lean-rs-sys`)
3. `lean-rs` (depends on `lean-rs-sys`)
4. `lean-rs-host` (depends on `lean-rs` and `lean-rs-sys`)

`cargo publish` enforces the dependency direction via the crates.io index — downstream
publishes will fail with a "no matching package" error until the upstream is indexed. The
release workflow sleeps 90s between each publish step to let the index catch up.

## One-time setup

1. Create a [scoped publish token](https://crates.io/settings/tokens) on crates.io with
   `publish-new`, `publish-update`, and `yank` scopes. Token format: `cio…`.
2. In the GitHub repo settings (Settings → Secrets and variables → Actions), add the token as
   `CARGO_REGISTRY_TOKEN`. The release workflow consumes it via
   `${{ secrets.CARGO_REGISTRY_TOKEN }}`.
3. If you sign git tags (recommended), make sure your GPG / SSH signing key is set in your local
   git config — the workflow does not verify signatures, but a signed tag is the audit trail
   the GitHub Release UI surfaces.

## Step 1 — Pre-flight (local)

These are the gates the release workflow will run; running them locally is the fast feedback
loop before a tag push.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace --profile ci
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

If any fails, stop; do not attempt to package. `cargo test` (single-process) is **not** the
gate — cumulative Lean state OOMs the binary after ~150 tests. See
[`docs/testing.md`](testing.md) for the rationale, the single-test debug escape hatch, and the
local override knobs.

## Step 2 — CHANGELOG + version bump

1. Move the previous `## [Unreleased]` entries into a new `## [vX.Y.Z]` section (or compose a
   fresh section). The release workflow extracts the section body whose heading matches the
   pushed tag and uses it as the GitHub Release body — make sure the heading text matches
   exactly (e.g., `## [0.1.0]` for tag `v0.1.0`).
2. Bump `[workspace.package].version` and `[workspace.dependencies]` constraints in the root
   `Cargo.toml` if the new version differs from what is already there. The release workflow
   asserts `"v${workspace.package.version}" == "${GITHUB_REF_NAME}"` before any publish and
   bails out with a clear error if they disagree.
3. If the public API changed intentionally, regenerate the baselines under
   [`docs/api-review/`](api-review/) in the same commit:

   ```sh
   cargo public-api -p lean-rs-sys    --simplified > docs/api-review/lean-rs-sys-public.txt
   cargo public-api -p lean-toolchain --simplified > docs/api-review/lean-toolchain-public.txt
   cargo public-api -p lean-rs        --simplified > docs/api-review/lean-rs-public.txt
   cargo public-api -p lean-rs-host   --simplified > docs/api-review/lean-rs-host-public.txt
   ```

   The release workflow re-runs `cargo public-api --simplified` for each of the four crates and
   `diff`s against these baselines. A drift fails the workflow before any publish.

## Step 3 — PR and merge

Open a PR with the CHANGELOG + version + (if needed) baseline changes. Merge after CI is green
on the existing matrix workflow (`ci.yml`). The release workflow does not run until the tag is
pushed; the regular CI run on the PR is the final correctness gate.

## Step 4 — (Optional) Workflow dry-run

Before tagging, manually trigger the release workflow with `dry_run: true` from the Actions UI:

> Actions → Release → "Run workflow" → check **dry_run** → Run.

This runs every gate including `cargo publish --workspace --dry-run` and the public-API diff,
but skips the live publish and the GitHub Release. Useful when the CHANGELOG section extraction
or the public-API diff needs a sanity check that doesn't show up in the regular CI run.

## Step 5 — Cut the tag

Only after the merge commit is on `main`:

```sh
git tag -s v0.1.0 -m "lean-rs v0.1.0"
git push origin v0.1.0
```

Use `-s` for a signed tag (recommended) or `-a` for an unsigned annotated tag. Plain
lightweight tags work but lose the message. The tag fires the release workflow.

## Step 6 — Watch the workflow

```sh
gh run watch --workflow=release.yml
```

The workflow:

1. Installs elan + Lean (head of the supported window).
2. Asserts the tag matches the workspace version.
3. Runs `fmt`, `clippy`, `nextest`, doctests, `doc` build.
4. Runs the public-API diff against the committed baselines.
5. Runs `cargo publish --workspace --dry-run`.
6. Publishes the four crates in order, with 90s sleeps between each step.
7. Extracts the matching `## [<version>]` section from `CHANGELOG.md`.
8. Creates a GitHub Release with that body. Tags containing `-` (e.g. `v0.1.0-rc.1`) are
   marked as prereleases automatically.

**If the workflow fails after one crate has published**, crates.io versions are immutable —
do not retry with the same version number. Bump the failed crate's patch version, repeat
Steps 1–3, and re-tag at the new merge commit. The release workflow's tag-vs-version assertion
prevents you from re-tagging against the wrong workspace version.

## Step 7 — Post-publish

- Verify the release on crates.io: `cargo search lean-rs` (the four crates should appear with
  the new version).
- Verify docs.rs built each crate cleanly: visit `https://docs.rs/lean-rs/<version>` (and the
  same for `lean-rs-sys`, `lean-toolchain`, `lean-rs-host`) within ~10 minutes. A docs.rs
  failure is recoverable only by a patch publish with the doc fix.
- Update the `RELEASE-READINESS` and `VERSION-COMPATIBILITY` contracts in
  `prompts/lean-rs/00-current-state.md` with the published versions and tag SHA.
- Open PRs against the downstream proof repos (`lean-rs-downstream`,
  `lean-rs-host-downstream`) to bump their dependencies to the new version. The L2 proof's
  `lakefile.lean` also bumps its `from git "…" @ "vX.Y.Z" / "lake/lean-rs-host-shims"` tag.
- Add a fresh `## [Unreleased]` heading at the top of `CHANGELOG.md`.

## Recorded dry-run status — v0.1.0 (2026-05-18, four-crate set)

Captured on `aarch64-apple-darwin` against Lean 4.29.1 (head of the supported window),
Rust 1.95.0 stable, `cargo 1.95.0`. Refreshed after `RD-2026-05-18-001` split `lean-rs::host`
into the published sibling crate `lean-rs-host` (publish set grew from 3 → 4).

| Crate            | `cargo package` (workspace)        | `cargo publish --dry-run` (workspace)                            | `cargo publish --dry-run` (per-crate)                                                                                       |
| ---------------- | ---------------------------------- | ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `lean-rs-sys`    | OK — `Packaged 26 files, 138.1KiB` | OK — `Uploading lean-rs-sys v0.1.0` (aborted due to dry run)     | OK — same as workspace                                                                                                       |
| `lean-toolchain` | OK — `Packaged 16 files, 65.0KiB`  | OK — `Uploading lean-toolchain v0.1.0` (aborted due to dry run)  | expected failure: `error: failed to prepare local package for uploading … no matching package named lean-rs-sys found`        |
| `lean-rs`        | OK — `Packaged 49 files, 370.9KiB` | OK — `Uploading lean-rs v0.1.0` (aborted due to dry run)         | expected failure: `error: failed to prepare local package for uploading … no matching package named lean-rs-sys found`        |
| `lean-rs-host`   | OK — `Packaged 45 files, 355.2KiB` | OK — `Uploading lean-rs-host v0.1.0` (aborted due to dry run)    | expected failure: `error: failed to prepare local package for uploading … no matching package named lean-rs found`            |

The workspace dry-run is the recommended local gate (`cargo publish --workspace --dry-run`);
the per-crate failures above are the documented pre-publish state, not a regression.

## Fallback — local publish when CI is unavailable

If the GitHub Actions workflow is unavailable (account suspension, runner outage, secret loss),
the publish can be driven from a laptop. Use this only when CI is genuinely blocked.

Local prerequisites: `cargo login` once with the same scoped publish token, then run the four
publishes in order with index-propagation waits between:

```sh
cargo publish -p lean-rs-sys
sleep 90 && cargo publish -p lean-toolchain
sleep 90 && cargo publish -p lean-rs
sleep 90 && cargo publish -p lean-rs-host
```

After all four succeed, push the tag and create the GitHub Release manually:

```sh
git tag -s v0.1.0 -m "lean-rs v0.1.0"
git push origin v0.1.0
gh release create v0.1.0 --notes-file <(awk '/^## \[0\.1\.0\]/{f=1;next}f&&/^## \[/{exit}f' CHANGELOG.md)
```

If any `cargo publish` fails — **stop**. Do not re-run with `--allow-dirty` or attempt to
overwrite the published version. crates.io versions are immutable; a failed publish that did
upload but failed to index requires bumping the patch version.

`cargo publish --dry-run` does not need credentials and is safe to run anytime.
