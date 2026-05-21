# Lean-Dup Readiness Proof

Prompt 86 proves that the generic worker foundation can replace the
subprocess-worker shape used by a `lean-dup`-class tool. The proof is not a
`lean-dup` implementation and not a migration guide. It is a boundary check:
the `lean-rs-worker` pool can host the operational responsibilities, while
downstream crates keep their schemas and semantic policy.

## What The Proof Covers

The readiness example runs:

```sh
cargo run -p lean-rs-worker --example lean_dup_readiness
```

The example uses the normal large-scale path:

```text
LeanWorkerImportPlanner -> LeanWorkerPool -> LeanWorkerSessionLease -> typed command
```

It exercises six command shapes through generic typed commands:

- `version` as a typed JSON command;
- `doctor` as a typed JSON command;
- `extract` as a typed streaming command;
- `features` as a typed streaming command;
- `index` as a typed streaming command;
- `probe` as a typed streaming command.

The fixture exports use command-like names only to stress the worker
capability layer. They do not define declaration rows, feature rows, probe
results, cache keys, ranking, report policy, or source provenance for
`lean-dup`.

## Responsibility Map

| Current subprocess responsibility | `lean-rs-worker` primitive |
| --- | --- |
| Start and supervise a Lean subprocess | `LeanWorkerPool` and worker supervisors |
| Build/load the worker capability | `LeanWorkerCapabilityBuilder` plus `lean-toolchain` |
| Batch modules by import/session requirements | `LeanWorkerImportPlanner` planned batches |
| Reuse imported sessions | `LeanWorkerSessionLease` keyed by session material |
| Stream JSONL-like rows | typed worker streaming commands and data sinks |
| Separate diagnostics from data | `LeanWorkerDiagnosticSink` |
| Emit progress/control events | `LeanWorkerProgressSink` |
| Mark output committable | terminal summaries with commit-after-success semantics |
| Enforce request deadlines | request timeout/watchdog policy |
| Recover from child panic/abort | typed child-failure errors and fresh leases |
| Reset Lean process-global memory | explicit/policy worker cycling |
| Observe large runs | `LeanWorkerPoolSnapshot` and lease snapshots |
| Bound slow sinks | bounded row-delivery backpressure |

This deletes caller-owned subprocess plumbing: ad hoc child spawning, stdin
JSON request writing, stdout JSONL framing, stderr parsing, EOF classification,
manual timeout kills, manual restart sequencing, pipe-draining loops, and
RSS/pool bookkeeping.

## What Remains Downstream-Owned

A downstream project still owns:

- request, row, and terminal summary schemas;
- semantic algorithms for extraction, features, indexing, and probes;
- cache validity and persistence;
- ranking and reporting;
- source provenance and user-facing paths;
- command names and CLI policy;
- any compatibility decisions based on downstream metadata.

This is the intended split. `lean-rs-worker` carries typed commands, rows,
diagnostics, terminal summaries, timeouts, cycling, backpressure, and pool
state; it does not know what a downstream row means.

## Comparison Input

`/Users/jcreinhold/Code/lean-dup` is read-only comparison input. The readiness
example records the checkout revision when present. If
`LEAN_RS_WORKER_COMPARE_COMMAND` is set, the example runs that command and
prints its status and elapsed time. The comparison is optional because
`lean-rs-worker` should not depend on a local downstream checkout.

Any comparison must name the command, revision, workload, and limits. Without
that, the readiness proof only claims generic coverage and local worker-pool
operating behavior.

## Measured Local Envelope

The example prints:

- command-shape coverage and row counts;
- diagnostic and progress counts;
- terminal summary command names;
- timeout, cancellation, fatal-exit recovery, explicit cycle, and
  backpressure outcomes;
- pool snapshot counters;
- parent and child RSS when the platform permits sampling;
- optional subprocess comparison status.

Do not treat the fixture rows as `lean-dup` rows. They are small generic test
data used to prove that the worker substrate can carry the shape.

