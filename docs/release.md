# Release Checklist

The `lean-rs` workspace publishes via [`.github/workflows/release.yml`](../.github/workflows/release.yml). Pushing a
`v<semver>` git tag fires the workflow, which runs the pre-flight gate set, the public-API diff, workspace package
creation, the per-crate live publish in dependency order, and opens a GitHub Release whose body is the matching
`## [<version>]` section of [`CHANGELOG.md`](../CHANGELOG.md).

This document is the **human checklist** for the steps before the tag push. The steps after the tag push are owned by
the workflow.

**Supported Lean window for v0.2.x:** 4.27.0 through 4.33.0-rc1. Adding the next release follows the
[bump procedure](bump-toolchain.md); re-confirm against the [version matrix](version-matrix.md) and
`crates/lean-rs-abi/src/supported.rs` before any release.

**Publishing is [`scripts/publish-workspace.sh`](../scripts/publish-workspace.sh): idempotent, per-crate, in dependency
order.** It walks the crates topologically, skips any already on crates.io at the workspace version, and publishes each
missing one with its own `cargo publish -p` (which waits for that upload to land in the index before the next crate's
verify build resolves it). Because already-published crates are skipped, **re-running the publish job after a partial
upload completes the release without burning a version**—no separate recovery run is needed in the common case. We do
not use `cargo publish --workspace`: its single global index-propagation deadline aborts the entire plan when one crate
loses the race, stranding the crates that had not uploaded and forcing a recovery run on nearly every release. The same
script backs `release-recover.yml`.

## One-time setup

