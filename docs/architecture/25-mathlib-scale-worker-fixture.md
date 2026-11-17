# Worker Scale and Replacement Fixtures

The fixtures exercise the operational path used by downstream worker tools:

```text
LeanWorkerImportPlanner -> LeanWorkerPool -> LeanWorkerSessionLease -> typed command
```

They use small, deterministic schemas. Command names such as `extract`, `features`, `index`, and `probe` resemble a
`lean-dup`-class tool, but the fixtures define no downstream extraction algorithm, cache policy, ranking, reporting, or
source-provenance contract.

## Scale workload

`lean_rs_interop_consumer_worker_shape_mathlib_scale_index` emits declaration-like rows on `declarations`, feature-like
rows on `features`, and probe-like rows on `probes`, with start/finish diagnostics, chunk progress, and terminal
metadata. The fallback uses 16 mathlib-style module names and emits 47 rows: 16 declarations, 16 features, and 15 probes.

With `LEAN_RS_MATHLIB_ROOT`, `mathlib_scale_probe` discovers a real mathlib module list for planning. It still emits
fixture rows. Such a run measures planning, leases, and session reuse; it does not index mathlib semantics.

The probe covers cancellation, fatal-exit recovery, post-cycle replay, and a slow sink. Snapshots report delivered rows,
payload bytes, elapsed time, backpressure, restart counts, and available parent/child RSS. The fallback creates one
planned batch: `pool_max_2` exercises the pool API without demonstrating parallel speedup.

## Downstream replacement coverage

`lean_dup_readiness` runs typed JSON `version`/`doctor` commands and streaming `extract`/`features`/`index`/`probe`
commands through pool leases. It checks completion metadata, diagnostics, progress, timeout, cancellation, fatal exits,
explicit cycling, and backpressure. The division of work is:

| Responsibility | Worker primitive |
| --- | --- |
| Child lifecycle and warm reuse | Pool and supervisor |
| Build/load capability | Capability builder and `lean-toolchain` |
| Group compatible module work | Import planner |
| Request/response and live rows | Typed commands and data sinks |
| Diagnostics and progress | Separate worker sinks |
| Mark output committable | Terminal success summary |
| Deadline, crash recovery, memory reset | Watchdog and worker cycling |
| Observe and bound slow sinks | Snapshots and bounded reader buffer |

Downstream code keeps command algorithms, schemas, metadata compatibility policy, persistence, semantic cache keys,
reporting, provenance, and CLI behavior. The fixture replaces operational subprocess plumbing, not those decisions.

`LEAN_RS_LEAN_DUP_ROOT` optionally supplies a downstream git checkout whose revision is printed. Separately,
`LEAN_RS_WORKER_COMPARE_COMMAND` runs a caller-provided shell comparison and reports its status and elapsed time. A
meaningful comparison records that exact command, checkout revision, workload, and limits.

## Commands

Run focused integration tests with nextest:

```sh
cargo nextest run -p lean-rs-worker-child --test mathlib_scale_fixture --no-capture
cargo nextest run -p lean-rs-worker-child --test lean_dup_readiness --no-capture
```

The probes and benchmark require the worker binary in the corresponding profile:

```sh
cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
cargo run -p lean-rs-worker-child --example mathlib_scale_probe
cargo run -p lean-rs-worker-child --example lean_dup_readiness

LEAN_RS_MATHLIB_ROOT=/path/to/mathlib4 \
LEAN_RS_MATHLIB_SCALE_LIMIT=128 \
cargo run -p lean-rs-worker-child --example mathlib_scale_probe

cargo build --release -p lean-rs-worker-child --bin lean-rs-worker-child
cargo bench -p lean-rs-worker-child --bench worker_capability -- mathlib_scale
```

## Recorded capture

On macOS/aarch64 with Lean 4.29.1, the fallback's 16 modules and 47 rows ran at about 140 rows/s for both `single_worker`
and `pool_max_2`. Cancellation, fatal-exit recovery, and post-cycle replay succeeded; `ps` RSS sampling was unavailable.
This is a historical local capture. A new scale claim must record the command, revision, module limit, platform, Lean
version, row count, and RSS availability. Re-capture before declaring a regression.
