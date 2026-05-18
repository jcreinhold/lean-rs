# Testing

The workspace gate is [`cargo-nextest`](https://nexte.st/), not the default `cargo test`. This
document explains why, and how to run the suite locally.

## Why nextest

Each test that exercises a `LeanSession` imports `LeanRsFixture.Handles`, which transitively
imports the full Lean compiler environment. The resulting per-test working set is several hundred
megabytes. In single-process `cargo test`, Lean's interned name table, mimalloc heap, and
globally-registered environments grow monotonically across tests — `Drop` releases per-session
reference counts but cannot reclaim what Lean's process-global tables hold. After ~150 of the
~225 tests in this workspace, a developer machine or CI runner with limited memory will OOM
(observed: macOS `memorystatus` kills the process at ~30 GB compressed memory).

`cargo nextest run` runs each test in its own process, so cumulative Lean state resets at every
process boundary. This is the supported test runner.

## Running the suite

Install once:

```sh
cargo install cargo-nextest --locked
```

Run:

```sh
cargo nextest run --workspace
```

The workspace's [`.config/nextest.toml`](../.config/nextest.toml) profile caps concurrent test
processes at **4** so total memory stays bounded across CI runners (~7 GiB on `ubuntu-latest` /
`macos-latest`). The workspace's [`.cargo/config.toml`](../.cargo/config.toml) sets
`LEAN_RS_NUM_THREADS=1` so each Lean process spawns a single worker thread; the product is at
most 4 Lean workers across the whole test run.

Doctests are not picked up by nextest. Run them separately:

```sh
cargo test --doc --workspace
```

## Local overrides

If your machine has memory to spare, you can raise the process cap:

```sh
NEXTEST_TEST_THREADS=8 cargo nextest run --workspace
```

For a single-test debug loop, plain `cargo test` is fine — the cumulative-state pathology only
fires across many tests in the same binary:

```sh
cargo test -p lean-rs --lib host::tests::session_query_missing_declaration_is_host_error -- --nocapture
```

To run with Lean's default thread count (e.g., a benchmark run that legitimately wants Lean
parallelism), unset the env var:

```sh
LEAN_RS_NUM_THREADS= cargo bench -p lean-rs
```

## CI

CI runs the `ci` nextest profile (`cargo nextest run --workspace --profile ci`), which adds one
automatic retry on transient failures. See [`.github/workflows/ci.yml`](../.github/workflows/ci.yml).

## Per-process Lean threads

The Lean task manager is started by `LeanRuntime::init`. By default the worker count is Lean's
compiled-in heuristic (typically one worker per hardware core). Set `LEAN_RS_NUM_THREADS` to a
positive integer **before** the first `init` call to pin the worker count for the lifetime of the
process; invalid values fall back to the Lean default with a `tracing::warn!`. See the
`LeanRuntime::init` docstring at [`crates/lean-rs/src/runtime/init.rs`](../crates/lean-rs/src/runtime/init.rs)
for the full contract.

## Why not fix the cumulative growth instead

We could:

- Hoist `LeanHost` / `LeanCapabilities` into per-binary shared state. Blocked by the
  `LeanHost: !Send + !Sync` contract (`docs/architecture/04-concurrency.md`); `OnceLock`/`LazyLock`
  require `Sync`.
- Find and fix the leak. Lean's interned name table and mimalloc retention behaviour are
  process-global by design; the work-to-payoff ratio is poor compared to process isolation.

nextest's process-per-test model dissolves both problems with no public-API churn. The trade-off
is wall-clock cost from repeated cold `LeanRuntime::init` calls; on a 12-core M4 Pro the full
suite runs in ~30 seconds, which is well under the human-attention threshold for an inner-loop
test gate.
