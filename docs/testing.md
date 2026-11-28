# Testing

The workspace gate is [`cargo-nextest`](https://nexte.st/). Earlier same-process `cargo test` runs exhausted memory
after about 150 tests as Lean state accumulated; see [Why nextest](#why-nextest).

## Formatting

Run `cargo fmt --all --check` for Rust formatting. The **Rust line width** CI job also checks added and edited Rust
lines against `max_width` in `rustfmt.toml`, including strings and macro calls that rustfmt can leave over the limit. It
reports the file, line, and width. To run the same check locally with Python 3.11 or later:

```sh
python3 scripts/check_rust_line_width.py HEAD
```

Use the pull request's base commit to check the full diff. Stage new Rust files or mark them with `git add -N` so Git
includes them in the local check.

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
cargo check --profile profiling -p lean-rs-worker-child --bin lean-rs-worker-child --example memory_cycling
cargo check --profile profiling -p lean-rs-profiling --bins
```

## Disposable Linux checks on Docker Desktop

Run the link-free worker startup tests on Linux without a host toolchain or cache mount:

```sh
scripts/test_linux_docker.sh
# ARM64 fallback when x86-64 emulation is unavailable or too slow:
scripts/test_linux_docker.sh --platform linux/arm64
# Other small link-free selections:
scripts/test_linux_docker.sh --package lean-rs-abi --lib
scripts/test_linux_docker.sh --filter startup_failure
```

The default uses x86-64 emulation (`linux/amd64`) and runs
`cargo test --locked -j1 -p lean-rs-worker-parent --lib -- --test-threads=1`: the parent crate's unit tests, which
start, fail and clean up fake children. It selects the local `desktop-linux` Docker Desktop context on macOS and rejects
non-Unix-socket endpoints. Inherited Docker endpoint settings are cleared. It requires Docker Desktop and GNU `timeout`
(`timeout` or `gtimeout`). The official `docker.io/library/rust:1.100.0-bookworm` image pins Rust 1.100.0 with rustup
auto-install off. Cargo may download locked registry dependencies; no Lean install, header bypass, host Cargo cache or
credentials are supplied. Only this checkout mounts at `/src`, read-only. A read-only container root and a disposable,
executable 3 GiB `/lab` tmpfs hold Cargo, home and temporary output. A separate private 16 MiB, non-executable `/tmp`
tmpfs gives the read-only root a writable `/tmp`; no host `/tmp` mounts into the container. The container has 2 CPUs, 4
GiB memory, 192 PIDs, no capabilities and no privilege escalation. Incremental compilation is off.

The script requires 30 GiB free on the checkout's filesystem: a 25 GiB reserve plus 5 GiB headroom. Also check Docker
Desktop's own storage if it uses another volume. The test has a six-minute timeout and Docker startup/pull/test together
have an eight-minute timeout. Exit or interruption removes the named container; `--rm` also removes it when it exits.
The Docker image remains cached. No logs or build outputs are saved on the host. A failed Docker connection can prevent
cleanup; inspect Docker Desktop before retrying rather than start duplicates.

The optional package selector allows only `lean-rs-abi`, `lean-rs-worker-protocol` and `lean-rs-worker-parent`;
`--test`, `--lib` and `--filter` choose a bounded libtest run, not arbitrary Cargo arguments. Check the reported
selected test count: a mistyped filter can select zero tests. These runs do not cover native Lean loading, and do not
replace broad nextest or Linux release gates. This local command does not use GitHub Actions.

## Tuning

The workspace's [`.config/nextest.toml`](https://github.com/jcreinhold/lean-rs/blob/main/.config/nextest.toml) runs
**two** independent test processes by default. Memory tests and tests that build or load the shipped template reserve
all slots. The named Lean 4.36 checks and older release checks stay serial. Use `NEXTEST_TEST_THREADS=1` for an
unprepared checkout or a small container. The workspace's
[`.cargo/config.toml`](https://github.com/jcreinhold/lean-rs/blob/main/.cargo/config.toml) also sets `build.jobs=1`,
`RUST_TEST_THREADS=1`, `LEAN_RS_NUM_THREADS=1`, and a default `LEAN_RS_LEAN_MAX_MEMORY_KIB=1572864` guardrail for worker
children. These defaults make normal `cargo nextest`, worker examples, and profiling examples use the local low-memory
shape without manual shell exports. Focused same-process `cargo test` runs that open full host sessions now fail before
Lean import unless the developer explicitly opts into that risk.

| Knob | Effect |
| --- | --- |
| `NEXTEST_TEST_THREADS=8` | Use more processes when the machine has memory to spare. |
| `CARGO_BUILD_JOBS=4` | Use more compile parallelism when the machine has memory to spare. |
| `cargo test -p lean-rs --lib …` | Single-test debug loop for crates that do not open full host sessions. |
| `LEAN_RS_ALLOW_CARGO_TEST_HOST_IMPORTS=1 cargo test -p lean-rs-host …` | Explicitly budgeted one-off host-session debug run. Do not use as a full-suite gate. |
| `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1 cargo nextest run -p lean-rs-host --test session_leak_loop --run-ignored all` | Run ignored leak-loop diagnostics intentionally. Tune `LEAN_RS_LEAK_LOOP_ITERS` first. |
| `LEAN_RS_NUM_THREADS=8 cargo bench -p lean-rs` | Select explicit Lean parallelism for a budgeted benchmark. |

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
`--run-ignored all` runs from executing long fresh-import loops in one test process. These are explicitly budgeted
import diagnostics; the current ASan job does not run host-session loops or instrument the Lean runtime. Routine
coverage comes from nextest-isolated small tests and worker checks.

Set a positive `LEAN_RS_NUM_THREADS` to select benchmark parallelism. For Lean's compiled-in heuristic, prefix the Cargo
command with `LEAN_RS_NUM_THREADS=`; the current parser warns about the empty value and falls back. Merely unsetting the
shell variable lets Cargo restore the configured value `1`.

## Worker fixture qualification

Use the SDK-owned fixture for focused Lean 4.36 tests on macOS arm64 or Linux x86-64. From the repository root, run the
broken-import control:

```sh
python3 scripts/qualify_metadata.py broken-import --emit-host-selection
```

For metadata, doctor or host-handle changes, test the affected cases and one control. Set `SHIM_MANIFEST` to the
absolute path of the prepared `*.lean-rs-capability.json` file reported in the shim descriptor above. The qualifier
builds one SDK shim generation under `target/metadata-qualification/stream-package` using the installed Elan toolchain.
It copies the tracked Lake inputs, links the Rust sources, and uses the normal Cargo target. The broken-import, typed
and `interop-stream` workers use this package's manifest and lock files. A successful `--emit-host-selection` hands the
generation to the next `interop-stream` command, which checks the package identity, manifest and images and removes it
after all selected workers close. An explicit checkout manifest can also serve as the comparison input to a standalone
`interop-stream` check. For the full 4.36 release checks, `--workspace-shims` hands the same generation to
`workspace-tests`. The header-reuse probe imports from that generation. The shipped-template tests use a disposable copy
of the tracked template with the same host dependency selected for both Cargo and the template build script. The
qualifier checks the resolved package before Nextest and checks the manifest and image pins again before removing the
package. A failed run retains its state until closure can be established.

```sh
python3 scripts/qualify_metadata.py interop-stream "$SHIM_MANIFEST" \
  --case runtime_and_capability_metadata_are_separate \
  --case session_import_profile_reaches_child_import
```

Repeat `--case` for each test. Run `interop-stream --help` to see the accepted names. The runner checks the fixture and
verifies that each case's workers exit. These focused checks do not replace `scripts/prerelease.sh`, which prepares the
fixtures and runs the release checks.

`workspace-tests` also runs the `supervisor` test `parent_death::busy_worker_exits_when_its_parent_is_killed` with the
sealed fixture worker, whose policy admits one busy `#eval`. The test kills the parent with SIGKILL. The worker, the
only holder of an inherited probe FIFO, must exit within 10 s while it is inside Lean.

## Worker fixtures

The interop fixture's capability exports metadata and doctor commands with small, deterministic schemas.
`metadata_doctor` checks that runtime and capability metadata stay separate, that doctor reports carry pass, warning and
error diagnostics, and that malformed JSON and missing exports come back as typed worker errors. `host_handle` checks
that the session import profile reaches the child's import. The fixtures define no extraction algorithm, cache policy,
ranking, reporting, or source-provenance contract; downstream code keeps those decisions.

## Import-heavy test inventory

Use this inventory when adding or selecting tests:

| Test surface | Category | Safe routine runner | Notes |
| --- | --- | --- | --- |
| `lean-rs`, `lean-toolchain`, `lean-rs-worker-protocol` unit tests | cheap/unit | `cargo test -p <crate>` or nextest | No full host-session imports. |
| `lean-rs-host` unit tests that do not call `LeanCapabilities::session` | cheap/unit | nextest or focused `cargo test` | Same-process host import guard does not affect these. |
| `crates/lean-rs-host/tests/session_leak_loop.rs` small tests | bounded per-test imports | `cargo nextest run -p lean-rs-host --test session_leak_loop --profile ci` | Each selected test process resets Lean state. |
| `session_create_drop_loop_long` | import-heavy diagnostic | ignored plus `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1` | Explicitly budgeted diagnostics; not part of the current ASan job or routine smoke tests. |
| `crates/lean-rs-host/tests/batching.rs` | many host-session service tests | `cargo nextest run -p lean-rs-host --test batching --profile ci` | Do not run the whole integration binary through plain `cargo test`; use nextest isolation. |
| `long_session_memory` example and profiling collectors | profiling-only | `cargo check` locally; default RSS cap is 1,572,864 KiB; tune with `LEAN_RS_LONG_SESSION_MAX_RSS_KIB` | Runtime profiling must be capped and opt-in. |
| Worker child integration tests | worker process boundary | the qualifier's `workspace-tests`, as `scripts/prerelease.sh` runs it | Fresh imports happen in worker children; the qualifier prepares the shims they load. |

## Per-process Lean threads

The Lean task manager starts in `LeanRuntime::init`. By default worker count is Lean's compiled-in heuristic (typically
one worker per hardware core). Set `LEAN_RS_NUM_THREADS` to a positive integer **before** the first `init` call to pin
the count for the process; invalid values fall back to the Lean default with a `tracing::warn!`. See the
`LeanRuntime::init` docstring at
[`crates/lean-rs/src/runtime/init.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/src/runtime/init.rs)
for the full contract.

## CI

CI runs the `ci` nextest profile (`cargo nextest run --workspace --profile ci`), which adds one automatic retry on
transient failures. See
[`.github/workflows/ci.yml`](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/ci.yml).

Per-test memory still matters under nextest's process isolation: a single worker child that performs several
compiler-environment imports in one test can exceed the CI runner's memory on its own. The `session_reuse` pool-key
tests import the `Init`-only fixture modules (`LeanRsFixture.Scalars`/`Strings`) rather than
`LeanRsHostShims.Elaboration` wherever the test pins key/eviction behavior and never elaborates: measured locally, the
compiler-environment variant of the capacity tests peaked at ~7.2 GiB of child RSS and was SIGKILLed by `memorystatus`
on the 7 GB macOS runner (empty diagnostics, exit `signal: 9`); the `Init`-only variant peaks at ~1.3 GiB. Tests that
genuinely elaborate keep the heavy imports. New worker tests should pick the lightest import profile that exercises the
behavior they pin.

## Why nextest

Import-heavy host fixture tests can load the full Lean compiler environment through `LeanRsFixture.Handles`. Their
working set can reach several hundred megabytes before repeated imports. In single-process `cargo test`, Lean's interned
name table, mimalloc heap, and globally-registered environments grow monotonically across tests—`Drop` releases
per-session refcounts but cannot reclaim what Lean's process-global tables hold. In a recorded earlier suite of about
225 tests, macOS `memorystatus` killed the process after about 150 tests at roughly 30 GB compressed memory. Those
counts are historical observations, not a current suite size or memory limit.

`cargo nextest run` runs each test in its own process, so cumulative Lean state resets at every process boundary. The
same memory fact applies outside tests: long-running applications should use the worker crates with restart/RSS policy
when they may open many distinct import sets.

Shared per-binary host state also conflicts with [thread affinity](architecture/concurrency.md): `LeanHost` is
`!Send + !Sync`, while static `OnceLock`/`LazyLock` storage requires `Sync`.

The host-session guard is intentionally narrow: it only blocks `LeanCapabilities::session`, `session_with_profile`, and
`profiling_session` from Cargo/libtest binaries. It does not affect normal `cargo run` profiling commands, worker child
processes, bracketed no-extension queries, or nextest runs.
