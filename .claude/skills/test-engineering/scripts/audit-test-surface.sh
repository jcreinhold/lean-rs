#!/usr/bin/env bash
set -euo pipefail

# Inventory the test and bench surface for a path inside the repo.
#
# Interface:
# - Argument 1: file or directory path to inspect. May point at a crate root,
#   a subdirectory inside a crate, or a single file.
# - Output: a plain-text report with four sections:
#   - [files]: paths under tests/ or benches/
#   - [counts]: coarse counts of common test surfaces
#   - [surfaces]: matching Rust lines that expose the main test mechanisms
#   - [verification]: the narrowest obvious `cargo nextest run -p ...` command
# - Failure modes: exits non-zero on missing input or invalid usage.
#
# Rationale:
# This script is intentionally small and approximate. Its job is to save repeated
# repo discovery work during test audits, not to compute a perfect coverage model.
# The counts are heuristics for "what surfaces exist here?", not claims about
# quality or completeness.

if [[ $# -ne 1 ]]; then
	echo "usage: $0 <path>" >&2
	exit 1
fi

input_path="$1"
if [[ ! -e "$input_path" ]]; then
	echo "error: path not found: $input_path" >&2
	exit 1
fi

target="$(cd "$(dirname "$input_path")" && pwd)/$(basename "$input_path")"

find_crate_root() {
	# Walk upward from the inspected path until we hit the nearest Cargo.toml.
	# The report needs the owning crate so it can suggest a targeted nextest
	# command instead of forcing callers to rediscover package boundaries.
	local path="$1"
	if [[ -d "$path" ]]; then
		local dir="$path"
		while [[ "$dir" != "/" ]]; do
			if [[ -f "$dir/Cargo.toml" ]]; then
				echo "$dir"
				return 0
			fi
			dir="$(dirname "$dir")"
		done
	else
		find_crate_root "$(dirname "$path")"
		return 0
	fi
	return 1
}

crate_root="$(find_crate_root "$target" || true)"

count_rg() {
	# Count regex matches in Rust source files only.
	# This is used for surfaces where matching a family of spellings matters more
	# than exact token text, such as snapshot helper variants.
	local pattern="$1"
	local path="$2"
	rg -g '*.rs' -n "$pattern" "$path" 2>/dev/null | wc -l | tr -d ' '
}

count_fixed() {
	# Count exact fixed-string matches in Rust source files.
	# Use this when regex word boundaries would make the intent less obvious or
	# more fragile, such as the literal `#[test]` attribute.
	local needle="$1"
	local path="$2"
	rg -g '*.rs' -n -F "$needle" "$path" 2>/dev/null | wc -l | tr -d ' '
}

echo "target: $target"
if [[ -n "$crate_root" ]]; then
	echo "crate_root: $crate_root"
else
	echo "crate_root: <none>"
fi

echo
echo "[files]"
find "$target" \( -path '*/tests/*' -o -path '*/benches/*' \) -type f 2>/dev/null | sort

echo
echo "[counts]"
# These counts are quick signals for what kinds of guardrails already exist.
# They are not correctness metrics and should never be treated as coverage.
echo "unit_tests=$(count_fixed '#[test]' "$target")"
echo "property_tests=$(count_rg 'proptest!' "$target")"
echo "snapshot_usage=$(count_rg 'assert_snapshot|assert_debug_snapshot|assert_display_snapshot|assert_yaml_snapshot|assert_json_snapshot|\\binsta::' "$target")"
echo "compile_fail_usage=$(count_rg 'trybuild|compile_fail' "$target")"
echo "criterion_benches=$(count_rg 'criterion_group!|criterion_main!' "$target")"

echo
echo "[surfaces]"
rg -g '*.rs' -n 'proptest!|#\[test\]|criterion_group!|criterion_main!|trybuild|insta|compile_fail' "$target" 2>/dev/null || true

if [[ -n "$crate_root" ]]; then
	package_name="$(sed -n 's/^name = "\(.*\)"/\1/p' "$crate_root/Cargo.toml" | head -n 1)"
	echo
	echo "[verification]"
	if [[ -n "$package_name" ]]; then
		echo "cargo nextest run -p $package_name"
	else
		echo "cargo nextest run -p <package-name>"
	fi
fi
