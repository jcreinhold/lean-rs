# lean-rs Profiling

This directory contains opt-in profiling commands for Lean import memory growth, worker-boundary CPU cost, and
release-note-ready baseline reports. The normal test suite should stay memory-bounded through `cargo nextest`; these
commands are for diagnosis and performance evidence.

The production-safe hosting pattern that these workloads validate is documented in
[`docs/production-hosting.md`](../docs/production-hosting.md): bounded worker pools, memory-bounded child cycling, warm
session reuse, batched repeated work, and typed resource-failure reporting.

## What To Use

- Baseline reports: `collect_baseline_quick` and `collect_baseline_full`.
- Bounded RSS probes: `profiling/scripts/profile_memory.sh`.
- CPU stacks: `profiling/scripts/profile_with_samply.sh`.
- Raw artifacts: `profiling_results/` (ignored by git).

## Baseline Reports

Run from the repository root:

```sh
export RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes"

cargo run --profile profiling -p lean-rs-profiling --bin collect_baseline_quick
cargo run --profile profiling -p lean-rs-profiling --bin collect_baseline_full
```

The collectors build profiling binaries once, run the examples directly, and write both structured JSON and Markdown:

- `profiling_results/baseline_data_quick.json`
- `profiling_results/profiling-baseline-quick.md`
- `profiling_results/baseline_data_full.json`
- `profiling_results/profiling-baseline-full.md`

Regenerate Markdown from existing JSON:

```sh
cargo run --profile profiling -p lean-rs-profiling --bin generate_report -- --quick
cargo run --profile profiling -p lean-rs-profiling --bin generate_report
```

## Workloads

- `long-session-fresh`: same-process fresh imports with RSS/import guardrails.
- `long-session-pooled`: same-process pooled reuse after one bounded warm import.
- `long-session-steady`: same-process steady query/elaboration loop after one bounded import.
- `long-session-matrix`: import-mode matrix for `exported-public`, `server`, `private`, explicit `full-private-compat`,
  and exported no-extension diagnostics.
- `long-session-bracketed`: one-shot declaration metadata query under `loadExts := false` with `freeRegions` after the
  bracket.
- `long-session-derived`: full-session query probes that report source-range, pretty-printing, proof-search,
  parser/elaborator, module-snapshot, and lazy discriminator initialization work.
- `worker-cycling`: worker-child restart behavior under a small `max_imports` budget.
- `pool-memory`: worker-pool admission, per-worker RSS policy, and reuse counters.
- `mathlib-scale`: optional larger worker-pool workload; set `LEAN_RS_MATHLIB_ROOT` to use a real mathlib checkout.

Long-session and worker session-open workloads print `import_stats=...` rows next to RSS checkpoints. Those rows include
total compacted-region bytes, mmap-backed compacted-region bytes, non-mmap compacted-region bytes, and `imported_bytes`
as the compatibility alias for the total. Bracketed lightweight runs print
`bracketed_import_stats=... free_regions_ran=true` after the no-extension bracket returns. Derived-index probes print
`query_derived_work=...` rows for query phases. Baseline reports parse those rows into Lean Import Stats and Lean
Derived Work tables while preserving the raw key-value output, and show an RSS gap when a workload-level RSS sample is
available.

Run a bounded memory workload:

```sh
./profiling/scripts/profile_memory.sh long-session-fresh
./profiling/scripts/profile_memory.sh long-session-pooled
./profiling/scripts/profile_memory.sh long-session-steady
./profiling/scripts/profile_memory.sh long-session-matrix
./profiling/scripts/profile_memory.sh long-session-bracketed
./profiling/scripts/profile_memory.sh long-session-derived
./profiling/scripts/profile_memory.sh worker-cycling
./profiling/scripts/profile_memory.sh pool-memory
```

`long-session` runs all same-process modes in child processes so retained Lean import state from `fresh-import` does not
poison pooled or steady-state measurements.

## CPU Profiles

Install `samply`, then record a bounded profile:

```sh
cargo install samply
./profiling/scripts/profile_with_samply.sh long-session
./profiling/scripts/profile_with_samply.sh worker-cycling
```

Profiles are saved as `profiling_results/<workload>.json.gz`. The local analyzer reports when a profile is captured but
mostly unsymbolicated; open the saved file in Firefox Profiler for native Lean frames.

## Safe Defaults

Defaults are deliberately small. Increase them only when the previous run's peak RSS is acceptable:

```sh
LEAN_RS_LONG_SESSION_IMPORTS=8 \
LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY=1 \
LEAN_RS_LONG_SESSION_MAX_RSS_KIB=1572864 \
./profiling/scripts/profile_memory.sh long-session-fresh
```

Do not use same-process fresh-import loops as a production soak test. Long-running hosts should use worker children with
`LeanWorkerRestartPolicy::memory_bounded` and `LeanWorkerPoolConfig` RSS ceilings. See
[`docs/production-hosting.md`](../docs/production-hosting.md) for the caller-facing configuration and error-handling
pattern; use this README when changing or recapturing the measurements behind it.

