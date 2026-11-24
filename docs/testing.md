# Testing

The workspace gate is [`cargo-nextest`](https://nexte.st/). Earlier same-process `cargo test` runs exhausted memory after
about 150 tests as Lean state accumulated; see [Why nextest](#why-nextest).

## Formatting

Run `cargo fmt --all --check` for Rust formatting. The **Rust line width** CI job also checks added and edited
Rust lines against `max_width` in `rustfmt.toml`, including strings and macro calls that rustfmt can leave over
the limit. It reports the file, line, and width. To run the same check locally with Python 3.11 or later:

```sh
python3 scripts/check_rust_line_width.py HEAD
```

Use the pull request's base commit to check the full diff. Stage new Rust files or mark them with `git add -N`
so Git includes them in the local check.

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

## Disposable Linux checks on Docker Desktop

Run the link-free worker startup tests on Linux without a host toolchain or cache mount:

```sh
scripts/test_linux_docker.sh
# ARM64 fallback when x86-64 emulation is unavailable or too slow:
scripts/test_linux_docker.sh --platform linux/arm64
# Other small link-free selections:
scripts/test_linux_docker.sh --package lean-rs-abi --lib
scripts/test_linux_docker.sh --filter missing_authentication
```

The default uses x86-64 emulation (`linux/amd64`) and runs
`cargo test --locked -j1 -p lean-rs-worker-parent --test lifetime_bootstrap -- --test-threads=1`.
It selects the local `desktop-linux` Docker Desktop context on macOS and rejects non-Unix-socket endpoints.
Inherited Docker endpoint settings are cleared. It requires Docker Desktop and GNU `timeout` (`timeout` or `gtimeout`). The official
`docker.io/library/rust:1.100.0-bookworm` image pins Rust 1.100.0 with rustup auto-install off.
Cargo may download locked registry dependencies; no Lean install, header bypass, host Cargo cache or credentials
are supplied. Only this checkout mounts at `/src`, read-only. A read-only container root and a disposable,
executable 3 GiB `/lab` tmpfs hold Cargo, home and temporary output. A separate private 16 MiB, non-executable
`/tmp` tmpfs holds real IPC sockets; no host `/tmp` mounts into the container. The container has 2 CPUs, 4 GiB memory,
192 PIDs, no capabilities and no privilege escalation. Incremental compilation is off.

The script requires 30 GiB free on the checkout's filesystem: a 25 GiB reserve plus 5 GiB headroom.
Also check Docker Desktop's own storage if it uses another volume. The test has a six-minute timeout and
Docker startup/pull/test together have an eight-minute timeout. Exit or interruption removes the named container;
`--rm` also removes it when it exits. The Docker image remains cached. No logs or build outputs are saved on the host.
A failed Docker connection can prevent cleanup; inspect Docker Desktop before retrying rather than start duplicates.

The optional package selector allows only `lean-rs-abi`, `lean-rs-worker-protocol` and `lean-rs-worker-parent`;
`--test`, `--lib` and `--filter` choose a bounded libtest run, not arbitrary Cargo arguments. Check the reported
selected test count: a mistyped filter can select zero tests. These runs do not cover native Lean loading,
writer helpers or managed-reader behavior, and do not replace broad nextest or Linux release gates.
The default command passed all five startup tests on x86-64 Docker Desktop, including cleanup after a failed
startup handshake. This local command does not use GitHub Actions.

## Tuning

The workspace's [`.config/nextest.toml`](https://github.com/jcreinhold/lean-rs/blob/main/.config/nextest.toml) runs **two** independent test processes by default. Memory tests and tests that build or load the shipped template reserve
all slots. The named Lean 4.36 checks and older release checks stay serial. Use `NEXTEST_TEST_THREADS=1` for an
unprepared checkout or a small container. The workspace's [`.cargo/config.toml`](https://github.com/jcreinhold/lean-rs/blob/main/.cargo/config.toml) also sets `build.jobs=1`, `RUST_TEST_THREADS=1`,
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
import diagnostics; the current ASan job does not run host-session loops or instrument the Lean runtime. Routine coverage
comes from nextest-isolated small tests and worker/pool checks.

Set a positive `LEAN_RS_NUM_THREADS` to select benchmark parallelism. For Lean's compiled-in heuristic, prefix the
Cargo command with `LEAN_RS_NUM_THREADS=`; the current parser warns about the empty value and falls back. Merely unsetting
the shell variable lets Cargo restore the configured value `1`.

## Worker fixture qualification

Use the SDK-owned fixture for focused Lean 4.36 tests on macOS arm64 or Linux x86-64.
From the repository root, run the broken-import control:

```sh
python3 scripts/qualify_metadata.py broken-import --emit-host-selection
```

For a cold Lake build through the standalone SDK writer, build `lean-rs-writer` and pass the canonical binary path
and SHA-256 to the custody check. It builds private copies with the SDK name, the former name, and matching values
under both names. It also checks that missing or conflicting settings stop before publishing artifacts.

```sh
python3 scripts/check_test_custody.py "$WRITER" "$WRITER_SHA256" --lake-fixture
```

The worker protocol integration test exercises the stock SDK worker and standalone writer under a single
typed owner-directory binding. It uses the standalone executable for the worker's native guardian.

```sh
LEAN_TOOLCHAIN_READER_TEST_HELPER="$WRITER" cargo nextest run --locked -p lean-rs-worker-child \
  --test protocol --run-ignored only -E 'test(=standalone_worker_and_writer_use_the_typed_owner_directory)'
```

For stream or metadata changes, test the affected cases and one control. Set `SHIM_MANIFEST` to the
absolute path of the prepared `*.lean-rs-capability.json` file reported in the shim descriptor above.
The qualifier builds one SDK shim generation under `target/metadata-qualification/stream-package` using the
installed Elan toolchain. It copies the tracked Lake inputs, links the Rust sources, and uses the normal Cargo
target. The broken-import, typed and stream workers use this package's manifest and lock files.
A successful `--emit-host-selection` hands the generation to the next stream command, which checks the package
identity, manifest and images and removes it after all selected workers close. An explicit checkout manifest can
also serve as the comparison input to a standalone stream check.
For the full 4.36 release checks, `--workspace-shims` hands the same generation to `workspace-tests`.
The header-reuse probe imports from that generation. The shipped-template tests use a disposable copy of the tracked
template with the same host dependency selected for both Cargo and the template build script. The qualifier checks the
resolved package before Nextest and checks the manifest and image pins again before removing the package.
A failed run retains its state until closure can be established.

```sh
python3 scripts/qualify_metadata.py interop-stream "$SHIM_MANIFEST" \
  --case successful_stream_delivers_rows_with_per_stream_sequences \
  --case request_timeout_cycles_child_and_invalidates_session
```

Repeat `--case` for each test. Run `interop-stream --help` to see the accepted names. The runner
checks the fixture and verifies that each case's workers exit. These focused checks do not replace
`scripts/prerelease.sh`, which prepares the fixtures and runs the release checks.

On macOS, the SDK-only cold-open custody regression uses the same host manifest and an SDK guardian launched by
the test executable. It checks a rejected worker, closure of its worker group and guardian, and successful reuse
of the same one-slot pool by a fresh worker.

```sh
python3 scripts/qualify_metadata.py interop-stream "$SHIM_MANIFEST" \
  --case failed_validator_releases_pool_slot_before_fresh_key
```

### Reviewed-writer metadata checks on macOS arm64

The separate `prepare` and `run` commands use a reviewed writer and run the metadata positive and
mismatch selectors in `capability_builder`. Their typed, verification and disposable-clean modes
are not available; use the ordinary SDK qualification above for release checks.

Use a Git checkout, Python 3.12 or later, a trusted Rust toolchain, and the installed release selected by the repository
`lean-toolchain` pin. Select
the executable writer for this platform and obtain its SHA-256 from an independent local review. Use canonical
absolute paths and keep the private deployment and release stable through preparation and execution.

```sh
python3 scripts/qualify_metadata.py --writer "$WRITER" --writer-sha256 "$REVIEWED_WRITER_SHA256" \
  prepare
python3 scripts/qualify_metadata.py --writer "$WRITER" --writer-sha256 "$REVIEWED_WRITER_SHA256" \
  run "$DEPLOYMENT" --worker-sha256 "$REVIEWED_WORKER_SHA256" --control-sha256 "$REVIEWED_CONTROL_SHA256" \
  --deployment-sha256 "$REVIEWED_DEPLOYMENT_SHA256"
```

Preparation uses the reviewed canonical writer in place and prints the worker, control and deployment candidate hashes before
worker launch. Each build embeds local paths and file identities. Have all three reviewed before
passing those hashes to `run`. The runner checks the deployed app sources, writer, worker, record, and selected
runtime before readiness. The readiness command and guarded worker use the same explicit private `target/ws` IPC root;
the runner removes it only after exact owner collection and an empty-root check. The ready descriptor must match the
sealed record. Failed or uncertain dispatch retains the one-use deployment and does not replay it.

## Native guardian service-loss regressions

The macOS-only `lean-toolchain` guardian tests start an independent writer before killing the actual service, separately
with a stopped worker. The service and test controller retain no artifact lock; the real guardian alone retains SH after
the worker drops its local loan handles. The contender records group presence after its first EX acquisition, appends to
the leaf's actual heartbeat file, and retains the same EX descriptor through observed cleanup. It never explicitly
unlocks or changes a live lock mode. The test requires no old group or later payload writes.

A separate guardian-loss control proves that raw EX can succeed while the managed writer's owner-record check still
rejects work. That control deliberately ignores the check to test the write oracle; it is not a production writer
violating it. Its leaf's fixed safety expiry only cleans up the negative case.

Positive setup and recovery must finish within three seconds, before the leaf's four-second safety exit. A test-only
thread in that same guardian can stop its own pinned payload group after three seconds; any rescue use or guardian error
fails the positive test. The outer controller never signals the worker after service death. Failed fixture directories
remain under `/tmp/native-guard-*` for inspection. These tests need no Lean fixture, runtime import or Mathlib build.
They are excluded on other platforms because their stopped-state query and inherited-session cleanup observations are
macOS-specific; they make no Linux, restart or lease claim.

Run the focused tests with an isolated Cargo target if another task owns build output:

```sh
CARGO_TARGET_DIR=/tmp/lean-rs-guardian-tests cargo nextest run --offline -p lean-toolchain --lib -E 'test(native_guardian::tests::)'
```

## Guardian record retention after payload exit

The macOS `record_lifetime_tests` regression pauses the **real guardian inside drain**, after complete payload-group
absence and before its actual SH descriptors drop. This is a running test barrier, not SIGSTOP or process recovery.
The controller holds no SH alias. It proves EX is still blocked, then reserves another actor under a limit of one:
the original record and complete domain list must survive while compute capacity becomes available. After normal
barrier release, actual guardian exit permits same-inode EX acquisition and later collection of the old record.

The test also fills a private namespace with 128 synthetic records referring to this one actual live custodian. This
checks the metadata bound, not 128 processes or transfers. Header-only capacity fixtures use deliberately mismatched
births and remain distinct from the real-custody regression. Existing malformed-record cases still reject admission.

The original collector failed this regression while the guardian still held SH: it removed the recovery record.
An unwind release and a five-second self-kill deadline bound the responsive barrier fixture. Safety use, missing normal
CLI return, a still-live guardian or an unobserved lock release cannot supply a pass. This does not qualify a frozen
last owner, guardian takeover, boot/token recovery or a kernel-stuck process.

```sh
cargo nextest run --offline -p lean-toolchain --lib \
  -E 'test(native_guardian::tests::record_lifetime_tests::)' --test-threads 1 --no-capture
```

## Writer retry regressions

The Unix `writer_lock::tests::admission_tests` use private files and actual owned dummy child processes. An observation
barrier pauses the writer after its real ownership check refuses admission, or after a real later-domain lock blocks.
A separate SH contender must first observe blockage, then acquire during the retry while the writer still announces
its complete demand. The owner remains live and the post-admission write must not run. A held recovery SH also blocks
the writer after that owner exits. The original algorithm failed all three lock-release cases.

The record headers are synthetic but refer to real live private process groups. They test passive admission, not a
writer helper's custody protocol or guardian recovery. A raw-EX control deliberately ignores the owner fence and writes
in the private fixture, proving that the write marker detects this unsafe action; it is not a source-mutation test.
Other cases cover timeout/cancellation cleanup, inode replacement/removal, unlocked anchors, a free zero-budget pass,
and hard rejection of a malformed committed record. Cleanup acts only through actual owned child handles. No test
converts a live artifact lock or explicitly unlocks a retained alias.

```sh
cargo nextest run --offline -p lean-toolchain --lib \
  -E 'test(writer_lock::tests::) | test(writer_demand::tests::) | test(write_domains::tests::)' --no-capture
```

## Worker fixtures

The fixtures exercise the operational path used by downstream worker tools:

```text
LeanWorkerImportPlanner -> LeanWorkerPool -> LeanWorkerSessionLease -> typed command
```

They use small, deterministic schemas. Command names such as `extract`, `features`, `index`, and `probe` resemble a
`lean-dup`-class tool, but the fixtures define no downstream extraction algorithm, cache policy, ranking, reporting, or
source-provenance contract.

### Scale workload

`lean_rs_interop_consumer_worker_shape_mathlib_scale_index` emits declaration-like rows on `declarations`, feature-like
rows on `features`, and probe-like rows on `probes`, with start/finish diagnostics, chunk progress, and terminal
metadata. The fallback uses 16 mathlib-style module names and emits 47 rows: 16 declarations, 16 features, and 15 probes.

With `LEAN_RS_MATHLIB_ROOT`, `mathlib_scale_probe` discovers a real mathlib module list for planning. It still emits
fixture rows. Such a run measures planning, leases, and session reuse; it does not index mathlib semantics.

The probe covers cancellation, fatal-exit recovery, post-cycle replay, and a slow sink. Snapshots report delivered rows,
payload bytes, elapsed time, backpressure, restart counts, and available parent/child RSS. The fallback creates one
planned batch: `pool_max_2` exercises the pool API without demonstrating parallel speedup.

### Worker readiness

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

### Worker fixture commands

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

### Recorded fixture capture

On macOS/aarch64 with Lean 4.29.1, the fallback's 16 modules and 47 rows ran at about 140 rows/s for both `single_worker`
and `pool_max_2`. Cancellation, fatal-exit recovery, and post-cycle replay succeeded; `ps` RSS sampling was unavailable.
This is a historical local capture. A new scale claim must record the command, revision, module limit, platform, Lean
version, row count, and RSS availability. Re-capture before declaring a regression.

## Import-heavy test inventory

Use this inventory when adding or selecting tests:

| Test surface | Category | Safe routine runner | Notes |
| --- | --- | --- | --- |
| `lean-rs`, `lean-toolchain`, `lean-rs-worker-protocol` unit tests | cheap/unit | `cargo test -p <crate>` or nextest | No full host-session imports. |
| `lean-rs-host` unit tests that do not call `LeanCapabilities::session` | cheap/unit | nextest or focused `cargo test` | Same-process host import guard does not affect these. |
| `crates/lean-rs-host/tests/session_leak_loop.rs` small tests | bounded per-test imports or pool reuse | `cargo nextest run -p lean-rs-host --test session_leak_loop --profile ci` | Each selected test process resets Lean state. |
| `session_create_drop_loop_long`, `pool_acquire_release_loop_long` | import-heavy diagnostic | ignored plus `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1` | Explicitly budgeted diagnostics; not part of the current ASan job or routine smoke tests. |
| `crates/lean-rs-host/tests/batching_and_pool.rs` | many host-session service tests | `cargo nextest run -p lean-rs-host --test batching_and_pool --profile ci` | Do not run the whole integration binary through plain `cargo test`; use nextest isolation. |
| `long_session_memory` example and profiling collectors | profiling-only | `cargo check` locally; default RSS cap is 1,572,864 KiB; tune with `LEAN_RS_LONG_SESSION_MAX_RSS_KIB` | Runtime profiling must be capped and opt-in. |
| Worker child/parent pool and capability tests | worker process boundary | `cargo test -p lean-rs-worker-child pool`, `cargo test -p lean-rs-worker-child capability_builder`, or nextest | Fresh imports happen in worker children or isolated test processes. |

## Per-process Lean threads

The Lean task manager starts in `LeanRuntime::init`. By default worker count is Lean's compiled-in heuristic (typically
one worker per hardware core). Set `LEAN_RS_NUM_THREADS` to a positive integer **before** the first `init` call to pin
the count for the process; invalid values fall back to the Lean default with a `tracing::warn!`. See the
`LeanRuntime::init` docstring at [`crates/lean-rs/src/runtime/init.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/src/runtime/init.rs) for the
full contract.

## CI

CI runs the `ci` nextest profile (`cargo nextest run --workspace --profile ci`), which adds one automatic retry on
transient failures. See [`.github/workflows/ci.yml`](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/ci.yml).

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
225 tests, macOS `memorystatus` killed the process after about 150 tests at roughly 30 GB compressed memory. Those counts
are historical observations, not a current suite size or memory limit.

`cargo nextest run` runs each test in its own process, so cumulative Lean state resets at every process boundary. The
same memory fact applies outside tests: long-running applications should use the worker crates with restart/RSS policy,
or a same-process `SessionPool` with a fresh-import memory policy, when they may open many distinct import sets.

Shared per-binary host state also conflicts with [thread affinity](architecture/concurrency.md): `LeanHost` is
`!Send + !Sync`, while static `OnceLock`/`LazyLock` storage requires `Sync`.

The host-session guard is intentionally narrow: it only blocks `LeanCapabilities::session`, `session_with_profile`, and
`profiling_session` from Cargo/libtest binaries. It does not affect normal `cargo run` profiling commands, worker child
processes, bracketed no-extension queries, or nextest runs.
