#!/usr/bin/env bash
# Print the prefix of an already-installed, explicitly named Lean release.
set -euo pipefail

version=${1:?expected a Lean release version}
pin="leanprover/lean4:v$version"
# Elan can provision a missing release even for 'run' without '--install'.
# Use its offline inventory for the local no-install policy. An unreadable or
# broken installation is an error; only an absent pin is skippable by the sweep.
inventory=$(elan dump-state --no-net)
installed=$(python3 -c '
import json, sys
pin = sys.argv[1]
for entry in json.load(sys.stdin)["toolchains"]["installed"]:
    if entry["resolved_name"] == pin:
        print(entry["path"])
        break
else:
    print(f"Lean toolchain is not installed: {pin}", file=sys.stderr)
    sys.exit(3)
' "$pin" <<< "$inventory")
if [[ "$installed" != /* || ! -x "$installed/bin/lean" ]]; then
	printf 'Invalid installed Lean executable for %s: %s/bin/lean\n' "$pin" "$installed" >&2
	exit 1
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