The worker rebaseline collector records those ceilings explicitly instead of relying on example defaults:

```sh
LEAN_RS_WORKER_MEMORY_IMPORTS=6 \
LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=1 \
LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB=1572864 \
./profiling/scripts/profile_memory.sh worker-cycling

LEAN_RS_POOL_MEMORY_MAX_WORKERS=1 \
LEAN_RS_POOL_MEMORY_TOTAL_RSS_KIB=1572864 \
LEAN_RS_POOL_MEMORY_PER_WORKER_RSS_KIB=1572864 \
LEAN_RS_POOL_MEMORY_MAX_IMPORTS=1 \
./profiling/scripts/profile_memory.sh pool-memory
```

Worker and pool workloads also print `admission=...` rows. The Markdown report renders them under **Import Admission**
with cold-open attempts, admitted opens, typed refusals, import-like requests, observed concurrent cold opens, and RSS
before/after admission. A nonzero refusal count is expected in the pool stress row when the configured worker/RSS budget
rejects a distinct cold session before another import starts.

Pool workloads print `session_reuse=...` rows. The Markdown report renders them under **Session Reuse Keys** with key
hits, key misses, distinct keys, fresh imports avoided, and miss reasons. These rows are separate from admission rows:
reuse rows explain whether an equivalent request used a warm session, while admission rows explain whether cold work was
allowed. Session keys preserve Lean import order and include only session-safety facts such as canonical roots, import
profile, metadata expectation, toolchain identity, and manifest identity where applicable; they are not downstream
result cache keys.

Worker and pool workloads also print `replacement=...` rows. The Markdown report renders them under **Worker
Replacement** with synchronous replacement attempts, successes, failures, spawn/handshake time, capability-load time,
session-open/import time, first-command time, warm-command time, total replacement time, restart reason, and budget
status. Prompt 24 keeps replacement synchronous and records `synchronous-no-overlap`; warm spares or background
prewarming remain deferred until measurements show a latency need and total child RSS budget can admit temporary
overlap.

Pool workloads print `batch=...` rows for the warm proof-agent module-query batch path. The Markdown report renders them
under **Warm Batch Workloads** with selector counts, request/import deltas, elapsed time, parent/child RSS, bounded item
counts, item-level failures, truncation status, and worker frame count when available. Prompt 25 reuses the existing
`process_module_query_batch` API through one warm pool lease; it reduces request/session churn and does not reclaim Lean
memory, avoid cold import cost, or replace worker cycling. Worker frame counts currently report `unavailable` because
the protocol layer does not expose frame counters.

`collect_baseline_quick` first measures `max_imports=1`. It only measures `max_imports=2` when the one-import worker run
stays at or below 70% of the configured 1.5 GiB budget, and the Markdown report recommends the largest candidate whose
peak RSS stays within the budget. With the default 1,572,864 KiB cap, the 70% gate is intentionally conservative and
normally skips the two-import candidate on local machines. Historical 2026-06-08 data at a 2 GiB cap showed
`max_imports=1` peaking at 1,194,368 KiB and `max_imports=2` peaking at 3,236,416 KiB.

The import matrix records narrower exported/server attempts, the selected default `private` profile, and explicit
`full-private-compat`. `Environment.freeRegions` remains unsafe after `loadExts := true`; the matrix records retained
environment shape and does not reclaim compacted regions.

The bracketed lightweight workload is the exception by construction: it uses `loadExts := false`, returns only
serialized Rust-owned data, and reports whether `freeRegions` ran before Rust receives the result.

The derived-index workload stays on normal full sessions with `loadExts := true`. It does not reclaim compacted regions.
It records whether a requested query phase touched derived work such as declaration-range lookup, notation-aware pretty
printing, proof-search facts, parser/elaborator execution, module snapshot construction, or Lean's
`lazy discriminator import initialization` profiler span. `LazyDiscrTree` laziness is derived-index laziness over
already imported module data; it is not lazy `.olean` loading and it does not make compacted regions unloadable.

`LEAN_RS_LEAN_MAX_MEMORY_KIB` can be set for worker/profiling runs as a Lean runtime fail-fast guardrail. It is not a
cleanup mechanism and does not replace worker cycling; it only lets Lean's periodic memory checks throw before the OS
terminates the process.

A local capped probe on macOS aarch64 with Lean 4.31.0-rc1, one `LeanRsFixture.Handles` import, reported
`bracketed_after_lean_import_1=1010720 KiB`, `bracketed_after_query_before_free_1=1032032 KiB`,
`bracketed_after_free_1=108048 KiB`, `imported_bytes=1955323616`, `compacted_regions=9033`, and `free_regions_ran=true`.
Treat these as raw local measurements, not portable RSS targets.
