#!/usr/bin/env bash
# Verify that the 10 #[repr(C)] struct layouts in lean-rs-sys/src/repr.rs
# match a candidate Lean release. Diffs the relevant block of lean.h
# between the existing supported release and a new candidate.
#
# Usage: check-lean-header.sh <existing-version> <candidate-version>
# Example: check-lean-header.sh 4.29.1 4.30.0
#
# Empty output = layouts unchanged, safe to add as another window entry.
# Non-empty output = stop and revisit
# docs/architecture/02-versioning-and-compatibility.md.

set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <existing-version> <candidate-version>" >&2
    exit 2
fi

EXISTING=$1
CANDIDATE=$2
TOOLCHAIN_ROOT="${ELAN_HOME:-$HOME/.elan}/toolchains"
EXTRACT='/^typedef struct lean_object \{|^} lean_(ctor|array|sarray|string|closure|ref|thunk|task|promise|external)_object/{p=1} p{print} /^} lean_external_object/{p=0}'

diff \
    <(awk "$EXTRACT" "$TOOLCHAIN_ROOT/leanprover--lean4---v$EXISTING/include/lean/lean.h") \
    <(awk "$EXTRACT" "$TOOLCHAIN_ROOT/leanprover--lean4---v$CANDIDATE/include/lean/lean.h")
