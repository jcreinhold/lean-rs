# Long-Session Memory

Repeated fresh `Lean.importModules` calls in one process drive resident set size from tens of MiB into the GiB range and
never give it back. Steady operations against a *reused* imported environment (bulk introspection, elaboration, kernel
checks, `MetaM`) do not accumulate. The shipped consumer pattern follows directly: pool imported environments, and cycle
the worker process when an import sweep exhausts the pool.

The rest of this document records what `Drop` reclaims, what the reproducer measures, and what shape the measurement has
on the supported toolchains.

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
| `SessionPool` slot | a retained environment `Obj<'lean>` keyed by imports | dropped when evicted, drained, or pool dropped | process-global Lean import/module state |

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

The default workload is bounded so it can be run on a developer machine without reproducing the OOM failure mode:

- 4 fresh import/drop acquisitions through a zero-capacity pool with `max_fresh_imports=4`.
- 4 bounded-pool acquisitions through a pool with capacity 4 and the same fresh-import budget.
- 64 `query_declarations_bulk` calls over 16 fixture names.
- 64 elaboration calls, 50/50 success vs. diagnostic-producing terms.
- Snapshots before/after runtime init, host/capability load, import loops, bulk introspection, elaboration, drop, and a
  short steady-state pause.

The old crash-shaped workload remains opt-in: raise `LEAN_RS_LONG_SESSION_IMPORTS`, `LEAN_RS_LONG_SESSION_BULK`, and
`LEAN_RS_LONG_SESSION_ELAB` only after the previous run's peak RSS is acceptable. Set
`LEAN_RS_LONG_SESSION_MAX_RSS_KIB` to make the same-process pool refuse the next fresh import before crossing a local
RSS ceiling.

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
outside compacted `.olean` regions. On unsupported or noisy platforms, RSS remains best-effort and should be reported
as unavailable or approximate.

Normal host sessions now default to the `Private` full-session profile: `importAll := false`, import level `.private`,
and `loadExts := true`. This is a pre-1.0 behavior correction for unacceptable memory growth, not a
compatibility-preservation exercise. `ExportedPublic` and `Server` are narrower on paper, but current fixture imports
are non-`module` files and Lean rejects exported/server-level imports of them with
`cannot import non-\`module\` ... from \`module\``. The old behavior remains available only as the explicit
`FullPrivateCompat` profile: `importAll := true`, import level `.private`, and `loadExts := true`.

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

`loadExts := true` remains required for full host-service semantics. It also means
`Environment.freeRegions` is not a safe reclamation tool after import: persistent extension data may still reference
objects in those compacted regions. This baseline records the environment shape that caused a RSS checkpoint; it does
not reclaim regions.

## Bracketed Lightweight Queries

`LeanCapabilities::bracketed_import_query` is a separate experimental path for one-shot declaration metadata queries.
It imports with `importAll := false`, import level `.private`, and `loadExts := false`, serializes declaration
existence, declaration kind, module ownership, raw type text, and import stats, and then calls
`Environment.freeRegions` before returning to Rust. The returned values are owned Rust data; no `Environment`, `Expr`,
`ConstantInfo`, `Name`, extension state, or capability handle escapes the bracket.

This path is deliberately not a `LeanSession` replacement. Elaboration, parser-backed source/range queries,
proof-state queries, notation-aware pretty-printing, and capability workflows remain full-session operations because
they require loaded environment extensions. The profiling workload `bracketed-lightweight` prints
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

Full sessions still import with `loadExts := true`, so they cannot use `Environment.freeRegions`. Prompt 19 narrows a
different cost: work derived from the already imported environment after a caller asks a query. The profiling workload
`derived-indexes` prints `query_derived_work=...` rows for query phases such as cheap declaration inspection, raw and
pretty statement rendering, opt-in proof-search facts, declaration search with and without source ranges, module
proof-state queries, proof attempts, and declaration verification.

Default declaration inspection no longer computes proof-search facts. A caller must explicitly request them with the
inspection request's `proof_search` field. When the field is off, the returned proof-search facts say they were not
computed instead of reporting complete false data. This prevents metadata-only callers from touching simp/proof-search
extension state by accident. Cheap attributes such as reducibility remain available through the normal `attributes`
field; proof-search-derived attributes such as `simp`, `rw`, `instance`, and `class` are reported only when
`proof_search` is requested.

The derived-work rows distinguish source-range extension lookups, docstring lookups, raw type rendering,
notation-aware pretty printing, proof-search fact collection, simp-extension lookup, parser/elaborator execution,
module snapshot construction, and Lean's `lazy discriminator import initialization` profiler span. `LazyDiscrTree`
laziness is derived-index laziness over imported module data. It is not lazy `.olean` loading, does not reduce
`env.header.regions`, and does not make compacted regions safe to free after `loadExts := true`.

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

Post-reduction rebaseline on 2026-06-08, commit `396bdf3`, macOS aarch64, Lean 4.31.0-rc1, profiling profile,
`LEAN_RS_NUM_THREADS=1`, and a local `2,097,152` KiB RSS budget. Current repo defaults are stricter:
`test-threads=1`, `build.jobs=1`, `RUST_TEST_THREADS=1`, `LEAN_RS_LEAN_MAX_MEMORY_KIB=1572864`, and profiling RSS caps
of 1,572,864 KiB.

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
load-bearing claim and reproduces across the supported window.

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

Full sweep with `LEAN_RS_LONG_SESSION_IMPORTS=64`:

