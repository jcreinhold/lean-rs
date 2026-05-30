#!/usr/bin/env bash
# Charter guard (CLAUDE.md): raw lean_* externs and the unsafe_code opt-out live only in
# crates/lean-rs-sys. This blocks an edit that would introduce either elsewhere, surfacing
# the violation in the edit loop rather than at the CI symbol check.
set -euo pipefail

payload=$(cat)

f=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty')

# Only Rust files are in scope.
case "$f" in
*.rs) ;;
*) exit 0 ;;
esac

# crates/lean-rs-sys is the one crate allowed to declare raw FFI / opt out of unsafe.
case "$f" in
*/crates/lean-rs-sys/* | crates/lean-rs-sys/*) exit 0 ;;
esac

# The text the edit would introduce (Write -> content, Edit -> new_string,
# MultiEdit -> every edit's new_string).
body=$(printf '%s' "$payload" | jq -r '.tool_input | (.content // .new_string // ([.edits[]?.new_string] | join("\n")) // "")')

if { printf '%s' "$body" | grep -Eq 'extern[[:space:]]+"C"' && printf '%s' "$body" | grep -q 'lean_'; } ||
	printf '%s' "$body" | grep -Eq 'allow\(\s*unsafe_code\s*\)'; then
	echo "lean-rs charter (CLAUDE.md): raw lean_* externs and the unsafe_code opt-out belong only in crates/lean-rs-sys. Declare the symbol there (extend the extern block + REQUIRED_SYMBOLS) and call it through the safe layer." >&2
	exit 2
fi

exit 0
