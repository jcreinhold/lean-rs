# Long-Session Memory

Repeated fresh `Lean.importModules` calls in one process drive resident set
size from tens of MiB into the GiB range and never give it back. Steady
operations against a *reused* imported environment (bulk introspection,
elaboration, kernel checks, `MetaM`) do not accumulate. The shipped consumer
pattern follows directly: pool imported environments, and cycle the worker
process when an import sweep exhausts the pool.

The rest of this document records what `Drop` reclaims, what the reproducer
measures, and what shape the measurement has on the supported toolchains.

## Retention Model

`lean-rs` has several lifetime boundaries. `Drop` reclaims only the
Rust-owned Lean reference counts attached to those boundaries; it does not
reset Lean's process-global runtime state.

| Lifetime           | Owned values                                                                        | `Drop` reclaims                                | Process-lifetime residue                                       |
| ------------------ | ----------------------------------------------------------------------------------- | ---------------------------------------------- | -------------------------------------------------------------- |
| Process            | Lean runtime, task manager, module-initializer globals, allocator state             | nothing                                        | all of the above, plus core/module init                        |
| `LeanRuntime`      | ZST anchor from `LeanRuntime::init()`                                               | nothing (intentionally process-lifetime)       | same as process                                                |
| `LeanLibrary`      | `dlopen` handle for one Lake-built dylib                                            | the Rust handle                                | initializer globals already run by the loaded module           |
| `LeanCapabilities` | user + shim libraries, resolved capability symbols                                  | library handles, symbol-table storage          | Lean-side initialized module state                             |
| `LeanSession`      | one imported `Lean.Environment` as `Obj<'lean>`                                     | one refcount on the environment (`lean_dec`)   | anything Lean retained globally while importing those modules  |
| `Obj<'lean>`       | one owned Lean refcount                                                             | that refcount; clones balance with `lean_inc`  | persistent and compacted Lean objects (no refcount used)       |
| `SessionPool` slot | a retained environment `Obj<'lean>` keyed by imports                                | dropped when evicted, drained, or pool dropped | process-global Lean import/module state                        |

The split has direct support in Lean's own sources. The runtime reference
requires foreign code to initialize the runtime before calling any Lean
code, and the FFI reference describes Lean-owned objects as
reference-counted. `initialize/init.cpp` initializes core libraries once
per process; `include/lean/lean.h` documents compact-region and persistent
objects as bypassing normal reference counting; `library/module.cpp` loads
`.olean` data through compacted regions. Module-initializer state is
idempotent on both the C++ side (`_G_initialized` short-circuit, used by
`crates/lean-rs/src/module/initializer.rs`) and the interpreted side
(`Compiler.InitAttr` tracks already-run initializers).

References:

- Lean Reference Manual, [Run-Time Code](https://lean-lang.org/doc/reference/latest/Run-Time-Code/)
- Lean Reference Manual, [Foreign Function Interface](https://lean-lang.org/doc/reference/latest/Run-Time-Code/Foreign-Function-Interface/)

## Reproducer

The retained-memory counterpart to the latency benches. It runs one
long-lived process through fresh imports, pooled reuse, bulk introspection,
and elaboration, printing RSS checkpoints in KiB (`ps -o rss=`) and
`SessionPool` counters between phases.

```sh
cd fixtures/lean && lake build && cd -
LEAN_RS_NUM_THREADS=1 cargo run --release -p lean-rs-host --example long_session_memory
```

The default workload is RSS-shaped, not latency-shaped:

- 192 fresh import/drop acquisitions through `SessionPool::with_capacity(runtime, 0)`.
- 192 bounded-pool acquisitions through `SessionPool::with_capacity(runtime, 4)`.
- 512 `query_declarations_bulk` calls over 16 fixture names.
- 512 elaboration calls, 50/50 success vs. diagnostic-producing terms.
- Snapshots before/after runtime init, host/capability load, import loops,
  bulk introspection, elaboration, drop, and a short steady-state pause.

Environment overrides:

```sh
LEAN_RS_LONG_SESSION_IMPORTS=64 \
LEAN_RS_LONG_SESSION_BULK=512 \
LEAN_RS_LONG_SESSION_ELAB=512 \
LEAN_RS_LONG_SESSION_POOL_CAPACITY=4 \
LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY=16 \
LEAN_RS_NUM_THREADS=1 \
cargo run --release -p lean-rs-host --example long_session_memory
```

This is not a Criterion bench by design. Criterion answers per-iteration
latency questions; this workload answers whether RSS returns at lifetime
boundaries after `LeanSession`, `SessionPool`, and `Obj<'lean>` drops. A
single long-running process with named checkpoints answers that directly.

## Measured Shape

The numbers below are local snapshots on macOS aarch64 against
`lean=4.29.1`. They are not portable: macOS RSS is noisy under memory
pressure and compression, and absolute KiB values vary between machines.
The *shape*—order-of-magnitude growth during fresh imports, flat reuse,
flat introspection and elaboration, no return to baseline after drop—is
the load-bearing claim and reproduces across the supported window.

Fresh-import-then-drop, capacity 0:

| Checkpoint               | RSS KiB    |
| ------------------------ | ---------: |
| `start`                  |      5,056 |
| `after_runtime_init`     |     47,648 |
| `after_host_capabilities`|     50,496 |
| `fresh_import_drop_16`   |  3,726,752 |
| `fresh_import_drop_32`   |  3,849,856 |
| `fresh_import_drop_48`   |  2,901,984 |
| `fresh_import_drop_64`   |  2,386,784 |

Sixteen imports of the fixture workload move RSS from ~50 MiB into the
multi-GiB range, despite a pool capacity of zero and every session
environment being dropped immediately.

Full sweep with `LEAN_RS_LONG_SESSION_IMPORTS=64`:

| Phase                          | Imports performed | Reuses | RSS KiB (entry → exit)   |
| ------------------------------ | ----------------: | -----: | ------------------------ |
| Fresh import/drop, capacity 0  |                64 |      0 | 4,372,352 → 4,121,040    |
| Bounded pool, capacity 4       |                 4 |     64 | 3,699,472 → 3,696,816    |
| Bulk introspection             |                 0 |     16 | 3,662,176 → 3,662,224    |
| Elaboration (256 ok, 256 fail) |                 0 |      1 | 3,668,672 → 3,532,464    |
| After drops + steady-state     |               n/a |    n/a | 3,532,464 → 3,487,472    |

The pool counters confirm the Rust-side bookkeeping:

```text
fresh_import_drop  imports=64 reused=0  acquired=64 released_to_pool=0  released_dropped=64
bounded_pool       imports=4  reused=64 acquired=68 released_to_pool=68 released_dropped=0
mixed_before_drop  imports=1  reused=0  acquired=1  released_to_pool=1  released_dropped=0
```

Three attributable findings:

- **Fresh imports drive growth.** Repeated `Lean.importModules` calls grow
  RSS even when every imported environment is dropped immediately.
- **Reuse is flat.** Bulk introspection (512 calls), elaboration (512
  calls), and bounded-pool reuse (64 acquires across 4 cached
  environments) add no measurable RSS.
- **Drop does not return RSS to baseline.** Dropping the pool and session
  after the sweep does not reclaim the imported-module residue.

Open questions the reproducer does not answer: how the retained bytes
split between interned names, globally registered module state, compacted
`.olean` regions, and mimalloc arena retention; behaviour on mathlib or
other large downstream module sets; cross-toolchain variation beyond the
current resolved version.

## Recycling API

`SessionPool::drain()` is the shipped recycle surface. It drops every
cached free-list environment, increments `PoolStats::{drains, drained}`,
and leaves checked-out `PooledSession` values valid. It is useful at idle
boundaries when a worker wants to release Rust-owned cached environments
without discarding the pool object. It is *not* an RSS reset: the
process-global Lean state in the retention model above remains live until
the process exits.

`LeanRuntime::recycle()` and `LeanCapabilities::reopen()` are not
provided. The safe Rust lifetime model treats `LeanRuntime::init()` as a
process-once anchor: every `Obj<'lean>`, `LeanSession<'lean, '_>`,
`LeanCapabilities<'lean, '_>`, and `SessionPool<'lean>` value is tied to
that borrow. An in-process recycle would have to prove that no
Lean-derived value, cached symbol address, thread-local runtime
attachment, task, initializer global, or persistent object from the old
runtime can be observed after reinitialization; Lean's embedding surface
provides no global stop-the-world / finalize / reinitialize contract
strong enough to back that claim. Reopening capabilities was rejected for
a separate reason: the measured growth is not attributable to loaded
dylib handles or symbol-resolution caches.

## Consumer Pattern

Reuse imported environments. Keep a small `SessionPool` keyed by the
import set and run introspection, elaboration, kernel checks, and `MetaM`
calls against pooled sessions. Steady operations against a warm
environment do not accumulate RSS.

Avoid repeatedly creating fresh imported environments in one process. If
a workload must sweep many distinct import sets, put a process boundary
around the sweep: restart the worker after a bounded number of fresh
imports. Sixty-four fresh imports of the fixture workload were already
enough to push RSS into the multi-GiB range; use a lower limit for larger
module sets.

Call `SessionPool::drain()` when a worker is idle, when a project closes,
or before handing a worker to a different stable import set. Drain
cadence is policy for the embedding application: drain releases cached
environments, but cannot bound workloads that continuously create fresh
import sets. Those still require cycling the worker process at a bounded
import count or RSS ceiling.

`lean-rs-worker` provides that process-cycling policy. Its restart policy
can cycle explicitly, before a configured request count, before a
configured import-like request count, after an idle interval, or when a
best-effort child RSS sample reaches a ceiling. The worker memory
reproducer is:

```sh
cargo build -p lean-rs-worker --bin lean-rs-worker-child
LEAN_RS_WORKER_MEMORY_IMPORTS=6 \
LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=2 \
cargo run -p lean-rs-worker --example memory_cycling
```

On a local macOS aarch64 run, the worker cycled after every
two import-like fixture requests. Child RSS moved from about 345 MiB after
the first request in each child to about 717 MiB after the second request,
then returned to about 345 MiB in the replacement child. This supports the
operational claim: process cycling bounds retained RSS for this workload;
in-process drain does not reset it.

`LeanWorkerPool` applies the same memory fact at the local orchestration
layer. Pool policy can reject new distinct workers when known total child
RSS reaches a budget, cycle a warm worker when its sampled RSS reaches a
per-worker ceiling, cycle idle workers, and bound admission waits for a
full pool. RSS sampling remains best effort and platform-specific: an
unavailable sample is recorded as unavailable, not treated as proof that
the pool is under budget. The pool memory-scheduling workload is:

```sh
cargo build -p lean-rs-worker --bin lean-rs-worker-child
cargo run -p lean-rs-worker --example pool_memory_scheduling
```

Use the pool knobs to avoid multiplying Lean import RSS across many local
children. They do not change the underlying reset rule: only process exit
resets Lean process-global retained memory.
