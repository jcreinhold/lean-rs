#!/usr/bin/env bash
# Print the prefix of an already-installed, explicitly named Lean release.
set -euo pipefail

version=${1:?expected a Lean release version}
pin="leanprover/lean4:v$version"
# The sweep skips absent releases but treats a broken installed release as a failure.
installed=$(elan toolchain list)
if ! awk -v pin="$pin" '$1 == pin { found = 1 } END { exit !found }' <<< "$installed"; then
	printf 'Lean toolchain is not installed: %s\n' "$pin" >&2
	exit 3
fi
prefix=$(elan run "$pin" lean --print-prefix)
if [[ "$prefix" != /* || "$prefix" == *$'\n'* || ! -x "$prefix/bin/lean" ]]; then
	printf 'Invalid Lean prefix for %s: %s\n' "$pin" "$prefix" >&2
	exit 1
fi
actual=$("$prefix/bin/lean" --short-version)
if [[ "$actual" != "$version" ]]; then
	printf 'Expected Lean %s, found %s at %s\n' "$version" "$actual" "$prefix" >&2
	exit 1
fi
printf '%s\n' "$prefix"
