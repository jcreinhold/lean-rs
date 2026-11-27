# Release checklist

[`.github/workflows/release.yml`](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/release.yml)
publishes all ten crates when a `v<semver>` tag is pushed. It verifies the head Lean version on Ubuntu and macOS, checks
public API baselines, packages and publishes crates in dependency order, and creates a GitHub Release from the matching
`## [<version>]` changelog section.

The [version matrix](version-matrix.md) owns support claims; use the [bump procedure](bump-toolchain.md) when changing
that window. A release that needs full-window evidence also needs CI's manual `full_matrix: true` run: the release
verification job tests only the head.

## Prepare the release

1. Run the [workspace gates](https://github.com/jcreinhold/lean-rs/blob/main/CONTRIBUTING.md). Release verification also
   runs ABI fuzz smoke, workflow linting, docs.rs-mode documentation, and the package-tarball simulation. The workflow
   is the exact gate list.
2. Run the clean-tree package check and inspect its results:

   ```sh
   uv run scripts/check_package_docsrs.py
   cargo package --workspace --no-verify
   ```

   The Python check packages the crates, verifies crate/template contents, then builds docs from unpacked packages with
   `DOCS_RS=1` and Lean tools hidden from `PATH`.
3. Choose `X.Y.Z`: while the crates are 0.x, a breaking change bumps the minor version, anything else the patch
   version. Move the unreleased changelog entries into `## [X.Y.Z]`. The heading must match tag `vX.Y.Z` exactly.
4. Set `[workspace.package].version` and the workspace dependency versions in `Cargo.toml` to `X.Y.Z`. A minor bump
   also changes the `"0.Y"` dependency examples in `README.md` and the crate READMEs.
5. Regenerate intentionally changed public API baselines using the pinned tools and procedure in
   [api-review.md](api-review.md). The release workflow compares the same eight baseline files; `lean-rs-abi` and
   `lean-rs-writer` are published outside that loop.
6. Merge the release changes after the required checks pass.

The repository must have a scoped crates.io publish token in the `CARGO_REGISTRY_TOKEN` Actions secret. Local publishing
uses the same credential through `cargo login`. Keep credential setup outside release commits.

## Test matched sources before pushing

1. Use the current SDK checkout and run its own required checks with `scripts/prerelease.sh`. The SDK suite must not
   depend on a downstream repo. For a coordinated change, also check the affected consumer using the exact local SDK
   sources before pushing either repo. Test the real affected operation, not just Cargo resolution or compilation.
2. In the consumer, use Cargo's command-line `--config` with a temporary `[patch.crates-io]` table. Point each affected
   crate to its directory in the SDK checkout. The local version must satisfy the consumer's version requirement; if it
   does not, temporarily align those pins and resolve the candidate lockfile. Confirm with
   `cargo metadata --format-version 1` that the intended packages resolve to local manifests and that matched worker
   crates do not come from different SDK versions. Do not commit local paths or a lockfile resolved against local
   packages.
3. Once the agreed local acceptance checks pass, remove the overrides and publish one batch in dependency order. Resolve
   the consumer lockfile against the published dependency and run the required release checks. A check that needs a
   hosted runner belongs on the final candidate, not on each intermediate SDK change.

From a consumer checkout with the SDK at `../lean-rs`, set `consumer_package` and `ownership_test` to the package and
integration test that exercise the changed operation:

```sh
sdk=$(git -C ../lean-rs rev-parse --show-toplevel)
cargo --config "patch.crates-io.lean-toolchain.path='$sdk/crates/lean-toolchain'" metadata --format-version 1
cargo --config "patch.crates-io.lean-toolchain.path='$sdk/crates/lean-toolchain'" test -p "$consumer_package" --test "$ownership_test"
```

This example patches one crate. When a change crosses the worker boundary, include each matching worker crate in the
same override. Use the consumer's installed-worker test for changes to installation or process ownership. For example,
lean-host-mcp is a separate consumer repository with such a test: in its checkout,
`LEAN_RS_CHECKOUT="$sdk" scripts/test-installed-stdio.sh --all-targets` builds the writer, shims, worker and host from
the SDK checkout, restores the host lockfile on exit, and fails if Cargo kept a registry SDK crate. Review the
consumer's manifest and lockfile for local changes before staging the release.

## Local fixture setup

Run `scripts/prerelease.sh` from the checkout to build the existing Lake packages and run the local gates. For Lean
4.36, it uses the same `scripts/select_test_fixtures.py` checks as normal CI and release verification. The selector
binds the scalar and callback images to their canonical paths, file identities and hashes. Selections stay in memory
locally; the script restores temporary fixture directory modes on exit. Keep this checkout and its installed toolchain
unchanged until the script finishes: do not run a competing build or clean. This is trusted job ownership, not
protection against arbitrary same-user writes. The opt-in covers only ordinary scalar/callback tests; shared-reader and
custody tests keep their real guards.

On macOS the prerelease script gives workspace Nextest a short, private socket directory under `target` and removes it
when the tests exit. An export directly below `target` uses that outer directory to keep Unix socket paths short. For a
longer checkout path, set `LEAN_RS_TEST_IPC_BASE` to a short, canonical directory you own with mode `0700`.

## Interop checks

For changes to callback, progress, or worker streaming contracts, run these targeted checks in addition to the workspace
gates:

```sh
cargo nextest run -p lean-rs --test callback_trampoline --test callback_registry --test string_callback_abi
cargo nextest run -p lean-rs-host --test progress
cargo nextest run -p lean-rs-worker-child --test streaming_runner --test typed_command
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example string_streaming
cargo build -p lean-rs-host --example progress
cargo run -p lean-rs-worker-child --example worker_capability_runner
```

The progress example accepts an application-admitted session. Its build checks the example API; the host progress test
above exercises runtime callbacks. The executable examples need their artifact-writer prerequisites; see the
[interop](recipes/downstream-interop.md) and [worker](recipes/worker-capability-runner.md) recipes. For
performance-sensitive changes, compare saved baselines on the same toolchain and machine:

```sh
cargo bench -p lean-rs-host --bench session -- host::session::declaration_kind_bulk_vs_loop/bulk_5000 --baseline <saved-baseline>
cargo bench -p lean-rs-worker-child --bench row_payload -- --baseline <saved-baseline>
cargo bench -p lean-rs-worker-child --bench worker_capability -- --sample-size 10
```

The worker-capability benchmark is an operational measurement, not a saved-baseline comparison. Record the command,
revision, workload, and results using the [performance guide](performance.md). Sanitizer coverage is narrower: the
current workflow runs Miri on pure-Rust sys helpers and ASan on Rust-only toolchain helpers; it does not instrument the
Lean runtime or establish callback/worker safety by itself.

## Optional dry run

Dispatch the Release workflow with `dry_run: true` (the default). It runs verification, API checks, and
[`scripts/publish-workspace.sh --dry-run`](https://github.com/jcreinhold/lean-rs/blob/main/scripts/publish-workspace.sh)
without uploading crates or creating a GitHub Release. Manual runs of Release cannot upload crates; use the tag for a
live release.

Cargo builds all ten publishable tarballs from a temporary registry. Sibling crates do not need to be published for this
check, but external dependencies still have to resolve. A real release runs the same check before its first upload, then
publishes crates in dependency order.

## Tag and watch

After the release commit is on `main`, create and push an annotated tag; sign it if signing is configured:

```sh
git tag -s vX.Y.Z -m "lean-rs vX.Y.Z"
git push origin vX.Y.Z
gh run list --workflow release.yml
gh run watch <run-id>
```

Use `git tag -a` for an unsigned annotated tag. The workflow checks tag/version agreement before publishing. It marks
versions containing `-` as prereleases and extracts the matching changelog section for the release body. Release and
recovery runs do not run at the same time. Both check the changelog before uploading crates.

## Recover a partial upload

[`scripts/publish-workspace.sh`](https://github.com/jcreinhold/lean-rs/blob/main/scripts/publish-workspace.sh) skips
versions already present on crates.io and uploads only missing crates, one crate at a time. Re-run the failed job after
resolving an index-propagation or runner failure; that resumes the same release without a version bump.

For a separate publish-only run, dispatch
[Release recovery](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/release-recover.yml) at the exact
release commit with its version, without the `v` prefix. It uses the same publish script and ensures the GitHub Release
exists. A live recovery requires the existing release tag to point to that commit. Recovery does not repeat the full
verification gates; use it only for contents already verified for that release. Recovery defaults to `dry_run: true`,
which builds the tarballs without uploading. Set it to `false` only when you intend to resume the upload.

For example, `gh workflow run release-recover.yml --ref vX.Y.Z -f version=X.Y.Z -f dry_run=true` checks all ten packages
at the original tag. The publish script checks that every publishable crate has the workspace version and uses the
committed lockfile for crates.io. If Cargo times out waiting for the index after an upload, check the registry for that
version before resuming. An indexing timeout can follow a successful upload.

Keep published crate versions and their release tags fixed. If any workspace crate reached crates.io, resume the partial
upload from the original commit, or use a new version for changed contents. Publish from a clean commit.

If a tagged build failed before publication, check every workspace crate at that version on crates.io and check GitHub
for a Release under the tag. When all are absent, correct the same version and dispatch the Release workflow with
`dry_run: true` on the exact corrected commit. After both hosted platforms and the package check pass, replace the
failed tag with a signed tag on that commit. Record the old and new commit IDs and the passing run in the release notes.

If CI is unavailable after the same release checks have passed locally, the local upload command is
`scripts/publish-workspace.sh`; it uses the same order and resume behavior. Record the verification and upload results
before creating the corresponding release. Local upload does not itself create a tag or GitHub Release.

## Verify delivery

Check every published crate at the intended version on crates.io and docs.rs. A docs.rs build failure that needs changed
contents requires a patch publish. Add a fresh `## [Unreleased]` heading for subsequent work.
