#!/usr/bin/env bash
# Record a bounded lean-rs profiling workload with samply.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TARGET="${1:-}"

if [[ -z "$TARGET" ]]; then
	echo "Usage: $0 <long-session|worker-cycling|pool-memory|mathlib-scale>"
	exit 1
fi

if ! command -v samply &>/dev/null; then
	echo "Error: samply not found. Install with: cargo install samply"
	exit 1
fi

cd "$REPO_ROOT"
export RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native -C force-frame-pointers=yes}"
export LEAN_RS_NUM_THREADS="${LEAN_RS_NUM_THREADS:-1}"

case "$TARGET" in
    long-session)
        LEAN_RS_LONG_SESSION_MAX_RSS_KIB="${LEAN_RS_LONG_SESSION_MAX_RSS_KIB:-2097152}" \
            samply record -- cargo run --release -p lean-rs-host --example long_session_memory
        ;;
worker-cycling)
	cargo build --release -p lean-rs-worker-child --bin lean-rs-worker-child
	samply record -- cargo run --release -p lean-rs-worker-child --example memory_cycling
	;;
pool-memory)
	cargo build --release -p lean-rs-worker-child --bin lean-rs-worker-child
	samply record -- cargo run --release -p lean-rs-worker-child --example pool_memory_scheduling
	;;
mathlib-scale)
	cargo build --release -p lean-rs-worker-child --bin lean-rs-worker-child
	samply record -- cargo run --release -p lean-rs-worker-child --example mathlib_scale_probe
	;;
*)
	echo "Unknown target: $TARGET"
	echo "Available: long-session, worker-cycling, pool-memory, mathlib-scale"
	exit 1
	;;
esac
