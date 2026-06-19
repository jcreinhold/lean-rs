#!/usr/bin/env bash
# Verify that every entry in REQUIRED_SYMBOLS resolves in a candidate
# release's libleanshared.
#
# Usage: check-lean-symbols.sh <candidate-version>
# Example: check-lean-symbols.sh 4.30.0
#
# Empty output = every required symbol resolves. Non-empty output =
# a symbol disappeared upstream; add it to the entry's
# missing_symbols list (and update consumer call sites to tolerate
# absence), or file an upstream issue and stop.

set -euo pipefail

if [ "$#" -ne 1 ]; then
	echo "usage: $0 <candidate-version>" >&2
	exit 2
fi

CANDIDATE=$1
REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
TOOLCHAIN_ROOT="${ELAN_HOME:-$HOME/.elan}/toolchains"
LIB_DIR="$TOOLCHAIN_ROOT/leanprover--lean4---v$CANDIDATE/lib/lean"

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

nm -gU "$LIB" | awk '{print $NF}' | sed 's/^_//' | grep -E '^lean_' |
	sort -u >"$PRESENT"

comm -23 "$REQUIRED" "$PRESENT"
