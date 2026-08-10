---
name: release-lean-rs
description: Cut a lean-rs release (version bump, CHANGELOG, tag) that publishes via CI. Use when releasing lean-rs, publishing the workspace crates, bumping the workspace version for a release, or cutting a vX.Y.Z tag.
---

# Release lean-rs

[`docs/release.md`](../../../docs/release.md) is the source of truth. This skill is the checklist plus the cross-file
invariants CI only catches *after* you tag — crates.io versions are **immutable**, so a botched publish burns a version
permanently.

**Publishing happens only in CI.** Pushing a `vX.Y.Z` tag fires `.github/workflows/release.yml`: gate set, public-API
diff, idempotent per-crate publish (`scripts/publish-workspace.sh`), GitHub Release. NEVER run `cargo publish` locally
or use `--allow-dirty`; the only exception is the CI-unavailable fallback in `docs/release.md`.

## Steps

Steps 1–4 are reversible. Step 5 (tag push) is irreversible — **stop and get explicit human confirmation first.**

### 1. Pre-flight gate

```sh
scripts/prerelease.sh            # mirrors release.yml's verify job; --quick skips fuzz + public-api
```

Stop on any failure; this is the same gate CI runs.

### 2. Version bump

Pick the new `X.Y.Z` (patch unless breaking/feature; pre-1.0, so breaking bumps minor). In the root `Cargo.toml`, set
**both**:

- `[workspace.package].version = "X.Y.Z"`
- every `[workspace.dependencies]` entry's `version = "X.Y.Z"` (all crates share the version)

The release workflow asserts `"v${workspace.package.version}" == "${GITHUB_REF_NAME}"` before publishing — a
half-updated version fails the run.

### 3. CHANGELOG

Move the `## [Unreleased]` entries into a new `## [X.Y.Z]` section (compose fresh if empty). The heading must match the
tag **exactly** (tag `v0.1.17` → heading `## [0.1.17]`); the workflow extracts that section verbatim as the GitHub
Release body.

### 4. Public-API baselines (only if the public API changed intentionally)

```sh
for c in lean-rs-sys lean-toolchain lean-rs lean-rs-host lean-rs-worker-protocol lean-rs-worker-parent lean-rs-worker-child; do
  cargo public-api -p "$c" --simplified > "docs/api-review/${c}-public.txt"
done
```

Commit the regenerated baselines together with the version + CHANGELOG. Review the diff against the red-flag checklist
in [`docs/api-review.md`](../../../docs/api-review.md).

### 5. Tag — invariants gate

Re-verify the three match-exactly invariants on the current commit:

- `grep '^version' Cargo.toml` matches the intended `X.Y.Z`, including the `[workspace.dependencies]` entries.
- `CHANGELOG.md` has a `## [X.Y.Z]` heading.
- CI is green on the commit you are tagging.

**Confirm with the human, then push the tag** (the irreversible step):

```sh
git tag -s vX.Y.Z -m "lean-rs vX.Y.Z"   # -s signed (preferred), or -a unsigned annotated
git push origin vX.Y.Z
gh run watch --workflow=release.yml
```

Tags containing `-` (e.g. `vX.Y.Z-rc.1`) are auto-marked prerelease.

### 6. Post-publish

- `cargo search lean-rs` — all crates show the new version.
- Within ~10 min, confirm `https://docs.rs/lean-rs/X.Y.Z` (and the other crates) built; a docs.rs failure is
  recoverable only by a patch publish with the fix.
- Add a fresh `## [Unreleased]` heading to the top of `CHANGELOG.md`.

## When publish fails mid-run

crates.io versions are immutable, so the fix depends on *why* it failed.

**Partial publish** (some crates uploaded, the rest did not — usually the index-propagation race, `... awaiting
confirmation`). Contents are fine; only the upload is incomplete, and the publish step is idempotent. Do **not** bump
the version. **Re-run the failed publish job** (Actions → the failed run → "Re-run failed jobs"): the script skips
crates already on crates.io. If re-running the tag job is undesirable, run **`release-recover.yml`** (Actions →
"Release recovery") with the same `version` — same idempotent script on a fresh checkout. Both are safe to re-run.

**Contents must change** — a genuine build break, not a race. Bump the patch version, repeat steps 2–5, and re-tag; the
already-published crates keep their old version.

See `docs/release.md` for the full recovery and the CI-unavailable fallback.
