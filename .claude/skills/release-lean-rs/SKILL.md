---
name: release-lean-rs
description: Cut a lean-rs release (version bump, CHANGELOG, tag) that publishes via CI. Use when releasing lean-rs, publishing the workspace crates, bumping the workspace version for a release, or cutting a vX.Y.Z tag.
---

# Release lean-rs

[`docs/release.md`](../../../docs/release.md) is the source of truth — follow it. This skill is
the checklist plus the cross-file invariants CI only catches *after* you tag, when it is too
late: crates.io versions are **immutable**, so a botched publish burns a version permanently.

**Publishing happens only in CI.** Pushing a `vX.Y.Z` git tag fires
`.github/workflows/release.yml`, which runs the gate set, the public-API diff, and
`cargo publish --workspace`, then opens the GitHub Release. NEVER run `cargo publish` locally,
NEVER use `--allow-dirty`, and do not propose a local-publish plan — the only exception is the
documented "Fallback—local publish when CI is unavailable" section of `docs/release.md`, and
only when CI is genuinely broken.

## Steps

Do the reversible prep (1–4) freely. Step 5 (tag push) is irreversible — **stop and get
explicit human confirmation before running it.**

### 1. Pre-flight gate

```sh
scripts/prerelease.sh            # mirrors release.yml's verify job; --quick to skip fuzz + public-api
```

Stop on any failure. This is the same gate CI runs; passing locally is the fast feedback loop.

### 2. Version bump (one source of truth, two places)

Pick the new `X.Y.Z` (patch unless the change is breaking/feature — it is pre-1.0, so breaking
changes bump the minor). In the root `Cargo.toml`, set **both**:

- `[workspace.package].version = "X.Y.Z"`
- every `[workspace.dependencies]` entry's `version = "X.Y.Z"` (all 7 crates share the version)

The release workflow asserts `"v${workspace.package.version}" == "${GITHUB_REF_NAME}"` before
any publish — a half-updated version fails the run.

### 3. CHANGELOG

Move the `## [Unreleased]` entries into a new `## [X.Y.Z]` section (compose fresh if empty).
The heading text must match the tag **exactly**: tag `v0.1.17` → heading `## [0.1.17]`. The
workflow extracts that section verbatim as the GitHub Release body and fails if it is missing.

### 4. Public-API baselines (only if the public API changed intentionally)

```sh
for c in lean-rs-sys lean-toolchain lean-rs lean-rs-host lean-rs-worker-protocol lean-rs-worker-parent lean-rs-worker-child; do
  cargo public-api -p "$c" --simplified > "docs/api-review/${c}-public.txt"
done
```

Commit the regenerated baselines **in the same commit** as the version + CHANGELOG. The
workflow diffs committed baselines and fails on any drift before publishing. Review the
diff against the red-flag checklist in [`docs/api-review.md`](../../../docs/api-review.md).

### 5. PR, merge, then tag — invariants gate

Open a PR with the version + CHANGELOG + (if any) baseline changes; merge after `ci.yml` is
green. Before tagging, re-verify the three match-exactly invariants on the merge commit:

- `git rev-parse --abbrev-ref HEAD` is `main` and up to date.
- `grep '^version' Cargo.toml` (workspace.package) matches the intended `X.Y.Z`, and the
  `[workspace.dependencies]` versions match it.
- `CHANGELOG.md` has a `## [X.Y.Z]` heading.

**Confirm with the human, then push the tag** (this is the irreversible step):

```sh
git tag -s vX.Y.Z -m "lean-rs vX.Y.Z"   # -s signed (preferred), or -a unsigned annotated
git push origin vX.Y.Z
gh run watch --workflow=release.yml
```

Tags containing `-` (e.g. `vX.Y.Z-rc.1`) are auto-marked prerelease.

### 6. Post-publish

- `cargo search lean-rs` — all 7 crates show the new version.
- Within ~10 min, confirm `https://docs.rs/lean-rs/X.Y.Z` (and the other 6) built; a docs.rs
  failure is recoverable only by a patch publish with the fix.
- Add a fresh `## [Unreleased]` heading to the top of `CHANGELOG.md`.

## When publish fails mid-run

If the workflow fails **after** a crate has published, do **not** retry the same version —
crates.io versions are immutable. Bump the patch version, repeat steps 2–5, and re-tag at the
new merge commit. See `docs/release.md` for the full recovery and the CI-unavailable fallback.
