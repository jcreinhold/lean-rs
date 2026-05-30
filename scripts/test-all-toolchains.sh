#!/usr/bin/env bash
# scripts/test-all-toolchains.sh — run the workspace test suite against
# every Lean release in the supported window (or a subset you name).
#
# Usage:
#   scripts/test-all-toolchains.sh                  # sweep every manifest version
#   scripts/test-all-toolchains.sh 4.31.0-rc1       # sweep only the named version(s)
#   scripts/test-all-toolchains.sh 4.30.0 4.31.0-rc1
#   scripts/test-all-toolchains.sh -h
#
# For each version (read from `crates/lean-rs-sys/digests/manifest.json`,
# or taken from the command line):
#   1. Repoint every tracked `lean-toolchain` pin to that version so `lean`
#      resolves to it.
#   2. Clean-rebuild the load-bearing Lake packages, and wipe the shipped-crate
#      template so the worker tests rebuild it lazily against the new toolchain.
#   3. `cargo clean` (so `lean-rs-sys`'s build.rs re-runs against the new
#      header) and `cargo nextest run --workspace --no-fail-fast`.
#
# Exit code = number of versions that failed. Per-version pass/fail is printed
# at the end. CI's matrix achieves the same coverage by sharding one cell per
# version, which is faster because it parallelises the rebuilds.
#
# Pin safety: the pins are restored to their pre-sweep working-tree content on
# exit — including on Ctrl-C, `kill` (SIGINT/SIGTERM/SIGHUP), and a normal
# error. A snapshot of the original pins is kept under `.git/` (outside the work
# tree), so a hard kill that no trap can catch — SIGKILL, an OOM kill (Lean's
# cumulative runtime state can OOM a long sweep), a power loss — is recovered
# automatically: the next run detects the leftover snapshot and restores from it
# before starting. The pins are git-tracked, so `git restore` is always the
# manual fallback.

set -euo pipefail

usage() {
	sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
	exit "${1:-0}"
}

