# Mathlib-Scale Worker Fixture

A large-workload fixture for the worker capability layer. It is deliberately not a `lean-dup` implementation; it uses
mathlib-shaped module names and command-like exports to stress the operational boundary:

```text
import planner -> LeanWorkerPool -> session lease -> typed command
```

## Boundary

The fixture may simulate `version`, `doctor`, `index`, `extract`, `features`, and `probe` command shapes, but the row
schemas remain small test data owned by `lean-rs-worker` fixtures. It does not copy `lean-dup` declaration rows, feature
rows, cache rules, ranking, reporting, or source-provenance policy.

The normal path is the pool path:

- `LeanWorkerImportPlanner` groups module work into stable session batches.
- `LeanWorkerPool` chooses a compatible local child and returns a lease.
- A typed command runs through the lease.
- Rows, diagnostics, progress, terminal metadata, cancellation, timeout, fatal exits, and cycling use the same worker
  capability contracts as downstream callers.

Low-level worker calls remain useful for focused tests, but they are not the source-of-truth scale path.

## Workload

The fixture export `lean_rs_interop_consumer_worker_shape_mathlib_scale_index` emits:

- declaration-like rows on the `declarations` stream;
- feature-like rows on the `features` stream;
- probe-like rows on the `probes` stream;
- start/finish diagnostics;
- chunk progress for the simulated import/index pass;
- terminal metadata with fixture name, command, row count, and module count.

The fallback workload uses 16 mathlib-shaped module names and emits 47 rows: 16 declarations, 16 features, and 15
probes. This keeps CI deterministic while still exercising mixed streams and terminal accounting.

When `LEAN_RS_MATHLIB_ROOT` points at a mathlib checkout, the `mathlib_scale_probe` example uses the discovered mathlib
module list as the planning workload shape. The current fixture still emits deterministic test rows; a run with a
mathlib module list is evidence about planning, pool leases, and session reuse, not a claim that `lean-rs-worker` has
indexed mathlib semantics.

## Commands

Build the fixture and run the focused tests:

```sh
cd fixtures/interop-shims && lake build
cargo test -p lean-rs-worker --test mathlib_scale_fixture -- --nocapture
```

Run the local scale probe:

```sh
cargo build -p lean-rs-worker --bin lean-rs-worker-child
cargo run -p lean-rs-worker --example mathlib_scale_probe
```

Use a real mathlib module list when available:

```sh
LEAN_RS_MATHLIB_ROOT=/path/to/mathlib4 \
LEAN_RS_MATHLIB_SCALE_LIMIT=128 \
cargo run -p lean-rs-worker --example mathlib_scale_probe
```

Run the benchmark group:

```sh
cargo build --release -p lean-rs-worker --bin lean-rs-worker-child
cargo bench -p lean-rs-worker --bench worker_capability -- mathlib_scale
```

## Sample capture

On macOS/aarch64 with Lean 4.29.1, the fallback probe (16 modules, 47 rows) ran at ~140 rows/s for both `single_worker`
and `pool_max_2` (the fallback produces one planned batch, so `pool_max_2` is a pool-API smoke test, not evidence of
parallel speedup). Cancellation, fatal-exit recovery, and post-cycle replay all worked; RSS sampling was unavailable
from `ps` on that machine. A claim about mathlib-scale performance must record the command, module limit, Lean version,
and whether RSS sampling was available. Re-capture on the same hardware before declaring a regression.

## Non-Goals

This fixture does not add remote workers, search/ranking/cache semantics, downstream row schemas, or new callback
payloads. Worker throughput still flows through worker IPC, pool leases, scheduling, row formats, and batching
decisions; L1 object callbacks are not a worker-scale shortcut.
