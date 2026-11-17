---
name: release-lean-rs
description: Prepare or cut a lean-rs release with workspace version, CHANGELOG, public-API review, release gates, tag, CI publishing, and partial-publish recovery. Use for releasing lean-rs, publishing its crates, or cutting a vX.Y.Z tag.
---

# Release lean-rs

Follow [docs/release.md](../../../docs/release.md) for preparation, gates, tagging, publishing, and recovery. Use
[docs/api-review.md](../../../docs/api-review.md) for intentional API changes and its pinned regeneration command. These
documents own the procedure and crate lists.

Publishing normally runs through CI. Do not run `cargo publish` locally, use `--allow-dirty`, or bypass a failed gate.
The only local-publish exception is the documented CI-unavailable fallback.

Prepare and verify the version, CHANGELOG, and any intentional API-baseline changes before requesting approval. The
workspace version, internal dependency versions, release heading, and tag must agree. Report the exact proposed tag,
commit, and gate results, then obtain explicit human confirmation before pushing the release tag.

For a partial publish with unchanged contents, use the documented idempotent retry or recovery workflow at the same
version. If published contents need to change, prepare a new patch version; crates.io versions are immutable. Report
publishing and docs.rs results, and restore the Unreleased section after completion.
