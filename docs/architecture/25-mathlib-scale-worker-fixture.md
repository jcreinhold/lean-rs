# Mathlib-Scale Worker Fixture

Prompt 84 adds a large-workload fixture for the worker capability layer. The
fixture is deliberately not a `lean-dup` implementation. It uses mathlib-shaped
module names and command-like exports to stress the same operational boundary:

```text
import planner -> LeanWorkerPool -> session lease -> typed command
```

## Boundary

The fixture may simulate `version`, `doctor`, `index`, `extract`, `features`,
and `probe` command shapes, but the row schemas remain small test data owned by
`lean-rs-worker` fixtures. It does not copy `lean-dup` declaration rows, feature
rows, cache rules, ranking, reporting, or source-provenance policy.

The normal path is the pool path:

- `LeanWorkerImportPlanner` groups module work into stable session batches.
- `LeanWorkerPool` chooses a compatible local child and returns a lease.
- A typed command runs through the lease.
- Rows, diagnostics, progress, terminal metadata, cancellation, timeout, fatal
  exits, and cycling use the same worker capability contracts as downstream
  callers.

Low-level worker calls remain useful for focused tests, but they are not the
source-of-truth scale path.

## Workload

The fixture export
`lean_rs_interop_consumer_worker_shape_mathlib_scale_index` emits:

- declaration-like rows on the `declarations` stream;
- feature-like rows on the `features` stream;
- probe-like rows on the `probes` stream;
- start/finish diagnostics;
- chunk progress for the simulated import/index pass;
- terminal metadata with fixture name, command, row count, and module count.

The fallback workload uses 16 mathlib-shaped module names and emits 47 rows:
16 declarations, 16 features, and 15 probes. This keeps CI deterministic while
still exercising mixed streams and terminal accounting.

When `LEAN_RS_MATHLIB_ROOT` points at a mathlib checkout, the
`mathlib_scale_probe` example uses the discovered mathlib module list as the
planning workload shape. The current fixture still emits deterministic test
rows; a run with a mathlib module list is evidence about planning, pool leases,
and session reuse, not a claim that `lean-rs-worker` has indexed mathlib
semantics.

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

## Local Capture

The initial fallback probe on macOS/aarch64 with Lean 4.29.1 recorded:

```text
workload=mathlib_scale_worker_fixture
module_source=fallback
module_count=16
mathlib_available=false
single_worker rows=47 rows_per_second=141.9
pool_max_2 rows=47 rows_per_second=149.3
cancellation=true
fatal_exit=true
post_cycle_rows=47
parent_rss_start_kib=unavailable
parent_rss_end_kib=unavailable
```

RSS was unavailable in that local `ps` sample, so the capture is throughput and
behavior evidence only. A future mathlib run must record the command, module
limit, Lean version, and whether RSS sampling was available before making any
mathlib-scale performance claim.

The first Criterion capture for the same fallback path recorded:

```text
cargo bench -p lean-rs-worker --bench worker_capability -- mathlib_scale --sample-size 10
worker::capability_shape/mathlib_scale_single_worker_pool
  time: [346.18 ms 350.61 ms 355.55 ms]
  throughput: [132.19 elem/s 134.05 elem/s 135.77 elem/s]
worker::capability_shape/mathlib_scale_pool_max_2
  time: [337.82 ms 342.28 ms 347.35 ms]
  throughput: [135.31 elem/s 137.31 elem/s 139.13 elem/s]
```

This benchmark uses the same typed command and pool lease surface. The
`pool_max_2` fallback path creates one planned batch, so it is a pool API
comparison, not evidence for parallel speedup. Parallel scaling requires a
multi-batch workload or a real mathlib module list.

## Non-Goals

This fixture does not add remote workers, search/ranking/cache semantics,
downstream row schemas, or new callback payloads. Worker throughput still flows
through worker IPC, pool leases, scheduling, row formats, and batching decisions;
L1 object callbacks are not a worker-scale shortcut.
