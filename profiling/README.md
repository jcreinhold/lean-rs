# lean-rs Profiling

This directory contains opt-in profiling commands for Lean import memory growth and worker-boundary performance. The
normal test suite should stay memory-bounded through `cargo nextest`; these scripts are for diagnosis and release notes.

## Workloads

- `long-session`: same-process `SessionPool` RSS checkpoints with a bounded fresh-import policy.
- `worker-cycling`: worker-child restart behavior under a small `max_imports` budget.
- `pool-memory`: worker-pool admission, per-worker RSS policy, and reuse counters.
- `mathlib-scale`: optional larger worker-pool workload; set `LEAN_RS_MATHLIB_ROOT` to use a real mathlib checkout.

## Commands

Run a bounded workload:

```sh
./profiling/scripts/profile_memory.sh long-session
./profiling/scripts/profile_memory.sh worker-cycling
./profiling/scripts/profile_memory.sh pool-memory
```

Capture CPU stacks with `samply`:

```sh
cargo install samply
./profiling/scripts/profile_with_samply.sh long-session
```

The scripts set `RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes"` unless the caller already supplied
`RUSTFLAGS`. Keep workload env vars in the command line so reports name the exact run.

## Safe Defaults

Defaults are deliberately small. Increase them only when the previous run's peak RSS is acceptable:

```sh
LEAN_RS_LONG_SESSION_IMPORTS=8 \
LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY=1 \
LEAN_RS_LONG_SESSION_MAX_RSS_KIB=2097152 \
./profiling/scripts/profile_memory.sh long-session
```

