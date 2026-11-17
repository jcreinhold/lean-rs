# Long-session memory

Repeated fresh `Lean.importModules` calls can retain GiB of process-lifetime state even after sessions drop. The
measured warm-session workloads stayed approximately flat. Reuse imported environments and bound fresh imports per child
generation; process exit resets the retained state. [Production hosting](../production-hosting.md) owns the operational
pattern. This page preserves the retention model, diagnostic methods, and recorded measurements.

## Measurement limits

RSS includes clean file-backed `.olean` pages, so it can fall under pressure while unreclaimable memory grows. In the
recorded Mathlib-scale downstream sample, ΔRSS / Δ`phys_footprint` was 5.78–5.83 on first import, then reached 0.00
while retained memory grew by 2.3–5.4 GB per import. macOS physical footprint comes from
`proc_pid_rusage(RUSAGE_INFO_V2).ri_phys_footprint`; Linux `Private_Dirty` from `smaps_rollup` is a separate
private-page measure. Neither is the same quantity as RSS. The current worker RSS policy remains best effort.

`LeanWorkerImportStats::non_memory_mapped_region_bytes` predicted Δ`phys_footprint` in that sample with slope 1.091 and
R² 0.962 (n = 12, first import of each process excluded). This is workload evidence for the residue counter, not a
universal conversion from import stats to process memory.

The fixture tables below use import sets sharing about 99% of their closure, since each imports `Lean`. On the recorded
real per-file headers, mmap byte fraction was 0.994 on first import and 0.000–0.29 later: `.olean` compacted regions are
position-dependent and each `importModules` gets a fresh `ImportState`. Recorded residue was 10–18 MB on first import
and 2.0–4.5 GB later. Five near-identical profiles retained 16.0 GiB; five unrelated profiles retained 9.6 GiB. The
fixture shows retention and reuse behavior; its magnitudes do not predict a downstream peak.

## Retention Model

`lean-rs` has several lifetime boundaries. `Drop` reclaims only the Rust-owned Lean reference counts attached to those
boundaries; it does not reset Lean's process-global runtime state.

| Lifetime | Owned values | `Drop` reclaims | Process-lifetime residue |
| --- | --- | --- | --- |
| Process | Lean runtime, task manager, module-initializer globals, allocator state | nothing | all of the above, plus core/module init |
| `LeanRuntime` | ZST anchor from `LeanRuntime::init()` | nothing (intentionally process-lifetime) | same as process |
| `LeanLibrary` | `dlopen` handle for one Lake-built dylib | the Rust handle | initializer globals already run by the loaded module |
| `LeanCapabilities` | user + shim libraries, resolved capability symbols | library handles, symbol-table storage | Lean-side initialized module state |
| `LeanSession` | one imported `Lean.Environment` as `Obj<'lean>` | one refcount on the environment (`lean_dec`) | anything Lean retained globally while importing those modules |
| `Obj<'lean>` | one owned Lean refcount | that refcount; clones balance with `lean_inc` | persistent and compacted Lean objects (no refcount used) |
| `SessionPool` slot | a retained environment `Obj<'lean>` keyed by canonical project root, ordered imports, and import profile | dropped when evicted, drained, or pool dropped | process-global Lean import/module state |
| Worker-child session pool slot | one parked `HostSessionState` keyed by `(project_root, mode, imports, import_profile)` | dropped when evicted past `LEAN_RS_WORKER_SESSION_POOL_CAPACITY` or at child exit | same as above; measured at ~70 MiB per extra *live* environment on real project headers (30–50 MB on the fixture), against 2.0–4.5 GB per import |

The split has direct support in Lean's own sources. The runtime reference requires foreign code to initialize the
runtime before calling any Lean code, and the FFI reference describes Lean-owned objects as reference-counted.
`initialize/init.cpp` initializes core libraries once per process; `include/lean/lean.h` documents compact-region and
persistent objects as bypassing normal reference counting; `library/module.cpp` loads `.olean` data through compacted
regions. Module-initializer state is idempotent on both the C++ side (`_G_initialized` short-circuit, used by
`crates/lean-rs/src/module/initializer.rs`) and the interpreted side (`Compiler.InitAttr` tracks already-run
initializers).

References:

- Lean Reference Manual, [Run-Time Code](https://lean-lang.org/doc/reference/latest/Run-Time-Code/)
- Lean Reference Manual,
  [Foreign Function Interface](https://lean-lang.org/doc/reference/latest/Run-Time-Code/Foreign-Function-Interface/)

## Reproducer

The retained-memory counterpart to the latency benches. It runs one long-lived process through fresh imports, pooled
reuse, bulk introspection, and elaboration, printing RSS checkpoints in KiB (`ps -o rss=`) and `SessionPool` counters
between phases.

```sh
cd fixtures/lean && lake build && cd -
LEAN_RS_NUM_THREADS=1 cargo run --release -p lean-rs-host --example long_session_memory
```

The example's fallback workload is bounded. Repository Cargo configuration overrides the import count to one; inspect
[`.cargo/config.toml`](https://github.com/jcreinhold/lean-rs/blob/main/.cargo/config.toml) and explicit environment
overrides when reproducing a result:

- 4 fresh import/drop acquisitions through a zero-capacity pool with `max_fresh_imports=4`.
- 4 bounded-pool acquisitions through a pool with capacity 1 and the same fresh-import budget.
- 64 `query_declarations_bulk` calls over 16 fixture names.
- 64 elaboration calls, 50/50 success vs. diagnostic-producing terms.
- Snapshots before/after runtime init, host/capability load, import loops, bulk introspection, elaboration, drop, and a
  short steady-state pause.

The old crash-shaped workload remains opt-in: raise `LEAN_RS_LONG_SESSION_IMPORTS`, `LEAN_RS_LONG_SESSION_BULK`, and
`LEAN_RS_LONG_SESSION_ELAB` only after the previous run's peak RSS is acceptable. Set `LEAN_RS_LONG_SESSION_MAX_RSS_KIB`
to make the same-process pool refuse the next fresh import before crossing a local RSS ceiling.

Environment overrides:

```sh
LEAN_RS_LONG_SESSION_IMPORTS=64 \
LEAN_RS_LONG_SESSION_BULK=512 \
LEAN_RS_LONG_SESSION_ELAB=512 \
LEAN_RS_LONG_SESSION_POOL_CAPACITY=4 \
LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY=16 \
LEAN_RS_LONG_SESSION_MAX_RSS_KIB=1572864 \
LEAN_RS_NUM_THREADS=1 \
cargo run --release -p lean-rs-host --example long_session_memory
```

One mode is a driver rather than a phase. `LEAN_RS_LONG_SESSION_MODE=pooled-distinct` forks three fresh child processes
— pooled, truncated, and alternating — over `LEAN_RS_LONG_SESSION_IMPORTS` distinct import sets (capped at the number
the fixture defines) and `LEAN_RS_LONG_SESSION_ROUNDS` rounds, then prints their peak RSS and the pooled−truncated
delta. Fresh processes are the point: the comparison is between two end states at an equal import count, which one
process sampled over time cannot give.

```sh
LEAN_RS_LONG_SESSION_MODE=pooled-distinct \
LEAN_RS_LONG_SESSION_IMPORTS=4 \
LEAN_RS_LONG_SESSION_ROUNDS=4 \
LEAN_RS_NUM_THREADS=1 \
cargo run --release -p lean-rs-host --example long_session_memory
```

The profiling wrappers under `profiling/scripts/` run the bounded variants and can be recorded with `samply`:

```sh
./profiling/scripts/profile_memory.sh long-session
./profiling/scripts/profile_memory.sh long-session-matrix
./profiling/scripts/profile_with_samply.sh long-session
```

This is not a Criterion bench by design. Criterion answers per-iteration latency questions; this workload answers
whether RSS returns at lifetime boundaries after `LeanSession`, `SessionPool`, and `Obj<'lean>` drops. A single
long-running process with named checkpoints answers that directly.

## Import Attribution

The reproducer now prints Lean-native `import_stats=...` rows next to RSS checkpoints. These rows are structured data
from the imported `Environment`, not parsed console output from `Environment.displayStats`. They report:

- direct import names from `env.header.imports`;
- effective module count from `env.header.modules`;
- compacted and memory-mapped region counts from `env.header.regions`;
- total compacted-region bytes, mmap-backed compacted-region bytes, and non-mmap compacted-region bytes from
  `env.header.regions`;
- imported bytes as a compatibility alias for total compacted-region bytes;
- imported constant count from `env.constants`;
- persistent extension name count and total imported extension entries from loaded module data;
- the import mode: `importAll`, import level, and `loadExts`.

Lean exposes each `CompactedRegion`'s size and whether it was memory-mapped, but it does not expose a stable imported
module owner for each region. The attribution is therefore aggregate: it can say how much compacted `.olean` data was
retained and how much of that was mmap-backed versus heap-loaded fallback data, but not which module owns an individual
region.

Profiling reports also show the gap between Lean-attributed compacted-region bytes and process RSS when RSS is
available. That gap is not a leak proof or a safety proof. Allocator arenas, initialized environment extensions,
interned names, task/runtime state, native module globals, Rust heap data, and ordinary Lean heap objects can all live
outside compacted `.olean` regions. On unsupported or noisy platforms, RSS remains best-effort and should be reported as
unavailable or approximate.

Normal host sessions now default to the `Private` full-session profile: `importAll := false`, import level `.private`,
and `loadExts := true`. This is a pre-1.0 behavior correction for unacceptable memory growth, not a
compatibility-preservation exercise. `ExportedPublic` and `Server` are narrower on paper, but current fixture imports
are non-`module` files and Lean rejects exported/server-level imports of them with
`cannot import non-module ... from module`. The old behavior remains available only as the explicit `FullPrivateCompat`
profile: `importAll := true`, import level `.private`, and `loadExts := true`.

The import-mode matrix compares the same fixture imports across:

| Matrix mode | `importAll` | Import level | `loadExts` |
| --- | --- | --- | --- |
| `exported-public` | `false` | `.exported` | `true` |
| `server` | `false` | `.server` | `true` |
| `private` | `false` | `.private` | `true` |
| `full-private-compat` | `true` | `.private` | `true` |
| `exported-no-exts` | `false` | `.exported` | `false` |

The focused profile gates pass for declaration lookup, declaration source ranges, pretty declaration inspection,
elaboration, proof-state/module-query batches, and capability-backed worker session opening under `private`. Profiling
reports should compare `private` against `full-private-compat` under an explicit RSS cap before claiming local RSS
improvements.

Local capped matrix run on 2026-06-07, macOS aarch64, Lean 4.31.0-rc1, `LEAN_RS_LONG_SESSION_IMPORTS=1`,
`LEAN_RS_LONG_SESSION_MAX_RSS_KIB=4194304`:

| Profile | Result | Checkpoint RSS KiB | `importAll` | Level | `loadExts` | Modules | Regions | mmap | Bytes | Constants | Exts | Entries |
| --- | --- | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `exported-public` | blocked: non-`module` fixture import | 234848 | `false` | `.exported` | `true` | - | - | - | - | - | - | - |
| `server` | blocked: non-`module` fixture import | 1825920 | `false` | `.server` | `true` | - | - | - | - | - | - | - |
| `private` | passed | 3809744 | `false` | `.private` | `true` | 2259 | 9033 | 1017 | 1955323616 | 203924 | 119 | 1732192 |
| `full-private-compat` | passed, then RSS guard refused the next import | 5781488 | `true` | `.private` | `true` | 2259 | 9033 | 0 | 1955323616 | 203924 | 119 | 1732192 |

`loadExts := true` remains required for full host-service semantics. It also means `Environment.freeRegions` is not a
safe reclamation tool after import: persistent extension data may still reference objects in those compacted regions.
This baseline records the environment shape that caused a RSS checkpoint; it does not reclaim regions.

## Bracketed Lightweight Queries

`LeanCapabilities::bracketed_import_query` is a separate experimental path for one-shot declaration metadata queries. It
imports with `importAll := false`, import level `.private`, and `loadExts := false`, serializes declaration existence,
declaration kind, module ownership, raw type text, and import stats, and then calls `Environment.freeRegions` before
returning to Rust. The returned values are owned Rust data; no `Environment`, `Expr`, `ConstantInfo`, `Name`, extension
state, or capability handle escapes the bracket.

This path is deliberately not a `LeanSession` replacement. Elaboration, parser-backed source/range queries, proof-state
queries, notation-aware pretty-printing, and capability workflows remain full-session operations because they require
loaded environment extensions. The profiling workload `bracketed-lightweight` prints
`bracketed_import_stats=... free_regions_ran=true` and RSS checkpoints for:

- before import dispatch;
- after Lean imports with `loadExts := false`;
- after the declaration query, before `freeRegions`;
- after `freeRegions`;
- after Rust receives the owned result;
- after a short pause.

Local capped probe on macOS aarch64 with Lean 4.31.0-rc1:

| Workload | Import RSS KiB | Query-before-free RSS KiB | After-free RSS KiB | Imported bytes | Regions | Constants | Ext entries |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `bracketed-lightweight` (`LeanRsFixture.Handles`, one import) | 1,010,720 | 1,032,032 | 108,048 | 1,955,323,616 | 9,033 | 204,109 | 1,732,192 |

Baseline JSON and Markdown reports under `profiling_results/` preserve the raw KiB checkpoints and render the Lean
Import Stats tables side by side. Treat those KiB values as local measurements only; do not compare absolute RSS across
machines or operating systems.

## Derived Index Attribution

Full sessions still import with `loadExts := true`, so they cannot use `Environment.freeRegions`. Derived-index
attribution narrows a different cost: work derived from the already imported environment after a caller asks a query.
The profiling workload `derived-indexes` prints `query_derived_work=...` rows for query phases such as cheap declaration
inspection, raw and pretty statement rendering, opt-in proof-search facts, declaration search with and without source
ranges, module proof-state queries, proof attempts, and declaration verification.

Default declaration inspection no longer computes proof-search facts. A caller must explicitly request them with the
inspection request's `proof_search` field. When the field is off, the returned proof-search facts say they were not
computed instead of reporting complete false data. This prevents metadata-only callers from touching simp/proof-search
extension state by accident. Cheap attributes such as reducibility remain available through the normal `attributes`
field; proof-search-derived attributes such as `simp`, `rw`, `instance`, and `class` are reported only when
`proof_search` is requested.

The derived-work rows distinguish source-range extension lookups, docstring lookups, raw type rendering, notation-aware
pretty printing, proof-search fact collection, simp-extension lookup, parser/elaborator execution, module snapshot
construction, and Lean's `lazy discriminator import initialization` profiler span. `LazyDiscrTree` laziness is
derived-index laziness over imported module data. It is not lazy `.olean` loading, does not reduce `env.header.regions`,
and does not make compacted regions safe to free after `loadExts := true`.

Historical local capped probe on macOS aarch64 with Lean 4.31.0-rc1, `LEAN_RS_LONG_SESSION_MODE=derived-indexes`,
`LEAN_RS_LONG_SESSION_IMPORTS=1`, and `LEAN_RS_LONG_SESSION_MAX_RSS_KIB=2097152`:

| Phase | Source ranges | Docstrings | Raw types | Pretty prints | Proof facts | Simp ext | Parser/elab | Snapshots | Lazy discr import init |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `cheap_inspection` | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `false` |
| `raw_statement_inspection` | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | `false` |
| `pretty_statement_inspection` | 1 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | `false` |
| `proof_search_inspection` | 1 | 1 | 0 | 1 | 1 | 1 | 0 | 0 | `false` |
| `declaration_search_no_source` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `false` |
| `declaration_search_with_source` | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `false` |
| `module_query_proof_state` | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 | `false` |
| `proof_attempt` | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 | `false` |
| `verify_declaration` | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 | `false` |

## Measured Shape

Post-reduction rebaseline on 2026-06-08, macOS aarch64, Lean 4.31.0-rc1, profiling profile, `LEAN_RS_NUM_THREADS=1`, and
a local `2,097,152` KiB RSS budget. Current repo defaults are stricter: `test-threads=1`, `build.jobs=1`,
`RUST_TEST_THREADS=1`, `LEAN_RS_LEAN_MAX_MEMORY_KIB=1572864`, and profiling RSS caps of 1,572,864 KiB.

| Workload | Command/env shape | Status | Peak RSS KiB | Import profile | Imported bytes | Regions | Ext entries |
| --- | --- | --- | ---: | --- | ---: | ---: | ---: |
| Fresh same-process imports | `LEAN_RS_LONG_SESSION_MODE=fresh-import`, `LEAN_RS_LONG_SESSION_IMPORTS=4`, `LEAN_RS_LONG_SESSION_MAX_RSS_KIB=2097152` | `resource_exhausted` after the second import | 2,225,584 | `private`, `importAll=false`, `loadExts=true` | 1,955,323,616 | 9,033 | 1,732,192 |
| Pooled reuse | `LEAN_RS_LONG_SESSION_MODE=pooled-reuse`, pool capacity 1 | `ok` | 1,192,976 | `private`, `importAll=false`, `loadExts=true` | 1,955,323,616 | 9,033 | 1,732,192 |
| Steady query/elaboration | `LEAN_RS_LONG_SESSION_MODE=steady-state` | `ok` | 1,198,608 | `private`, `importAll=false`, `loadExts=true` | 1,956,159,664 | 9,037 | 1,733,621 |
| Bracketed lightweight | `LEAN_RS_LONG_SESSION_MODE=bracketed-lightweight` | `ok`; after-free RSS settled near 122,480 KiB | 1,044,832 | `private`, `importAll=false`, `loadExts=false` | 1,955,323,616 | 9,033 | 1,732,192 |
| Derived-index probes | `LEAN_RS_LONG_SESSION_MODE=derived-indexes` | `ok` | 1,236,368 | `private`, `importAll=false`, `loadExts=true` | 1,955,250,248 | 9,033 | 1,732,016 |
| Worker cycling candidate | `LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=1`, `LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB=2097152` | `ok`, 5 restarts for 6 imports | 1,194,368 | `private`, `importAll=false`, `loadExts=true` | 1,955,323,616 | 9,033 | 1,732,192 |
| Worker cycling candidate | `LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=2`, same RSS cap | `ok`, but over local cap | 3,236,416 | `private`, `importAll=false`, `loadExts=true` | 1,955,323,616 | 9,033 | 1,732,192 |
| Worker pool fixture | `max_workers=1`, total/per-worker RSS cap 2,097,152 KiB, `max_imports=1` | `ok` | 419,616 | `private`, `importAll=false`, `loadExts=true` | 460,060,392 | 2,638 | 464,688 |

The local worker recommendation from that run is:

```rust
LeanWorkerRestartPolicy::memory_bounded(1, 2_097_152);
LeanWorkerPoolConfig::new(1)
    .per_worker_rss_ceiling_kib(2_097_152)
    .max_total_child_rss_kib(2_097_152);
```

The current checked-in local-safe defaults are stricter and require no shell exports:

```rust
LeanWorkerRestartPolicy::memory_bounded(1, 1_572_864);
LeanWorkerPoolConfig::new(1)
    .per_worker_rss_ceiling_kib(1_572_864)
    .max_total_child_rss_kib(1_572_864);
```

The `max_imports=2` candidate was collected because the one-import run stayed below the 70% widening threshold, but it
then peaked above the 2 GiB budget. For this fixture and machine, one fresh full-session import per child is the
measured local policy. Larger hosts should scale `max_total_child_rss_kib` with `max_workers *
per_worker_rss_ceiling_kib`, and should set both values from their own capped baseline rather than copying the raw KiB
above.

The same-process test command `cargo test -p lean-rs-host session_leak_loop` is not a safe local verification command.
It can run multiple fresh full-session imports inside one Rust test harness process without a `SessionPoolMemoryPolicy`
or worker boundary. Host full-session imports are therefore rejected when started from Cargo's same-process libtest
harness, before Lean import begins. Use nextest process isolation instead. For an explicitly budgeted one-off debug run,
set `LEAN_RS_ALLOW_CARGO_TEST_HOST_IMPORTS=1`; `-- --test-threads=1` only serializes tests inside the same process and
does not reset Lean import state.

The numbers below are local snapshots on macOS aarch64 against `lean=4.29.1`. They are not portable: macOS RSS is noisy
under memory pressure and compression, and absolute KiB values vary between machines. The *shape*—order-of-magnitude
growth during fresh imports, flat reuse, flat introspection and elaboration, no return to baseline after drop—is the
load-bearing claim and was observed in the recorded runs.

Fresh-import-then-drop, capacity 0:

| Checkpoint | RSS KiB |
| --- | ---: |
| `start` | 5,056 |
| `after_runtime_init` | 47,648 |
| `after_host_capabilities` | 50,496 |
| `fresh_import_drop_16` | 3,726,752 |
| `fresh_import_drop_32` | 3,849,856 |
| `fresh_import_drop_48` | 2,901,984 |
| `fresh_import_drop_64` | 2,386,784 |

Sixteen imports of the fixture workload move RSS from ~50 MiB into the multi-GiB range, despite a pool capacity of zero
and every session environment being dropped immediately.

Read the series as evidence that dropping does not return process memory to baseline, not as a growth *rate*: it is not
monotone (3.73 → 3.85 → 2.90 → 2.39 GB), because macOS RSS under compression is noisy enough at this scale to move the
reading in either direction. The marginal cost of one import is measured separately, by the `pooled-distinct` arms
below, which compare fresh processes at a fixed import count instead of sampling one process over time.

Full sweep with `LEAN_RS_LONG_SESSION_IMPORTS=64`:

| Phase | Imports performed | Reuses | RSS KiB (entry → exit) |
| --- | ---: | ---: | --- |
| Fresh import/drop, capacity 0 | 64 | 0 | 4,372,352 → 4,121,040 |
| Bounded pool, capacity 4 | 4 | 64 | 3,699,472 → 3,696,816 |
| Bulk introspection | 0 | 16 | 3,662,176 → 3,662,224 |
| Elaboration (256 ok, 256 fail) | 0 | 1 | 3,668,672 → 3,532,464 |
| After drops + steady-state | n/a | n/a | 3,532,464 → 3,487,472 |

In the 2026-06-08 pool fixture capture, parent RSS ranged from 2,144 to 3,456 KiB; cold requests were about 215 ms and
warm reuse about 5–6 ms. Child peak RSS was 419,616 KiB. These are local workload measurements, not service-level
bounds.

The pool counters confirm the Rust-side bookkeeping:

```text
fresh_import_drop  imports=64 reused=0  acquired=64 released_to_pool=0  released_dropped=64
bounded_pool       imports=4  reused=64 acquired=68 released_to_pool=68 released_dropped=0
mixed_before_drop  imports=1  reused=0  acquired=1  released_to_pool=1  released_dropped=0
```

Three attributable findings:

- **Fresh imports drive growth.** Repeated `Lean.importModules` calls grow RSS even when every imported environment is
  dropped immediately.
- **Reuse is flat.** Bulk introspection (512 calls), elaboration (512 calls), and bounded-pool reuse (64 acquires across
  4 cached environments) add no measurable RSS.
- **Drop does not return RSS to baseline.** Dropping the pool and session after the sweep does not reclaim the
  imported-module residue.

### What one extra *live* environment costs

`LEAN_RS_LONG_SESSION_MODE=pooled-distinct` isolates the quantity the sweep above cannot: how much a *retained*
environment costs beyond the import that produced it. Three arms over N distinct import sets, each in a fresh process,
each sampled after the same settling pause:

| N | Arm | Imports | Live envs | Peak RSS |
| ---: | --- | ---: | ---: | ---: |
| 2 | truncated (capacity 0, stops at N acquires) | 2 | 1 | 1.650 GB |
| 2 | pooled (capacity N) | 2 | 2 | 1.679 GB |
| 2 | alternating (capacity 0, N·M acquires) | 8 | 1 | 8.824 GB |
| 4 | truncated | 4 | 1 | 4.043 GB |
| 4 | pooled | 4 | 4 | 4.190 GB |
| 4 | alternating | 16 | 1 | ~7.9 GB |

`pooled − truncated` is the cost of holding N environments live rather than one: **+29 MB at N=2, +147 MB at N=4**, or
30–50 MB per extra environment against a marginal import cost of 0.8–1.0 GiB. That is 4–6% of one import, and it is the
retention model's own prediction: the import's compacted regions survive the environment either way, so what an extra
live environment costs is the environment, not its regions. Pooled and truncated repeat to within 0.01% across
repetitions; the alternating arm is the noisy one, for the reason recorded above.

`alternating` versus `pooled` at equal work is the product claim: the same workload, 4× fewer imports, 8.82 → 1.68 GB at
N=2.

Open questions the reproducer does not answer: how the retained bytes split between interned names, globally registered
module state, compacted `.olean` regions, and mimalloc arena retention; how these fixture measurements generalize beyond
the separately recorded downstream sample; cross-toolchain variation.

## Recycling API

`SessionPool::drain()` is the shipped recycle surface. It drops every cached free-list environment, increments
`PoolStats::{drains, drained}`, and leaves checked-out `PooledSession` values valid. It is useful at idle boundaries
when a worker wants to release Rust-owned cached environments without discarding the pool object. It is *not* an RSS
reset: the process-global Lean state in the retention model above remains live until the process exits.

`LeanRuntime::recycle()` and `LeanCapabilities::reopen()` are not provided. The safe Rust lifetime model treats
`LeanRuntime::init()` as a process-once anchor: every `Obj<'lean>`, `LeanSession<'lean, '_>`,
`LeanCapabilities<'lean, '_>`, and `SessionPool<'lean>` value is tied to that borrow. An in-process recycle would have
to prove that no Lean-derived value, cached symbol address, thread-local runtime attachment, task, initializer global,
or persistent object from the old runtime can be observed after reinitialization; Lean's embedding surface provides no
global stop-the-world / finalize / reinitialize contract strong enough to back that claim. Reopening capabilities was
rejected for a separate reason: the measured growth is not attributable to loaded dylib handles or symbol-resolution
caches.

## Pooling and cycling

[Production hosting](../production-hosting.md) owns pool sizing, residue/RSS limits, admission, retries, and typed
resource failures. The [profiling guide](https://github.com/jcreinhold/lean-rs/blob/main/profiling/README.md) owns the
bounded worker/pool reproducer commands and report fields. These controls act at lifecycle boundaries; they do not
prevent a single import from overshooting a budget. `LEAN_RS_LEAN_MAX_MEMORY_KIB` changes Lean's process-global limit.
An overrun can abort through an uncaught foreign exception, so it is a contained-process guardrail rather than a
recoverable Rust error or reclamation API.

### Pooled worker sessions

The child reuses its live session when `(project_root, mode, imports, import_profile)` matches; import order matters.
Otherwise it checks a bounded pool of parked sessions before importing. `LeanWorkerConfig::session_pool_capacity` sets
capacity explicitly; when absent, the supervisor derives it from `restart_policy.max_imports` when available. Configure
capacity separately from the restart budget so cycling does not preempt useful warm reuse.

A pooled environment is a snapshot of its import. Rebuilding `.olean` files does not refresh that snapshot. The caller
must track freshness for every retained profile and cycle the worker or select a different validated key when artifacts
change. Reopening the same key alone does not promise a new import.

A later import can register an environment extension and leave an older environment with a short `extensions` array.
Both pools compare an epoch derived from public append-only registries before reuse and evict stale environments. The
epoch includes persistent extensions, scoped extensions, and builtin attributes. This is a correctness boundary: reusing
an environment across a registering import can otherwise cause Lean to index past its extension storage.

`stats.imports` counts imports rather than opens. Snapshot caches survive parking and clear when the corresponding
session drops; cache identity includes mode. A failed open can leave the child sessionless while other parked sessions
remain reusable. See [Worker session adapter](../architecture/17-worker-session-adapter.md) for the supported session
operations and [Worker pool](../architecture/20-worker-pool.md) for lease and admission behavior.

The retained-memory measurements above explain why pooling helps: an extra live environment cost 30–50 MB in the fixture
sample, far less than the 0.8–1.0 GiB additional import residue in that run. These observations support reuse; they do
not make cached environments free or bound an arbitrary workload.

## Process-global Lean state that is not memory

Everything above is about bytes. Two process-global registries are not about bytes at all, but they are retained by the
same rule — nothing but process exit resets them — and they change what a long-lived child *computes*, so they belong in
the same document.

### The extension registry (handled)

`envExtensionsRef` grows whenever an imported module's `initialize` block registers an environment extension, and every
environment created before that point keeps a permanently short `extensions` array. This is a correctness hazard rather
than a memory one, and both pools handle it by dropping the affected environments; see
[Pooled worker sessions](#pooled-worker-sessions) for the mechanism and `LeanSession::extension_registry_epoch` for the
primitive.

### The attribute map (documented, not fixed)

`AttributeExtension.mkInitial` seeds every new environment's attribute map from the process-global `attributeMapRef`,
which only ever grows. A *later* environment therefore accepts attribute syntax registered by modules that only an
*earlier* import brought in. The child is, in that narrow sense, more permissive than a fresh `lean` — and permissively
in a way that depends on the child's import history.

This is not fixable at this layer. There is no unregister; registration is initialization-only; the mechanism assumes
one environment lineage per process. It cannot make a proof unsound: the kernel never consults `attributeMapRef`, so
kernel checking and evidence collection are untouched. What it can do is let an elaboration succeed inside a warm child
and fail in the user's editor.

It predates session pooling — two sequential imports in one process are enough — and pooling neither amplifies nor
relieves it. Including the builtin attribute count in the extension-registry stamp does mean a pool cannot hold an
environment across an attribute registration, which narrows the window without closing it. The only lever that closes it
is `max_imports = 1` (equivalently `LEAN_RS_WORKER_SESSION_POOL_CAPACITY=1` plus a per-import cycle), at the cost of a
full re-import per profile switch.
