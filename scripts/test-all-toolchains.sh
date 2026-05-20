#!/usr/bin/env bash
# scripts/test-all-toolchains.sh — run the workspace test suite against
# every Lean release in the supported window.
#
# For each version in `crates/lean-rs-sys/digests/manifest.json`:
#   1. Repoint the workspace `lean-toolchain` files (root +
#      `lake/lean-rs-interop-shims/` + `lake/lean-rs-host-shims/` +
#      `fixtures/lean/` + `fixtures/interop-shims/`) so `lean` resolves
#      to that toolchain.
#   2. Rebuild the Lake packages from a clean `.lake/` directory.
#   3. `cargo clean` the workspace (so `lean-rs-sys`'s build.rs re-runs
#      against the new header) and `cargo nextest run --workspace`.
#   4. Restore the original `lean-toolchain` files on exit.
#
# Failures print which versions failed; exit code is the count of
# failures. Designed to be run locally or by CI; CI's matrix achieves
# the same thing by sharding one cell per version, which is faster
# because it parallelises the rebuilds.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$REPO_ROOT/crates/lean-rs-sys/digests/manifest.json"

# Paths that need to agree on the active toolchain. The root file is
# the one `lean --print-prefix` (invoked by the shim's
# `Lean.findSysroot`) resolves against from the test process's cwd.
ROOT_TOOLCHAIN="$REPO_ROOT/lean-toolchain"
INTEROP_SHIM_TOOLCHAIN="$REPO_ROOT/lake/lean-rs-interop-shims/lean-toolchain"
SHIM_TOOLCHAIN="$REPO_ROOT/lake/lean-rs-host-shims/lean-toolchain"
FIXTURE_TOOLCHAIN="$REPO_ROOT/fixtures/lean/lean-toolchain"
INTEROP_FIXTURE_TOOLCHAIN="$REPO_ROOT/fixtures/interop-shims/lean-toolchain"

declare -a TOUCHED_FILES=()

backup_one() {
    local path="$1"
    if [ -e "$path" ]; then
        cp "$path" "$path.bak"
        TOUCHED_FILES+=("$path")
    fi
}

restore_all() {
    for path in "${TOUCHED_FILES[@]}"; do
        if [ -e "$path.bak" ]; then
            mv "$path.bak" "$path"
        fi
    done
    # If we created a root toolchain file that didn't exist before,
    # remove it so the original state is preserved.
    if [ -e "$ROOT_TOOLCHAIN" ] && ! grep -qE 'leanprover/lean4' "$ROOT_TOOLCHAIN" 2>/dev/null; then
        return
    fi
    if [ -f "$ROOT_TOOLCHAIN.bak" ]; then
        :
    elif [ -f "$ROOT_TOOLCHAIN" ] && [ ! " ${TOUCHED_FILES[*]} " =~ " $ROOT_TOOLCHAIN " ]; then
        rm -f "$ROOT_TOOLCHAIN"
    fi
}
trap restore_all EXIT

write_toolchain() {
    local path="$1" version="$2"
    printf 'leanprover/lean4:v%s\n' "$version" > "$path"
}

rebuild_lake_packages() {
    rm -rf "$REPO_ROOT/lake/lean-rs-interop-shims/.lake" \
        "$REPO_ROOT/lake/lean-rs-host-shims/.lake" \
        "$REPO_ROOT/fixtures/lean/.lake" \
        "$REPO_ROOT/fixtures/interop-shims/.lake"
    (cd "$REPO_ROOT/lake/lean-rs-interop-shims" && lake build >/dev/null)
    (cd "$REPO_ROOT/lake/lean-rs-host-shims" && lake build >/dev/null)
    (cd "$REPO_ROOT/fixtures/lean" && lake build >/dev/null)
    (cd "$REPO_ROOT/fixtures/interop-shims" && lake build >/dev/null)
}

run_one_version() {
    local version="$1"
    printf '\n=== Lean %s ===\n' "$version"

    write_toolchain "$ROOT_TOOLCHAIN" "$version"
    write_toolchain "$INTEROP_SHIM_TOOLCHAIN" "$version"
    write_toolchain "$SHIM_TOOLCHAIN" "$version"
    write_toolchain "$FIXTURE_TOOLCHAIN" "$version"
    write_toolchain "$INTEROP_FIXTURE_TOOLCHAIN" "$version"

    rebuild_lake_packages
    (cd "$REPO_ROOT" && cargo clean >/dev/null 2>&1 || true)
    LEAN_SYSROOT="$HOME/.elan/toolchains/leanprover--lean4---v${version}" \
        cargo nextest run --workspace --no-fail-fast
}

# Read versions out of the manifest. Use jq if available, else a
# minimal grep-based fallback that walks the "versions" arrays.
list_versions() {
    if command -v jq >/dev/null 2>&1; then
        jq -r '.entries[].versions[]' "$MANIFEST"
    else
        # Fallback: extract every quoted string in a `"versions": [...]`
        # array. Order is preserved.
        python3 - <<'PY'
import json, sys, pathlib
manifest = json.loads(pathlib.Path(sys.argv[1]).read_text())
for entry in manifest['entries']:
    for v in entry['versions']:
        print(v)
PY
    fi
}

backup_one "$ROOT_TOOLCHAIN"
backup_one "$INTEROP_SHIM_TOOLCHAIN"
backup_one "$SHIM_TOOLCHAIN"
backup_one "$FIXTURE_TOOLCHAIN"
backup_one "$INTEROP_FIXTURE_TOOLCHAIN"

declare -a FAILED=()
declare -a PASSED=()

while IFS= read -r version; do
    [ -z "$version" ] && continue
    sysroot="$HOME/.elan/toolchains/leanprover--lean4---v${version}"
    if [ ! -d "$sysroot" ]; then
        printf '\n=== Lean %s SKIPPED (run `elan toolchain install leanprover/lean4:v%s`) ===\n' "$version" "$version"
        continue
    fi
    if run_one_version "$version"; then
        PASSED+=("$version")
    else
        FAILED+=("$version")
    fi
done < <(list_versions "$MANIFEST" 2>/dev/null || echo "$MANIFEST: cannot list versions")

echo
echo '====== Summary ======'
printf 'passed: %s\n' "${PASSED[*]:-<none>}"
printf 'failed: %s\n' "${FAILED[*]:-<none>}"

exit "${#FAILED[@]}"
