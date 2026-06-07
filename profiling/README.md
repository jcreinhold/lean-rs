# lean-rs Profiling

This directory contains opt-in profiling commands for Lean import memory growth, worker-boundary CPU cost, and
release-note-ready baseline reports. The normal test suite should stay memory-bounded through `cargo nextest`; these
commands are for diagnosis and performance evidence.

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
- `long-session-matrix`: import-mode matrix for `exported-public`, `server`, `private`, explicit `full-private-compat`, and exported no-extension diagnostics.
- `long-session-bracketed`: one-shot declaration metadata query under `loadExts := false` with `freeRegions` after the bracket.
- `worker-cycling`: worker-child restart behavior under a small `max_imports` budget.
- `pool-memory`: worker-pool admission, per-worker RSS policy, and reuse counters.
- `mathlib-scale`: optional larger worker-pool workload; set `LEAN_RS_MATHLIB_ROOT` to use a real mathlib checkout.

Long-session and worker session-open workloads print `import_stats=...` rows next to RSS checkpoints. Bracketed
lightweight runs print `bracketed_import_stats=... free_regions_ran=true` after the no-extension bracket returns.
Baseline reports parse those rows into Lean Import Stats tables while preserving the raw key-value output.

Run a bounded memory workload:

```sh
./profiling/scripts/profile_memory.sh long-session-fresh
./profiling/scripts/profile_memory.sh long-session-pooled
./profiling/scripts/profile_memory.sh long-session-steady
./profiling/scripts/profile_memory.sh long-session-matrix
./profiling/scripts/profile_memory.sh long-session-bracketed
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
LEAN_RS_LONG_SESSION_MAX_RSS_KIB=2097152 \
./profiling/scripts/profile_memory.sh long-session-fresh
```

Do not use same-process fresh-import loops as a production soak test. Long-running hosts should use worker children with
`LeanWorkerRestartPolicy::memory_bounded` and `LeanWorkerPoolConfig` RSS ceilings.

The import matrix records narrower exported/server attempts, the selected default `private` profile, and explicit
`full-private-compat`. `Environment.freeRegions` remains unsafe after `loadExts := true`; the matrix records retained
environment shape and does not reclaim compacted regions.

The bracketed lightweight workload is the exception by construction: it uses `loadExts := false`, returns only
serialized Rust-owned data, and reports whether `freeRegions` ran before Rust receives the result.

A local capped probe on macOS aarch64 with Lean 4.31.0-rc1, one `LeanRsFixture.Handles` import, reported
`bracketed_after_lean_import_1=1010720 KiB`, `bracketed_after_query_before_free_1=1032032 KiB`,
`bracketed_after_free_1=108048 KiB`, `imported_bytes=1955323616`, `compacted_regions=9033`, and
`free_regions_ran=true`. Treat these as raw local measurements, not portable RSS targets.
