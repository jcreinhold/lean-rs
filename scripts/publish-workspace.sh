#!/usr/bin/env bash
# Publish every publishable workspace crate to crates.io, idempotently, in
# dependency order. Shared by `release.yml` (tag-driven) and `release-recover.yml`
# (manual gap-fill) so the publish algorithm — and the crate ORDER — live in
# exactly one place.
#
# Why not `cargo publish --workspace`? Its scheduler imposes a single global
# deadline on index propagation: when one crate's upload has not appeared in the
# index within the wait window, the whole plan aborts ("no packages ready to
# publish but N packages remain in plan ... 1 awaiting confirmation"), stranding
# every crate that had not started yet. Re-running then fails outright, because
# the crates that DID upload now collide ("crate <name>@<ver> already exists").
# That all-or-nothing behaviour is why a partial release used to need a separate
# recovery workflow on every run.
#
# This loop has no global deadline. Each crate is published with its own
# `cargo publish -p`, which waits only for ITS OWN upload to land in the index
# before returning; a crate already on crates.io is skipped. So a partial run
# completes cleanly when the job is re-run — no version is burned, no separate
# recovery is required.
#
# Usage: scripts/publish-workspace.sh [--dry-run]
#   The version is the workspace version (read from `cargo metadata`). crates.io
#   versions are immutable, so this only ever fills gaps, never overwrites.
#   --dry-run runs each missing crate's verify build without uploading. Note:
#   with a chain of unpublished interdependent crates, a dry run of a downstream
#   crate cannot resolve its not-yet-uploaded workspace dependency from the
#   index and will fail — that failure is a dry-run artifact, not a real defect.
set -euo pipefail

dry_run=false
case "${1:-}" in
  --dry-run) dry_run=true ;;
  "") ;;
  *)
    echo "usage: $0 [--dry-run]" >&2
    exit 2
    ;;
esac

# Topological publish order: every crate appears after all workspace crates it
# depends on, so each `cargo publish` resolves its predecessors from the index.
order=(
  lean-rs-abi
  lean-rs-sys
  lean-toolchain
  lean-rs-interop-shims
  lean-rs
  lean-rs-host
  lean-rs-worker-protocol
  lean-rs-worker-parent
  lean-rs-worker-child
)

# Drift guard: the ordered list above must cover exactly the publishable
# workspace members. A crate added to the workspace (or flipped to publishable)
# but missing here would silently never publish — which is how
# lean-rs-interop-shims slipped through the 0.2.1 release. Compare the SET of
# names (order is asserted by the topological list, not by this check).
publishable=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys
m = json.load(sys.stdin)
# publish == [] means publish = false (workspace-internal); anything else
# (null = default, or an explicit registry list) is publishable.
names = [p["name"] for p in m["packages"] if p.get("publish") != []]
print(" ".join(sorted(names)))')
expected=$(printf '%s\n' "${order[@]}" | sort | tr '\n' ' ' | sed 's/ $//')
if [ "$publishable" != "$expected" ]; then
  echo "::error::publish order list is stale. Publishable members: [$publishable]; scripts/publish-workspace.sh lists: [$expected]. Reconcile the two." >&2
  exit 1
fi

version=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["version"] for p in m["packages"] if p["name"]=="lean-rs"))')

echo "Publishing workspace at version $version (dry_run=$dry_run)"
published=0
skipped=0
for c in "${order[@]}"; do
  # crates.io returns 200 for an existing version, 404 otherwise.
  if curl -sf -o /dev/null \
       -H "User-Agent: lean-rs-release (github.com/jcreinhold/lean-rs)" \
       "https://crates.io/api/v1/crates/$c/$version"; then
    echo "✓ $c@$version already on crates.io — skipping"
    skipped=$((skipped + 1))
    continue
  fi
  echo "→ $c@$version is missing — publishing"
  if [ "$dry_run" = "true" ]; then
    cargo publish -p "$c" --dry-run
  else
    # Single-crate publish waits for the upload to land in the index before
    # returning, so the next crate's verify build resolves it.
    cargo publish -p "$c"
  fi
  published=$((published + 1))
done
echo "Publish summary: published=$published skipped=$skipped (dry_run=$dry_run)"
if [ "$dry_run" != "true" ] && [ "$published" = "0" ]; then
  echo "Nothing to publish — $version was already complete on crates.io."
fi
