# Testing

The workspace gate is [`cargo-nextest`](https://nexte.st/), not `cargo test`. `cargo test` (single-process) is not the
gate—cumulative Lean state OOMs the binary after ~150 tests (see [Why nextest](#why-nextest) below).

## Run the suite

Install once:

```sh
cargo install cargo-nextest --locked
```

Run:

```sh
cargo xtest
```

`cargo xtest` is a workspace alias for `cargo nextest run --workspace`; use `cargo xtest-ci` for
`cargo nextest run --workspace --profile ci`. Cargo configuration cannot replace the built-in `cargo test` subcommand,
so the repo also rejects full-session host imports when they are started from a same-process libtest binary.

Doctests are not picked up by nextest:

```sh
cargo test --doc --workspace
```

For a local smoke check that avoids same-process import-heavy targets, use:

```sh
cargo nextest run -p lean-rs-host --test session_leak_loop --profile ci
cargo test -p lean-rs-profiling parses_import_stats_lines
cargo check --profile profiling -p lean-rs-host --example long_session_memory
cargo check --profile profiling -p lean-rs-worker-child --bin lean-rs-worker-child --example memory_cycling --example pool_memory_scheduling
cargo check --profile profiling -p lean-rs-profiling --bins
```

## Tuning

The workspace's [`.config/nextest.toml`](../.config/nextest.toml) caps concurrent test processes at **1**. The
workspace's [`.cargo/config.toml`](../.cargo/config.toml) also sets `build.jobs=1`, `RUST_TEST_THREADS=1`,
`LEAN_RS_NUM_THREADS=1`, and a default `LEAN_RS_LEAN_MAX_MEMORY_KIB=1572864` guardrail for worker children. These
defaults make normal `cargo nextest`, worker examples, and profiling examples use the local low-memory shape without
manual shell exports. Focused same-process `cargo test` runs that open full host sessions now fail before Lean import
unless the developer explicitly opts into that risk.

| Knob                            | Effect                                                                                |
| ------------------------------- | ------------------------------------------------------------------------------------- |
| `NEXTEST_TEST_THREADS=8`        | Use more processes when the machine has memory to spare.                              |
| `CARGO_BUILD_JOBS=4`            | Use more compile parallelism when the machine has memory to spare.                    |
| `cargo test -p lean-rs --lib …` | Single-test debug loop for crates that do not open full host sessions.                |
| `LEAN_RS_ALLOW_CARGO_TEST_HOST_IMPORTS=1 cargo test -p lean-rs-host …` | Explicitly budgeted one-off host-session debug run. Do not use as a full-suite gate. |
| `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1 cargo nextest run -p lean-rs-host --test session_leak_loop --include-ignored` | Run ignored leak-loop diagnostics intentionally. Tune `LEAN_RS_LEAK_LOOP_ITERS` first. |
| `cargo bench -p lean-rs`        | Bench that wants real Lean parallelism. Unset `LEAN_RS_NUM_THREADS` first (see below).|

Do not use broad or file-shaped `cargo test` filters for import-heavy host tests, including:

```sh
cargo test -p lean-rs-host session_leak_loop
cargo test -p lean-rs-host --test session_leak_loop
```

Those commands can execute multiple fresh full-session imports inside one Rust test harness process. The post-reduction
fixture import still retains about 1.95 GB of compacted `.olean` region data, and the second same-process fresh import
can move RSS into multi-GiB territory. The host crate now rejects these same-process full-session imports with a typed
resource-exhausted error before Lean import starts. Use `cargo nextest run -p lean-rs-host --test session_leak_loop` for
process isolation, or set `LEAN_RS_ALLOW_CARGO_TEST_HOST_IMPORTS=1` only for one exact debug test with an explicit local
memory budget.

Ignored import-heavy diagnostics add a second opt-in: `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1`. This prevents accidental
`--include-ignored` runs from executing long fresh-import loops in one test process. The ignored loops remain available
for sanitizer or explicitly budgeted diagnostics; routine coverage comes from nextest-isolated small tests and
worker/pool checks.

Unset `LEAN_RS_NUM_THREADS` (`unset LEAN_RS_NUM_THREADS` in the shell, or prefix the command with
`LEAN_RS_NUM_THREADS=`) when you need Lean to use its own worker-count heuristic—typically for benchmarks.

## Import-heavy test inventory

Use this inventory when adding or selecting tests:

| Test surface | Category | Safe routine runner | Notes |
| --- | --- | --- | --- |
| `lean-rs`, `lean-toolchain`, `lean-rs-worker-protocol` unit tests | cheap/unit | `cargo test -p <crate>` or nextest | No full host-session imports. |
| `lean-rs-host` unit tests that do not call `LeanCapabilities::session` | cheap/unit | nextest or focused `cargo test` | Same-process host import guard does not affect these. |
| `crates/lean-rs-host/tests/session_leak_loop.rs` small tests | single fresh import or bounded pool reuse | `cargo nextest run -p lean-rs-host --test session_leak_loop --profile ci` | Each selected test process resets Lean state. |
| `session_create_drop_loop_long`, `pool_acquire_release_loop_long` | import-heavy diagnostic | ignored plus `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1` | Intended for sanitizer/explicit diagnostics, not routine local smoke tests. |
| `crates/lean-rs-host/tests/batching_and_pool.rs` | many host-session service tests | `cargo nextest run -p lean-rs-host --test batching_and_pool --profile ci` | Do not run the whole integration binary through plain `cargo test`; use nextest isolation. |
| `long_session_memory` example and profiling collectors | profiling-only | `cargo check` locally; run with explicit `LEAN_RS_LONG_SESSION_MAX_RSS_KIB` when measuring | Runtime profiling must be capped and opt-in. |
| Worker child/parent pool and capability tests | worker process boundary | `cargo test -p lean-rs-worker-child pool`, `cargo test -p lean-rs-worker-child capability_builder`, or nextest | Fresh imports happen in worker children or isolated test processes. |

## Per-process Lean threads

The Lean task manager starts in `LeanRuntime::init`. By default worker count is Lean's compiled-in heuristic (typically
one worker per hardware core). Set `LEAN_RS_NUM_THREADS` to a positive integer **before** the first `init` call to pin
the count for the process; invalid values fall back to the Lean default with a `tracing::warn!`. See the
`LeanRuntime::init` docstring at [`crates/lean-rs/src/runtime/init.rs`](../crates/lean-rs/src/runtime/init.rs) for the
full contract.

## CI

CI runs the `ci` nextest profile (`cargo nextest run --workspace --profile ci`), which adds one automatic retry on
transient failures. See [`.github/workflows/ci.yml`](../.github/workflows/ci.yml).

## Why nextest

Each test that exercises a `LeanSession` imports `LeanRsFixture.Handles`, which transitively imports the full Lean
compiler environment. Per-test working set is several hundred megabytes. In single-process `cargo test`, Lean's interned
name table, mimalloc heap, and globally-registered environments grow monotonically across tests—`Drop` releases
per-session refcounts but cannot reclaim what Lean's process-global tables hold. After ~150 of ~225 tests, a developer
machine or CI runner with limited memory OOMs (observed: macOS `memorystatus` kills the process at ~30 GB compressed
memory).

`cargo nextest run` runs each test in its own process, so cumulative Lean state resets at every process boundary. The
same memory fact applies outside tests: long-running applications should use the worker crates with restart/RSS policy,
or a same-process `SessionPool` with a fresh-import memory policy, when they may open many distinct import sets.

The host-session guard is intentionally narrow: it only blocks `LeanCapabilities::session`, `session_with_profile`, and
`profiling_session` from Cargo/libtest binaries. It does not affect normal `cargo run` profiling commands, worker child
processes, bracketed no-extension queries, or nextest runs.

## Why not fix the cumulative growth instead

Two alternatives, both rejected:

- **Hoist `LeanHost` / `LeanCapabilities` into per-binary shared state.** The `LeanHost: !Send + !Sync` contract
  ([`docs/architecture/04-concurrency.md`](architecture/04-concurrency.md)) prevents this; `OnceLock`/`LazyLock` require
  `Sync`.
- **Find and fix the leak.** Lean's interned name table and mimalloc retention are process-global by design; the
  work-to-payoff ratio is poor compared to process isolation.

Nextest's process-per-test model dissolves both problems with no public-API churn. The trade-off is wall-clock cost from
repeated cold `LeanRuntime::init` calls; on a 12-core M4 Pro the full suite runs in ~30 seconds—well under the
inner-loop attention threshold.