case "${1:-}" in
-h | --help) usage 0 ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$REPO_ROOT/crates/lean-rs-sys/digests/manifest.json"
GIT_DIR="$(git -C "$REPO_ROOT" rev-parse --git-dir)"
case "$GIT_DIR" in /*) ;; *) GIT_DIR="$REPO_ROOT/$GIT_DIR" ;; esac
SNAPSHOT_DIR="$GIT_DIR/test-all-toolchains.snapshot"

# Lake packages the in-workspace test suite builds and the host loader opens at
# runtime. These are the load-bearing copies — what CI builds and what the
# runtime `dlopen`s. (`lake/lean-rs-*-shims` are the *published* consumer copies
# that downstream projects `require` over git; they are repointed for pin
# consistency below but not rebuilt here, because the workspace suite never
# loads them.)
LAKE_PACKAGES=(
	"crates/lean-rs/shims/lean-rs-interop-shims"
	"crates/lean-rs-host/shims/lean-rs-interop-shims"
	"crates/lean-rs-host/shims/lean-rs-host-shims"
	"fixtures/lean"
	"fixtures/interop-shims"
)

# The downstream-shipped-crate template exercised by the `lean-rs-worker-child`
# loader-regression tests. Its `.lake/` and `target/` are rebuilt lazily by the
# worker tests (cargo build → build.rs → lake), so we only wipe them; wiping
# `target/` busts cargo's incremental fingerprint so the embedded toolchain
# digest does not stick at the previous iteration's version.
TEMPLATE_DIR="$REPO_ROOT/templates/shipped-lean-crate/lean"

# Every tracked `lean-toolchain` pin, discovered from git so the set cannot
# drift out of sync with the repository (new shim packages are covered with no
# edit here). Stored as repo-relative paths.
declare -a PINS=()
while IFS= read -r -d '' f; do
	[ "$(basename "$f")" = "lean-toolchain" ] && PINS+=("$f")
done < <(git -C "$REPO_ROOT" ls-files -z)

snapshot_pins() {
	mkdir -p "$SNAPSHOT_DIR"
	local rel
	for rel in "${PINS[@]}"; do
		mkdir -p "$SNAPSHOT_DIR/$(dirname "$rel")"
		cp "$REPO_ROOT/$rel" "$SNAPSHOT_DIR/$rel"
	done
}

restore_pins() {
	[ -d "$SNAPSHOT_DIR" ] || return 0
	local rel
	for rel in "${PINS[@]}"; do
		[ -e "$SNAPSHOT_DIR/$rel" ] && cp "$SNAPSHOT_DIR/$rel" "$REPO_ROOT/$rel"
	done
}

# Invoked via `trap cleanup EXIT`; shellcheck cannot see the dispatch.
# shellcheck disable=SC2329
cleanup() {
	# Tolerate failures here so a restore error cannot mask the real exit code.
	restore_pins || true
	rm -rf "$SNAPSHOT_DIR" || true
}

# Self-heal: a leftover snapshot means a previous run died before it could
# restore (SIGKILL / OOM / crash). Recover the original pins from it, then
# start clean.
if [ -d "$SNAPSHOT_DIR" ]; then
	echo "note: found a snapshot from an interrupted run; restoring pins before starting" >&2
	restore_pins || true
	rm -rf "$SNAPSHOT_DIR"
fi

# Route signals through the EXIT trap: calling `exit` from a signal handler runs
# the EXIT trap, so `cleanup` restores the pins on Ctrl-C and `kill` too.
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

snapshot_pins

write_pins() {
	local version="$1" rel
	for rel in "${PINS[@]}"; do
		printf 'leanprover/lean4:v%s\n' "$version" >"$REPO_ROOT/$rel"
	done
}

rebuild_lake_packages() {
	local pkg
	for pkg in "${LAKE_PACKAGES[@]}"; do
		rm -rf "$REPO_ROOT/$pkg/.lake"
	done
	rm -rf "$TEMPLATE_DIR/.lake" "$REPO_ROOT/templates/shipped-lean-crate/target"
	for pkg in "${LAKE_PACKAGES[@]}"; do
		(cd "$REPO_ROOT/$pkg" && lake build >/dev/null)
	done
}

run_one_version() {
	local version="$1"
	printf '\n=== Lean %s ===\n' "$version"
	write_pins "$version"
	rebuild_lake_packages
	(cd "$REPO_ROOT" && cargo clean >/dev/null 2>&1 || true)
	LEAN_SYSROOT="$HOME/.elan/toolchains/leanprover--lean4---v${version}" \
		cargo nextest run --workspace --no-fail-fast
}

# Versions to sweep: the command-line list if given, else every version in the
# manifest. Manifest read via jq when present, else a python fallback.
list_manifest_versions() {
	if command -v jq >/dev/null 2>&1; then
		jq -r '.entries[].versions[]' "$MANIFEST"
	else
		python3 - "$MANIFEST" <<'PY'
import json, sys, pathlib
manifest = json.loads(pathlib.Path(sys.argv[1]).read_text())
for entry in manifest['entries']:
    for v in entry['versions']:
        print(v)
PY
	fi
}

declare -a VERSIONS=()
if [ "$#" -gt 0 ]; then
	VERSIONS=("$@")
else
	while IFS= read -r v; do
		[ -n "$v" ] && VERSIONS+=("$v")
	done < <(list_manifest_versions)
fi

declare -a FAILED=()
declare -a PASSED=()

for version in "${VERSIONS[@]}"; do
	sysroot="$HOME/.elan/toolchains/leanprover--lean4---v${version}"
	if [ ! -d "$sysroot" ]; then
		# Backticks here are literal — the hint renders as inline code to the reader.
		# shellcheck disable=SC2016
		printf '\n=== Lean %s SKIPPED (run `elan toolchain install leanprover/lean4:v%s`) ===\n' "$version" "$version"
		continue
	fi
	if run_one_version "$version"; then
		PASSED+=("$version")
	else
		FAILED+=("$version")
	fi
done

echo
echo '====== Summary ======'
printf 'passed: %s\n' "${PASSED[*]:-<none>}"
printf 'failed: %s\n' "${FAILED[*]:-<none>}"

exit "${#FAILED[@]}"
