# Testing

The workspace gate is [`cargo-nextest`](https://nexte.st/), not `cargo test`. `cargo test`
(single-process) is not the gate—cumulative Lean state OOMs the binary after ~150 tests
(see [Why nextest](#why-nextest) below).

## Run the suite

Install once:

```sh
cargo install cargo-nextest --locked
```

Run:

```sh
cargo nextest run --workspace
```

Doctests are not picked up by nextest:

```sh
cargo test --doc --workspace
```

## Tuning

The workspace's [`.config/nextest.toml`](../.config/nextest.toml) caps concurrent test processes
at **4** so total memory stays bounded across CI runners (~7 GiB on
`{ubuntu-latest, macos-latest}`). The workspace's [`.cargo/config.toml`](../.cargo/config.toml)
sets `LEAN_RS_NUM_THREADS=1` so each Lean process spawns a single worker thread; product is at
most 4 Lean workers across the full run.

| Knob                            | Effect                                                                                |
| ------------------------------- | ------------------------------------------------------------------------------------- |
| `NEXTEST_TEST_THREADS=8`        | Use more processes when the machine has memory to spare.                              |
| `cargo test -p lean-rs --lib …` | Single-test debug loop. The cumulative-state pathology doesn't fire at one test.      |
| `cargo bench -p lean-rs`        | Bench that wants real Lean parallelism. Unset `LEAN_RS_NUM_THREADS` first (see below).|

Unset `LEAN_RS_NUM_THREADS` (`unset LEAN_RS_NUM_THREADS` in the shell, or
prefix the command with `LEAN_RS_NUM_THREADS=`) when you need Lean to use
its own worker-count heuristic—typically for benchmarks.

## Per-process Lean threads

The Lean task manager starts in `LeanRuntime::init`. By default worker count is Lean's
compiled-in heuristic (typically one worker per hardware core). Set `LEAN_RS_NUM_THREADS` to a
positive integer **before** the first `init` call to pin the count for the process; invalid
values fall back to the Lean default with a `tracing::warn!`. See the `LeanRuntime::init`
docstring at [`crates/lean-rs/src/runtime/init.rs`](../crates/lean-rs/src/runtime/init.rs)
for the full contract.

## CI

CI runs the `ci` nextest profile (`cargo nextest run --workspace --profile ci`), which adds one
automatic retry on transient failures. See [`.github/workflows/ci.yml`](../.github/workflows/ci.yml).

## Why nextest

Each test that exercises a `LeanSession` imports `LeanRsFixture.Handles`, which transitively
imports the full Lean compiler environment. Per-test working set is several hundred megabytes.
In single-process `cargo test`, Lean's interned name table, mimalloc heap, and
globally-registered environments grow monotonically across tests—`Drop` releases per-session
refcounts but cannot reclaim what Lean's process-global tables hold. After ~150 of ~225 tests,
a developer machine or CI runner with limited memory OOMs (observed: macOS `memorystatus` kills
the process at ~30 GB compressed memory).

`cargo nextest run` runs each test in its own process, so cumulative Lean state resets at every
process boundary.

## Why not fix the cumulative growth instead

Two alternatives, both rejected:

- **Hoist `LeanHost` / `LeanCapabilities` into per-binary shared state.** The `LeanHost: !Send + !Sync` contract ([`docs/architecture/04-concurrency.md`](architecture/04-concurrency.md)) prevents this; `OnceLock`/`LazyLock` require `Sync`.
- **Find and fix the leak.** Lean's interned name table and mimalloc retention are process-global by design; the work-to-payoff ratio is poor compared to process isolation.

Nextest's process-per-test model dissolves both problems with no public-API churn. The trade-off
is wall-clock cost from repeated cold `LeanRuntime::init` calls; on a 12-core M4 Pro the full
suite runs in ~30 seconds—well under the inner-loop attention threshold.
