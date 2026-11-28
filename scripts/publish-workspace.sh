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
#   Run from the workspace root. Every publishable crate must have the workspace version. crates.io
#   versions are immutable, so this only ever fills gaps, never overwrites.
# Cargo verifies all publishable tarballs together in a temporary registry.
# Unpublished workspace dependencies resolve there. --dry-run stops after
# verification and never needs a publish token.
set -euo pipefail

dry_run=false
case "$#:${1:-}" in
  1:--dry-run) dry_run=true ;;
  0:) ;;
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
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
publishable=$(cargo metadata --locked --no-deps --format-version 1 \
  | python3 -c 'import json,sys
m = json.load(sys.stdin)
# publish == [] means publish = false (workspace-internal); anything else
# (null = default, or an explicit registry list) is publishable.
packages = [p for p in m["packages"] if p.get("publish") != []]
mismatches = [p for p in packages if p["version"] != sys.argv[1]]
if mismatches:
    sys.exit("Expected workspace version " + sys.argv[1] + ", found " + ", ".join(
        p["name"] + "@" + p["version"] for p in mismatches))
print(" ".join(sorted(p["name"] for p in packages)))' "$version")
expected=$(printf '%s\n' "${order[@]}" | sort | tr '\n' ' ' | sed 's/ $//')
if [ "$publishable" != "$expected" ]; then
  echo "::error::publish order list is stale. Publishable members: [$publishable]; scripts/publish-workspace.sh lists: [$expected]. Reconcile the two." >&2
  exit 1
fi

# Select only the packages we ship, not internal workspace members.
packages=()
for c in "${order[@]}"; do
  packages+=(-p "$c")
done
cargo package --registry crates-io --locked "${packages[@]}"
if "$dry_run"; then
  exit 0
fi

echo "Publishing workspace at version $version"
published=0
skipped=0
for c in "${order[@]}"; do
  status=$(curl --silent --show-error --retry 2 --max-time 30 --output /dev/null --write-out '%{http_code}' \
    -H "User-Agent: lean-rs-release (github.com/jcreinhold/lean-rs)" \
    "https://crates.io/api/v1/crates/$c/$version") || {
      echo "Could not check $c@$version on crates.io; no upload attempted for that crate." >&2
      exit 1
    }
  case "$status" in
    200)
      echo "$c@$version is already published; skipping"
      skipped=$((skipped + 1))
      continue ;;
    404) ;;
    *)
      echo "Unexpected crates.io response HTTP $status for $c@$version; no upload attempted for that crate." >&2
      exit 1 ;;
  esac
  cargo publish --registry crates-io --locked -p "$c"
  published=$((published + 1))
done
echo "Publish summary: published=$published skipped=$skipped"
if [ "$published" = "0" ]; then
  echo "Nothing to publish — $version was already complete on crates.io."
fi
