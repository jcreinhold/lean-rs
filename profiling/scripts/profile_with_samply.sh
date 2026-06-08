#!/usr/bin/env bash
# Record a bounded lean-rs profiling workload with samply.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TARGET="${1:-}"
RESULTS_DIR="$REPO_ROOT/profiling_results"

if [[ -z "$TARGET" ]]; then
	echo "Usage: $0 <long-session|worker-cycling|pool-memory|mathlib-scale>"
	exit 1
fi

if ! command -v samply &>/dev/null; then
	echo "Error: samply not found. Install with: cargo install samply"
	exit 1
fi

cd "$REPO_ROOT"
mkdir -p "$RESULTS_DIR"
export RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native -C force-frame-pointers=yes}"
export LEAN_RS_NUM_THREADS="${LEAN_RS_NUM_THREADS:-1}"

record_profile() {
	local profile_name="$1"
	shift
	local output="$RESULTS_DIR/${profile_name}.json.gz"
	echo "Recording $profile_name -> $output"
	samply record --save-only --output "$output" --profile-name "$profile_name" -- "$@"
	python3 "$SCRIPT_DIR/analyze_samply_symbols.py" "$output" || true
}

case "$TARGET" in
long-session)
	cargo build --profile profiling -p lean-rs-host --example long_session_memory
	export LEAN_RS_LONG_SESSION_MODE="${LEAN_RS_LONG_SESSION_MODE:-fresh-import}"
	export LEAN_RS_LONG_SESSION_IMPORTS="${LEAN_RS_LONG_SESSION_IMPORTS:-1}"
	export LEAN_RS_LONG_SESSION_POOL_CAPACITY="${LEAN_RS_LONG_SESSION_POOL_CAPACITY:-1}"
	export LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY="${LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY:-1}"
	export LEAN_RS_LONG_SESSION_MAX_RSS_KIB="${LEAN_RS_LONG_SESSION_MAX_RSS_KIB:-1572864}"
	record_profile "long-session-${LEAN_RS_LONG_SESSION_MODE:-fresh-import}" \
		"$REPO_ROOT/target/profiling/examples/long_session_memory"
	;;
worker-cycling)
	cargo build --profile profiling -p lean-rs-worker-child --bin lean-rs-worker-child --example memory_cycling
	export LEAN_RS_WORKER_MEMORY_IMPORTS="${LEAN_RS_WORKER_MEMORY_IMPORTS:-6}"
	export LEAN_RS_WORKER_MEMORY_MAX_IMPORTS="${LEAN_RS_WORKER_MEMORY_MAX_IMPORTS:-2}"
	record_profile "worker-cycling" "$REPO_ROOT/target/profiling/examples/memory_cycling"
	;;
pool-memory)
	cargo build --profile profiling -p lean-rs-worker-child --bin lean-rs-worker-child --example pool_memory_scheduling
	record_profile "pool-memory" "$REPO_ROOT/target/profiling/examples/pool_memory_scheduling"
	;;
mathlib-scale)
	cargo build --profile profiling -p lean-rs-worker-child --bin lean-rs-worker-child --example mathlib_scale_probe
	export LEAN_RS_MATHLIB_SCALE_LIMIT="${LEAN_RS_MATHLIB_SCALE_LIMIT:-32}"
	record_profile "mathlib-scale" "$REPO_ROOT/target/profiling/examples/mathlib_scale_probe"
	;;
*)
	echo "Unknown target: $TARGET"
	echo "Available: long-session, worker-cycling, pool-memory, mathlib-scale"
	exit 1
	;;
esac
