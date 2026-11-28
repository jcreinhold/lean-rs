---
name: release-lean-rs
description: Prepare or cut a lean-rs release, covering the version, CHANGELOG, public-API files, release gates, checks against matched local sources, the tag, CI publishing and recovery from a partial upload. Use for releasing lean-rs, publishing its crates, or cutting a vX.Y.Z tag.
---

# Release lean-rs

`.github/workflows/release.yml` publishes all eight crates when a `v<semver>` tag is pushed. It tests the newest Lean
release on Ubuntu and macOS, diffs the public API, packages and publishes the crates in dependency order, and creates a
GitHub Release from the matching `## [<version>]` section of `CHANGELOG.md`. It tests only the newest Lean release; a
release that claims the whole window also needs a CI run started with `full_matrix: true`.

Publishing runs through CI. Do not run `cargo publish` locally, use `--allow-dirty`, or work around a failed gate; the
one exception is the fallback for when CI is down, under "Recover a partial upload". Report the proposed tag, commit and
gate results, and get explicit human confirmation before you push a release tag.

## Prepare

1. Run `scripts/prerelease.sh` on the release commit, then the package check, and read their output:

   ```sh
   uv run scripts/check_package_docsrs.py
   cargo package --workspace --no-verify
   ```

   The Python check packages each crate, checks crate and template contents, and builds docs from the unpacked
   packages with `DOCS_RS=1` and no Lean on `PATH`. `release.yml` holds the exact gate list.
2. Choose `X.Y.Z`. While the crates are 0.x, a breaking change bumps the minor version and anything else the patch
   version. A crate README change needs a release too, since crates.io shows the README packaged with the crate.
3. Move the `## [Unreleased]` entries in `CHANGELOG.md` under `## [X.Y.Z]`. The heading must match the tag `vX.Y.Z`
   exactly.
4. Set `[workspace.package].version` and the workspace dependency versions in `Cargo.toml` to `X.Y.Z`. A minor bump also
   changes the `"0.Y"` dependency lines in `README.md` and the crate READMEs.
5. Regenerate the public-API files an intended change moved, as `AGENTS.md` describes. `release.yml` diffs the same
   seven files.
6. Merge after the required checks pass.

The `CARGO_REGISTRY_TOKEN` Actions secret holds a scoped crates.io publish token. Keep credential setup out of release
commits.

## Test matched sources before pushing

For a change that spans repositories, test each affected downstream crate against the exact local SDK sources before you
push either repository. Test the operation that changed, not just that Cargo resolves and compiles. The SDK's own checks
must not depend on a downstream repository.

From the downstream checkout, with the SDK at `../lean-rs`, patch each affected crate on the command line. Set `package`
and `test` to the package and integration test that exercise the change:

```sh
sdk=$(git -C ../lean-rs rev-parse --show-toplevel)
cargo --config "patch.crates-io.lean-toolchain.path='$sdk/crates/lean-toolchain'" metadata --format-version 1
cargo --config "patch.crates-io.lean-toolchain.path='$sdk/crates/lean-toolchain'" test -p "$package" --test "$test"
```

- Repeat `--config` for each affected crate; a change that crosses the worker boundary patches each worker crate.
- The local version must satisfy the downstream version requirement. If it does not, align the requirement for the test
  and resolve a trial lockfile.
- Confirm with `cargo metadata` that each patched package resolves to a local manifest and that the worker crates come
  from one SDK version.
- Never commit local paths or a lockfile resolved against local packages. Check the downstream manifest and lockfile
  before you stage anything.

Once the agreed checks pass, drop the overrides and publish one batch in dependency order. Then resolve the downstream
lockfile against the published crates and run its release checks. A check that needs a hosted runner runs on the final
candidate, not on each SDK change.

## Dry run

Start the Release workflow by hand with `dry_run: true`, the default. It runs the checks, the API diff and
`scripts/publish-workspace.sh --dry-run`, which builds all eight tarballs against a temporary registry; sibling crates
need not be published, but outside dependencies must resolve. It uploads nothing and creates no GitHub Release. A run
started by hand can never upload, so a live release needs the tag.

## Tag and watch

With the release commit on `main`, and after the human confirms, create and push a signed, annotated tag:

```sh
git tag -s vX.Y.Z -m "lean-rs vX.Y.Z"
git push origin vX.Y.Z
gh run list --workflow release.yml
gh run watch <run-id>
```

Use `git tag -a` where signing is not set up. The workflow checks that the tag matches the version before it publishes,
marks a version that contains `-` as a prerelease, and takes the release notes from the changelog. Release and recovery
runs never overlap, and both check the changelog before they upload.

## Recover a partial upload

`scripts/publish-workspace.sh` skips versions already on crates.io and uploads the rest one crate at a time. After an
index delay or a runner failure, re-run the failed job; it resumes the same release at the same version.

For a publish-only run, start Release recovery at the release commit with the version, without the `v`:

```sh
gh workflow run release-recover.yml --ref vX.Y.Z -f version=X.Y.Z -f dry_run=true
```

It runs the same publish script and makes sure the GitHub Release exists, but it does not repeat the checks, so use it
only for contents already checked for that release. A live run (`dry_run=false`) requires the release tag to point at
that commit. If Cargo times out waiting for the index after an upload, check crates.io for that version before you
resume: the upload may have succeeded.

Published versions and their tags never change. If any crate reached crates.io, resume from the original commit, or
release changed contents as a new version.

If a tagged build failed before anything was published, check crates.io for every crate at that version and GitHub for a
Release under the tag. If all are absent, fix the same version and run the Release workflow with `dry_run: true` on the
fixed commit. Once both platforms and the package check pass, replace the failed tag with a signed tag on that commit,
and record the old and new commits and the passing run in the release notes.

If CI is down after the same checks passed locally, `scripts/publish-workspace.sh` uploads from a clean checkout in the
same order and resumes the same way, with the token from `cargo login`. Record the checks and the upload result before
you create the release; the script creates no tag or GitHub Release.

## After publishing

Check each crate at the new version on crates.io and docs.rs. A docs.rs failure that needs changed contents needs a
patch release. Report the publish and docs.rs results, and add a fresh `## [Unreleased]` heading to `CHANGELOG.md`.