1. Create a [scoped publish token](https://crates.io/settings/tokens) on crates.io with `publish-new`, `publish-update`,
   and `yank` scopes. Token format: `cio…`.
2. Add the token in the GitHub repo settings (Settings → Secrets and variables → Actions) as `CARGO_REGISTRY_TOKEN`.
3. If you sign git tags (recommended), set up your GPG / SSH signing key in your local git config—the workflow doesn't
   verify signatures, but a signed tag is the audit trail the GitHub Release UI surfaces.

## Step 1—Pre-flight (local)

The gates the release workflow will run; running them locally is the fast feedback loop.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace --profile ci
cargo test --doc --workspace
cargo test -p lean-rs-worker-child --test loader_regressions -- --nocapture --test-threads=1
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
DOCS_RS=1 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
python3 scripts/check_package_docsrs.py
cargo package --workspace --no-verify
actionlint .github/workflows/ci.yml .github/workflows/release.yml .github/workflows/sanitizer.yml .github/workflows/compile-fail.yml
```

Stop on any failure. `cargo test` (single-process) is not the gate—see [`docs/testing.md`](testing.md).

## Step 2—CHANGELOG + version bump

1. Move `## [Unreleased]` entries into a new `## [X.Y.Z]` section (or compose fresh). The workflow extracts the section
   body whose heading matches the pushed tag—heading text must match exactly (e.g., `## [0.2.1]` for tag `v0.2.1`).
2. Bump `[workspace.package].version` and `[workspace.dependencies]` in the root `Cargo.toml` if they don't already
   match. The workflow asserts `"v${workspace.package.version}" == "${GITHUB_REF_NAME}"` before any publish.
3. If the public API changed intentionally, regenerate the baselines in the same commit:

   ```sh
   for c in lean-rs-sys lean-toolchain lean-rs-interop-shims lean-rs lean-rs-host lean-rs-worker-protocol lean-rs-worker-parent lean-rs-worker-child; do
     cargo public-api -p "$c" --simplified > "docs/api-review/${c}-public.txt"
   done
   ```

   The workflow re-runs `cargo public-api --simplified` for each crate and `diff`s against
   these baselines. Drift fails the workflow before any publish.

## Step 3—PR and merge

Open a PR with CHANGELOG + version + (if needed) baseline changes. Merge after CI is green on the existing `ci.yml`
matrix. The release workflow does not run until the tag is pushed; the regular CI run on the PR is the final correctness
gate.

## Step 4—(Optional) Workflow dry-run

Before tagging, manually trigger the release workflow with `dry_run: true` from the Actions UI (Actions → Release → "Run
workflow" → check **dry_run**). Runs every gate including workspace package creation and the public-API diff but skips
the live publish and the GitHub Release. Useful when CHANGELOG section extraction or the public-API diff needs a sanity
check that doesn't show up in the regular CI run.

The dry run invokes `scripts/publish-workspace.sh --dry-run`, which verify-builds each crate without uploading. One
caveat: a dry run cannot resolve a downstream crate against an upstream workspace crate that has not actually been
uploaded, so a dry run of the full chain fails on the first crate with an unpublished workspace dependency. That failure
is a dry-run artifact (the real run uploads each crate before the next resolves it), so use the dry run to exercise the
gates and CHANGELOG/baseline checks, not as a publish rehearsal.

## Step 5—Cut the tag

Only after the merge commit is on `main`:

```sh
git tag -s v0.2.1 -m "lean-rs v0.2.1"
git push origin v0.2.1
```

`-s` for a signed tag (recommended) or `-a` for unsigned annotated. The tag fires the workflow.

## Step 6—Watch the workflow

```sh
gh run watch --workflow=release.yml
```

The workflow:

1. Installs elan + Lean (head of the supported window).
2. Asserts the tag matches the workspace version.
3. Runs `fmt`, `clippy`, `nextest`, doctests, `doc` build.
4. Runs the docs.rs-compatible doc build with `DOCS_RS=1`.
5. Runs the public-API diff against the committed baselines.
6. Runs `python3 scripts/check_package_docsrs.py`, which packages the workspace, checks crate/template package contents,
   unpacks the normalized tarballs, hides Lean/elan/lake from `PATH`, and builds docs with `DOCS_RS=1`.
7. Runs `cargo package --workspace --no-verify` to create the package tarballs.
8. Publishes the workspace with `scripts/publish-workspace.sh` (idempotent, per-crate, topological order; skips crates
   already on crates.io).
9. Extracts the matching `## [<version>]` section from `CHANGELOG.md`.
10. Creates a GitHub Release with that body. Tags containing `-` (e.g. `v0.2.0-rc.1`) are marked prerelease
    automatically.

**If the workflow fails after some crates have published** (a partial release), crates.io versions are immutable, but
the publish step is idempotent, so recovery does **not** burn a version:

1. **Re-run the failed job** (Actions → the failed run → "Re-run failed jobs"). The publish script skips the crates
   already on crates.io and publishes only the rest. This is the normal fix—a partial upload from an index-propagation
   race (`... awaiting confirmation`) completes on the second attempt.
2. **If re-running the tag job is undesirable or unavailable** (the heavy `verify` matrix already passed and you want a
   light publish-only run, or the tag job's environment is wedged), run the `release-recover.yml` workflow (Actions →
   "Release recovery" → Run workflow) with the same `version` (e.g. `0.2.3`). It runs the same idempotent publish script
   on a fresh checkout and ensures the GitHub Release exists. The workspace version on the checked-out ref must equal
   the `version` input, since it uploads the crate contents on that ref. `dry_run: true` verify-builds without uploading
   (subject to the dry-run interdependency caveat above).

Only when a crate's *contents* must change (a genuine build break, not a propagation race) do you bump the patch
version: repeat steps 1–3 and re-tag at the new merge commit. The tag-vs-version assertion prevents re-tagging against
the wrong workspace version.

## Step 7—Post-publish

- Verify the release on crates.io: `cargo search lean-rs` (every published crate should appear with the new version).
- Verify docs.rs built each crate cleanly: visit `https://docs.rs/lean-rs/<version>` (and the same for each of the other
  published crates) within ~10 minutes. A docs.rs failure is recoverable only by a patch publish with the doc fix.
- Add a fresh `## [Unreleased]` heading at the top of `CHANGELOG.md`.

## Fallback—local publish when CI is unavailable

Use only when CI is genuinely blocked (account suspension, runner outage, secret loss).

```sh
version=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["version"] for p in m["packages"] if p["name"]=="lean-rs"))')

scripts/publish-workspace.sh

git tag -s "v${version}" -m "lean-rs v${version}"
git push origin "v${version}"
gh release create "v${version}" \
  --notes-file <(awk -v ver="$version" '$0 ~ "^## \\[" ver "\\]" {f=1;next} f&&/^## \\[/{exit} f' CHANGELOG.md)
```

Prerequisite: `cargo login` once with the same scoped publish token.

If the script fails partway—**stop**, then simply re-run `scripts/publish-workspace.sh`: it skips the crates already on
crates.io and resumes with the rest (the same idempotence the CI job relies on). Do not use `--allow-dirty` or attempt
to overwrite a published version; crates.io versions are immutable. Only a genuine *content* change (a build break, not
an index race) requires bumping the patch version. `scripts/publish-workspace.sh --dry-run` needs no credentials,
subject to the interdependency caveat above.
