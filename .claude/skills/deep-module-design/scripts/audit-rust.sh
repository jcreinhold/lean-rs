#!/usr/bin/env bash
# audit-rust.sh — Analyze a Rust module or crate for depth and surface area.
#
# Normally invoked via the language dispatcher
#   bash .claude/skills/deep-module-design/scripts/audit-module.sh <path>
# but can be run directly:
#
#   bash .claude/skills/deep-module-design/scripts/audit-rust.sh <path>
#   bash .claude/skills/deep-module-design/scripts/audit-rust.sh crates/kernel/core-ast
#   bash .claude/skills/deep-module-design/scripts/audit-rust.sh crates/frontend/elaboration/src/resolve.rs
#
# What it checks:
#   1. Public surface area (pub items) vs total items
#   2. Shallow wrappers (pub methods that just delegate)
#   3. Dead re-exports (pub use with no external callers)
#   4. Pass-through types (pub structs where all fields are also pub)
#   5. Temporal coupling (methods that assert ordering)
#
# Exit codes:
#   0 = clean (no issues found)
#   1 = issues found (details printed)
#   2 = usage error

set -euo pipefail

if [ $# -lt 1 ]; then
	echo "Usage: $0 <crate-or-module-path>"
	echo ""
	echo "Examples:"
	echo "  $0 crates/kernel/core-ast"
	echo "  $0 crates/frontend/elaboration/src/resolve.rs"
	exit 2
fi

TARGET="$1"
ISSUES_FOUND=0

# Resolve to directory
if [ -f "$TARGET" ]; then
	TARGET_DIR="$(dirname "$TARGET")"
	TARGET_FILES="$TARGET"
elif [ -d "$TARGET/src" ]; then
	TARGET_DIR="$TARGET/src"
	TARGET_FILES=""
elif [ -d "$TARGET" ]; then
	TARGET_DIR="$TARGET"
	TARGET_FILES=""
else
	echo "Error: $TARGET is not a file, crate, or directory"
	exit 2
fi

echo "=== Deep Module Audit (Rust): $1 ==="
echo ""

# --- 1. Public surface area ---
echo "## 1. Public Surface Area"
echo ""

if [ -n "${TARGET_FILES:-}" ]; then
	SEARCH_PATH="$TARGET_FILES"
else
	SEARCH_PATH="$TARGET_DIR"
fi

PUB_ITEMS=$(rg -c '^\s*pub\s+(fn|struct|enum|trait|type|const|static|mod)\s' "$SEARCH_PATH" --type rust 2>/dev/null | awk -F: '{sum += $NF} END {print sum+0}')
TOTAL_ITEMS=$(rg -c '^\s*(pub\s+)?(fn|struct|enum|trait|type|const|static|mod)\s' "$SEARCH_PATH" --type rust 2>/dev/null | awk -F: '{sum += $NF} END {print sum+0}')

if [ "$TOTAL_ITEMS" -gt 0 ]; then
	RATIO=$(echo "scale=1; $PUB_ITEMS * 100 / $TOTAL_ITEMS" | bc 2>/dev/null || echo "??")
	echo "  Public items: $PUB_ITEMS"
	echo "  Total items:  $TOTAL_ITEMS"
	echo "  Public ratio: ${RATIO}%"
	if [ "$PUB_ITEMS" -gt "$((TOTAL_ITEMS * 70 / 100))" ] 2>/dev/null; then
		echo "  WARNING: >70% public — module may be inside-out"
		ISSUES_FOUND=1
	fi
else
	echo "  No items found in $SEARCH_PATH"
fi
echo ""

# --- 2. Shallow wrappers (methods that just call self.inner.method()) ---
echo "## 2. Shallow Wrappers (delegating methods)"
echo ""

SHALLOW=$(rg -n 'pub\s+fn\s+\w+.*\{' "$SEARCH_PATH" --type rust -A 3 2>/dev/null \
	| rg 'self\.\w+\.\w+\(' 2>/dev/null | head -20 || true)

if [ -n "$SHALLOW" ]; then
	echo "  Possible delegation-only methods (verify manually):"
	echo "$SHALLOW" | sed 's/^/    /'
	ISSUES_FOUND=1
else
	echo "  None detected."
fi
echo ""

# --- 3. Dead re-exports ---
echo "## 3. Re-exports"
echo ""

REEXPORTS=$(rg -n '^\s*pub\s+use\s' "$SEARCH_PATH" --type rust 2>/dev/null || true)

if [ -n "$REEXPORTS" ]; then
	echo "  Found re-exports (check for external callers):"
	echo "$REEXPORTS" | sed 's/^/    /' | head -30
	echo ""
	echo "  To check if a re-export is used externally:"
	echo "    rg 'use.*<item_name>' --type rust | grep -v '$TARGET'"
else
	echo "  No re-exports."
fi
echo ""

# --- 4. Inside-out structs (all fields public) ---
echo "## 4. Inside-Out Structs (all fields public)"
echo ""

# Find pub structs and check if they have pub fields
INSIDE_OUT=$(rg -l 'pub\s+struct\s+\w+\s*\{' "$SEARCH_PATH" --type rust 2>/dev/null | while read -r file; do
	# For each file with pub structs, look for structs where all fields are pub
	rg -U 'pub\s+struct\s+\w+[^;]*\{[^}]*\}' "$file" 2>/dev/null \
		| rg -v 'pub\s+struct.*\(\)' 2>/dev/null \
		| head -10 || true
done)

if [ -n "$INSIDE_OUT" ]; then
	echo "  Structs with public fields (may need information hiding):"
	rg -n 'pub\s+\w+:\s' "$SEARCH_PATH" --type rust 2>/dev/null | head -20 | sed 's/^/    /'
	ISSUES_FOUND=1
else
	echo "  No obvious inside-out structs."
fi
echo ""

# --- 5. Temporal coupling (runtime ordering assertions) ---
echo "## 5. Temporal Coupling (ordering assertions)"
echo ""

TEMPORAL=$(rg -n 'assert!.*must\|assert!.*first\|assert!.*before\|assert!.*already\|assert!.*initialized' "$SEARCH_PATH" --type rust -i 2>/dev/null | head -10 || true)

if [ -n "$TEMPORAL" ]; then
	echo "  Runtime ordering assertions found (consider typestate):"
	echo "$TEMPORAL" | sed 's/^/    /'
	ISSUES_FOUND=1
else
	echo "  No runtime ordering assertions detected."
fi
echo ""

# --- 6. Complexity indicators ---
echo "## 6. Complexity Indicators"
echo ""

# Count impl blocks
IMPL_COUNT=$(rg -c '^\s*impl\s' "$SEARCH_PATH" --type rust 2>/dev/null | awk -F: '{sum += $NF} END {print sum+0}')
echo "  impl blocks: $IMPL_COUNT"

# Count trait definitions
TRAIT_COUNT=$(rg -c '^\s*pub\s+trait\s' "$SEARCH_PATH" --type rust 2>/dev/null | awk -F: '{sum += $NF} END {print sum+0}')
echo "  pub traits:  $TRAIT_COUNT"

# Count generic parameters (complexity indicator)
GENERIC_COUNT=$(rg -c '<[A-Z]\w*[:,>]' "$SEARCH_PATH" --type rust 2>/dev/null | awk -F: '{sum += $NF} END {print sum+0}')
echo "  generic params: $GENERIC_COUNT"

# Lines of code
LOC=$(rg -c '.' "$SEARCH_PATH" --type rust 2>/dev/null | awk -F: '{sum += $NF} END {print sum+0}')
echo "  lines of Rust: $LOC"

if [ "$IMPL_COUNT" -gt 0 ] && [ "$PUB_ITEMS" -gt 0 ]; then
	DEPTH_ESTIMATE=$(echo "scale=1; $LOC / $PUB_ITEMS" | bc 2>/dev/null || echo "??")
	echo ""
	echo "  Depth estimate (LOC / pub items): $DEPTH_ESTIMATE"
	echo "  (Higher = deeper module. Very low = shallow wrapper.)"
fi

echo ""
echo "=== Audit Complete ==="

if [ "$ISSUES_FOUND" -eq 1 ]; then
	echo "Issues found. Review the items above."
	exit 1
else
	echo "No issues detected."
	exit 0
fi