| Phase | Imports performed | Reuses | RSS KiB (entry → exit) |
| --- | ---: | ---: | --- |
| Fresh import/drop, capacity 0 | 64 | 0 | 4,372,352 → 4,121,040 |
| Bounded pool, capacity 4 | 4 | 64 | 3,699,472 → 3,696,816 |
| Bulk introspection | 0 | 16 | 3,662,176 → 3,662,224 |
| Elaboration (256 ok, 256 fail) | 0 | 1 | 3,668,672 → 3,532,464 |
| After drops + steady-state | n/a | n/a | 3,532,464 → 3,487,472 |

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

Open questions the reproducer does not answer: how the retained bytes split between interned names, globally registered
module state, compacted `.olean` regions, and mimalloc arena retention; behaviour on mathlib or other large downstream
module sets; cross-toolchain variation beyond the current resolved version.

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

## Consumer Pattern

Reuse imported environments. Keep a small `SessionPool` keyed by the import set and run introspection, elaboration,
kernel checks, and `MetaM` calls against pooled sessions. For same-process hosts that may see many distinct import
sets, configure `SessionPoolMemoryPolicy` so cache-miss imports fail before the process crosses a fresh-import or RSS
budget. Steady operations against a warm environment do not accumulate RSS.

Avoid repeatedly creating fresh imported environments in one process. If a workload must sweep many distinct import
sets, put a process boundary around the sweep: restart the worker after a bounded number of fresh imports. Sixty-four
fresh imports of the fixture workload were already enough to push RSS into the multi-GiB range; use a lower limit for
larger module sets.

Call `SessionPool::drain()` when a worker is idle, when a project closes, or before handing a worker to a different
stable import set. Drain cadence is policy for the embedding application: drain releases cached environments, but cannot
bound workloads that continuously create fresh import sets. Those still require cycling the worker process at a bounded
import count or RSS ceiling.

The worker crates provide that process-cycling policy. Its restart policy can cycle explicitly, before a configured
request count, before a configured import-like request count, after an idle interval, or when a best-effort child RSS
sample reaches a ceiling. Memory guardrail errors include the latest Lean import stats when a session has opened in
that process, so an RSS refusal can be read next to the retained `.olean` region shape that preceded it.

Worker children also accept `LEAN_RS_LEAN_MAX_MEMORY_KIB` as an opt-in Lean runtime memory limit. Lean checks this
limit periodically and can throw before the OS kills the process. It is not reclamation: it does not free compacted
regions, extension state, interned names, allocator arenas, or runtime globals, and process cycling remains the reset
boundary.

The worker memory reproducer is:

```sh
cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
LEAN_RS_WORKER_MEMORY_IMPORTS=6 \
LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=1 \
LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB=1572864 \
cargo run -p lean-rs-worker-child --example memory_cycling
```

On the 2026-06-08 macOS aarch64 rebaseline with a 2 GiB cap, cycling after every fresh full-session import kept each
child near 1,194,368 KiB. Allowing two fresh imports in one child reached 3,236,416 KiB, above that local budget. The
current 1,572,864 KiB default keeps the one-import policy but avoids automatically widening to the two-import candidate.
This supports the operational claim: process cycling bounds retained RSS for this workload; in-process drain does not
reset it.

`LeanWorkerPool` applies the same memory fact at the local orchestration layer. Pool policy can reject new distinct
workers when known total child RSS reaches a budget, cycle a warm worker when its sampled RSS reaches a per-worker
ceiling, cycle idle workers, and bound admission waits for a full pool. RSS sampling remains best effort and
platform-specific: an unavailable sample is recorded as unavailable, not treated as proof that the pool is under budget.

Cold import admission is intentionally local to the existing Rust boundaries:

| Path | Admission shape |
| --- | --- |
| Direct `LeanCapabilities::session` | no pooling; caller owns process budget |
| Same-process `SessionPool` cache miss | `SessionPoolMemoryPolicy` refuses before `Lean.importModules` |
| Same-process `SessionPool` cache hit | warm reuse; no fresh import admission |
| `LeanWorkerCapabilityBuilder::open` | single worker child; `LeanWorkerRestartPolicy` checks import-like session open before it enters the child |
| Explicit worker `open_session` / `open_session_with_imports` | supervisor records import-like admission and applies restart/RSS policy first |
| `LeanWorkerPool::acquire_lease` with a new session key | `&mut LeanWorkerPool` serializes one pool; `max_workers` and total child RSS are checked before `builder.open()` |
| Lease command after `session_missing` | reopens through the same worker supervisor admission path |

There is no global import semaphore and no `max_concurrent_imports` knob in this baseline. One pool cannot internally
run concurrent cold opens because lease acquisition takes `&mut LeanWorkerPool`; multiple independent pools are a
caller-level concurrency decision and should be bounded by their own process policy and RSS budget.
The pool memory-scheduling workload is:

```sh
cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
LEAN_RS_POOL_MEMORY_MAX_WORKERS=1 \
LEAN_RS_POOL_MEMORY_TOTAL_RSS_KIB=1572864 \
LEAN_RS_POOL_MEMORY_PER_WORKER_RSS_KIB=1572864 \
LEAN_RS_POOL_MEMORY_MAX_IMPORTS=1 \
cargo run -p lean-rs-worker-child --example pool_memory_scheduling
```

On the same run, the pool fixture used one worker, peaked at 419,616 KiB child RSS, and held the parent process around
2,144 KiB to 3,456 KiB. Cold pool requests were roughly 215 ms; warm pool reuse was roughly 5-6 ms. The profiling rows
named `admission=...` report cold-open attempts, admitted opens, typed refusals, observed concurrent cold opens, import-like
requests, and RSS before/after admission. Use the pool knobs together to avoid multiplying Lean import RSS across many
local children. They do not change the underlying reset rule: only process exit resets Lean process-global retained
memory.
