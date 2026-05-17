#!/usr/bin/env bash
# audit-module.sh — Dispatch a module audit to the right language backend.
#
# Usage:
#   bash .claude/skills/deep-module-design/scripts/audit-module.sh <path>
#
# Selection rules:
#   - File ending .rs, or directory containing Cargo.toml         -> audit-rust.sh
#   - File ending .lean, or directory containing lakefile.* / .lean files
#                                                                 -> audit-lean.sh
#   - Otherwise                                                   -> usage error
#
# Exit codes:
#   0 = clean (no issues found by the backend)
#   1 = issues found (details printed by the backend)
#   2 = usage error or unrecognized layout

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ $# -lt 1 ]; then
	echo "Usage: $0 <crate-or-module-or-lean-path>"
	echo ""
	echo "Examples:"
	echo "  $0 crates/kernel/core-ast"
	echo "  $0 crates/frontend/elaboration/src/resolve.rs"
	exit 2
fi

TARGET="$1"

is_rust() {
	if [ -f "$1" ]; then
		case "$1" in *.rs) return 0 ;; esac
		return 1
	fi
	if [ -d "$1" ]; then
		[ -f "$1/Cargo.toml" ] && return 0
		# Walk up one level for files inside a crate's src/
		[ -f "$1/../Cargo.toml" ] && return 0
		# Look for any *.rs at depth 2 as a final fallback
		find "$1" -maxdepth 2 -name '*.rs' -print -quit 2>/dev/null | grep -q . && return 0
	fi
	return 1
}

is_lean() {
	if [ -f "$1" ]; then
		case "$1" in *.lean) return 0 ;; esac
		return 1
	fi
	if [ -d "$1" ]; then
		ls "$1"/lakefile.* >/dev/null 2>&1 && return 0
		find "$1" -maxdepth 2 -name '*.lean' -print -quit 2>/dev/null | grep -q . && return 0
	fi
	return 1
}

if is_rust "$TARGET"; then
	exec bash "$SCRIPT_DIR/audit-rust.sh" "$TARGET"
fi

if is_lean "$TARGET"; then
	exec bash "$SCRIPT_DIR/audit-lean.sh" "$TARGET"
fi

echo "Error: $TARGET is not a recognized Rust or Lean target."
echo "Recognized layouts:"
echo "  - .rs file, or directory with Cargo.toml or *.rs files"
echo "  - .lean file, or directory with lakefile.* or *.lean files"
exit 2
