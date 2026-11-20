#!/usr/bin/env bash
# Verify that every entry in REQUIRED_SYMBOLS resolves in a candidate
# release's libleanshared.
#
# Usage: check-lean-symbols.sh <candidate-version>
# Example: check-lean-symbols.sh 4.30.0
#
# Empty output and exit status 0 = every required symbol resolves.
# Missing symbols are printed and exit status is 1. Stop and resolve the
# ABI change before extending the supported window; see
# docs/bump-toolchain.md. Do not add missing-symbol exceptions.

set -euo pipefail

if [ "$#" -ne 1 ]; then
	echo "usage: $0 <candidate-version>" >&2
	exit 2
fi

CANDIDATE=$1
REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
prefix=$("$REPO_ROOT/scripts/lean-release-prefix.sh" "$CANDIDATE")
LIB_DIR="$prefix/lib/lean"

case "$(uname -s)" in
Darwin) LIB="$LIB_DIR/libleanshared.dylib" ;;
Linux) LIB="$LIB_DIR/libleanshared.so" ;;
*)
	echo "unsupported OS: $(uname -s)" >&2
	exit 2
	;;
esac

REQUIRED=$(mktemp)
PRESENT=$(mktemp)
trap 'rm -f "$REQUIRED" "$PRESENT"' EXIT

awk '/^pub const REQUIRED_SYMBOLS/,/^];/' \
	"$REPO_ROOT/crates/lean-rs-abi/src/symbols.rs" |
	grep -oE '"lean_[a-z0-9_]+"' | tr -d '"' | sort -u >"$REQUIRED"

if [ ! -s "$REQUIRED" ]; then
	echo "no required symbols were extracted from crates/lean-rs-abi/src/symbols.rs" >&2
	exit 2
fi

nm -gU "$LIB" | awk '{print $NF}' | sed 's/^_//' | grep -E '^lean_' |
	sort -u >"$PRESENT"

MISSING=$(comm -23 "$REQUIRED" "$PRESENT")
if [ -n "$MISSING" ]; then
	printf '%s\n' "$MISSING"
	exit 1
fi
