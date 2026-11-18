#!/usr/bin/env bash
# Compare the C declarations mirrored by lean-rs-sys/src/repr.rs between
# an existing supported release and a candidate. This is a textual check;
# measure size, alignment and offsets separately and review inline semantics.
#
# Usage: check-lean-header.sh <existing-version> <candidate-version>
# Example: check-lean-header.sh 4.29.1 4.30.0
#
# Empty output = the declarations are unchanged. This does not establish
# ABI compatibility or symbol availability; follow docs/bump-toolchain.md.
# Non-empty output = review the changes before extending the window.

set -euo pipefail

if [ "$#" -ne 2 ]; then
	echo "usage: $0 <existing-version> <candidate-version>" >&2
	exit 2
fi

EXISTING=$1
CANDIDATE=$2
TOOLCHAIN_ROOT="${ELAN_HOME:-$HOME/.elan}/toolchains"
EXTRACT='/^typedef struct[^{]*\{/{p=1} p{print} /^} lean_external_object;/{found=1; exit} END{if (!p || !found) exit 1}'

EXISTING_DECLS=$(mktemp)
CANDIDATE_DECLS=$(mktemp)
trap 'rm -f "$EXISTING_DECLS" "$CANDIDATE_DECLS"' EXIT

# lean_object and lean_ctor_object are anonymous typedefs in lean.h. Start
# at the first struct definition, including named or anonymous typedefs,
# so neither header nor constructor fields are skipped. Extract before
# diff so an absent block fails closed.
awk "$EXTRACT" "$TOOLCHAIN_ROOT/leanprover--lean4---v$EXISTING/include/lean/lean.h" >"$EXISTING_DECLS"
awk "$EXTRACT" "$TOOLCHAIN_ROOT/leanprover--lean4---v$CANDIDATE/include/lean/lean.h" >"$CANDIDATE_DECLS"
diff "$EXISTING_DECLS" "$CANDIDATE_DECLS"
