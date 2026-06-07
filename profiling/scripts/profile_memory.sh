#!/usr/bin/env bash
# Run bounded lean-rs memory profiling workloads.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TARGET="${1:-}"

if [[ -z "$TARGET" ]]; then
	echo "Usage: $0 <long-session|worker-cycling|pool-memory|mathlib-scale|all>"
	exit 1
fi

cd "$REPO_ROOT"
export RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native -C force-frame-pointers=yes}"
export LEAN_RS_NUM_THREADS="${LEAN_RS_NUM_THREADS:-1}"

run_long_session() {
	echo "=== long-session ==="
	LEAN_RS_LONG_SESSION_IMPORTS="${LEAN_RS_LONG_SESSION_IMPORTS:-4}" \
    LEAN_RS_LONG_SESSION_BULK="${LEAN_RS_LONG_SESSION_BULK:-64}" \
    LEAN_RS_LONG_SESSION_ELAB="${LEAN_RS_LONG_SESSION_ELAB:-64}" \
    LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY="${LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY:-1}" \
    LEAN_RS_LONG_SESSION_MAX_RSS_KIB="${LEAN_RS_LONG_SESSION_MAX_RSS_KIB:-2097152}" \
    cargo run --release -p lean-rs-host --example long_session_memory
}

build_worker_child() {
	cargo build --release -p lean-rs-worker-child --bin lean-rs-worker-child
}

run_worker_cycling() {
	echo "=== worker-cycling ==="
	build_worker_child
	LEAN_RS_WORKER_MEMORY_IMPORTS="${LEAN_RS_WORKER_MEMORY_IMPORTS:-6}" \
		LEAN_RS_WORKER_MEMORY_MAX_IMPORTS="${LEAN_RS_WORKER_MEMORY_MAX_IMPORTS:-2}" \
		cargo run --release -p lean-rs-worker-child --example memory_cycling
}

run_pool_memory() {
	echo "=== pool-memory ==="
	build_worker_child
	cargo run --release -p lean-rs-worker-child --example pool_memory_scheduling
}

run_mathlib_scale() {
	echo "=== mathlib-scale ==="
	build_worker_child
	LEAN_RS_MATHLIB_SCALE_LIMIT="${LEAN_RS_MATHLIB_SCALE_LIMIT:-32}" \
		cargo run --release -p lean-rs-worker-child --example mathlib_scale_probe
}

case "$TARGET" in
long-session) run_long_session ;;
worker-cycling) run_worker_cycling ;;
pool-memory) run_pool_memory ;;
mathlib-scale) run_mathlib_scale ;;
all)
	run_long_session
	run_worker_cycling
	run_pool_memory
	;;
*)
	echo "Unknown target: $TARGET"
	echo "Available: long-session, worker-cycling, pool-memory, mathlib-scale, all"
	exit 1
	;;
esac
