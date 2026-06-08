#!/usr/bin/env bash
# Run bounded lean-rs memory profiling workloads.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TARGET="${1:-}"

if [[ -z "$TARGET" ]]; then
	echo "Usage: $0 <long-session|long-session-fresh|long-session-pooled|long-session-steady|long-session-matrix|long-session-bracketed|long-session-derived|worker-cycling|pool-memory|mathlib-scale|all>"
	exit 1
fi

cd "$REPO_ROOT"
export RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native -C force-frame-pointers=yes}"
export LEAN_RS_NUM_THREADS="${LEAN_RS_NUM_THREADS:-1}"

build_host_example() {
	cargo build --profile profiling -p lean-rs-host --example long_session_memory
}

build_worker_examples() {
	cargo build --profile profiling -p lean-rs-worker-child \
		--bin lean-rs-worker-child \
		--example memory_cycling \
		--example pool_memory_scheduling \
		--example mathlib_scale_probe
}

run_long_session_mode() {
	local mode="$1"
	echo "=== long-session ${mode} ==="
	build_host_example
	LEAN_RS_LONG_SESSION_MODE="$mode" \
		LEAN_RS_LONG_SESSION_IMPORTS="${LEAN_RS_LONG_SESSION_IMPORTS:-4}" \
		LEAN_RS_LONG_SESSION_BULK="${LEAN_RS_LONG_SESSION_BULK:-64}" \
		LEAN_RS_LONG_SESSION_ELAB="${LEAN_RS_LONG_SESSION_ELAB:-64}" \
		LEAN_RS_LONG_SESSION_POOL_CAPACITY="${LEAN_RS_LONG_SESSION_POOL_CAPACITY:-1}" \
		LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY="${LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY:-1}" \
		LEAN_RS_LONG_SESSION_MAX_RSS_KIB="${LEAN_RS_LONG_SESSION_MAX_RSS_KIB:-2097152}" \
		"$REPO_ROOT/target/profiling/examples/long_session_memory"
}

run_worker_cycling() {
	echo "=== worker-cycling ==="
	build_worker_examples
	LEAN_RS_WORKER_MEMORY_IMPORTS="${LEAN_RS_WORKER_MEMORY_IMPORTS:-6}" \
		LEAN_RS_WORKER_MEMORY_MAX_IMPORTS="${LEAN_RS_WORKER_MEMORY_MAX_IMPORTS:-2}" \
		"$REPO_ROOT/target/profiling/examples/memory_cycling"
}

run_pool_memory() {
	echo "=== pool-memory ==="
	build_worker_examples
	"$REPO_ROOT/target/profiling/examples/pool_memory_scheduling"
}

run_mathlib_scale() {
	echo "=== mathlib-scale ==="
	build_worker_examples
	LEAN_RS_MATHLIB_SCALE_LIMIT="${LEAN_RS_MATHLIB_SCALE_LIMIT:-32}" \
		"$REPO_ROOT/target/profiling/examples/mathlib_scale_probe"
}

case "$TARGET" in
long-session) run_long_session_mode all ;;
long-session-fresh) run_long_session_mode fresh-import ;;
long-session-pooled) run_long_session_mode pooled-reuse ;;
long-session-steady) run_long_session_mode steady-state ;;
long-session-matrix) run_long_session_mode import-matrix ;;
long-session-bracketed) run_long_session_mode bracketed-lightweight ;;
long-session-derived) run_long_session_mode derived-indexes ;;
worker-cycling) run_worker_cycling ;;
pool-memory) run_pool_memory ;;
mathlib-scale) run_mathlib_scale ;;
all)
	run_long_session_mode fresh-import
	run_long_session_mode pooled-reuse
	run_long_session_mode steady-state
	run_long_session_mode import-matrix
	run_long_session_mode bracketed-lightweight
	run_long_session_mode derived-indexes
	run_worker_cycling
	run_pool_memory
	;;
*)
	echo "Unknown target: $TARGET"
	echo "Available: long-session, long-session-fresh, long-session-pooled, long-session-steady, long-session-matrix, long-session-bracketed, long-session-derived, worker-cycling, pool-memory, mathlib-scale, all"
	exit 1
	;;
esac
