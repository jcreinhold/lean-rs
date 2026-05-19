# Long-Session Memory

Run the reproducer from a clean checkout after building the fixture:

```sh
cd fixtures/lean && lake build && cd -
LEAN_RS_NUM_THREADS=1 cargo run --release -p lean-rs-host --example long_session_memory
```

The example prints stable `key=value` lines: active Lean version, workload parameters, `SessionPool` counters, and RSS
checkpoints in KiB as reported by `ps -o rss= -p <pid>`.

## Retention Model

`lean-rs` has several distinct lifetime boundaries. `Drop` only reclaims the Rust-owned Lean reference counts attached
to those boundaries; it does not reset Lean's process-global runtime state.

| Lifetime | Owned values | What `Drop` reclaims | What remains process-lifetime |
| --- | --- | --- | --- |
| Process | Lean runtime, task manager, module initializer globals, Lean allocator state | Nothing; `LeanRuntime` has no finalizer | Runtime initialization, core library/module initialization, task manager, allocator arenas |
| `LeanRuntime` | ZST lifetime anchor returned by `LeanRuntime::init()` | Nothing; it is intentionally process-lifetime | Same as process |
| `LeanLibrary` | `dlopen` handle for one Lake-built dylib | Rust library handle when dropped | Initializer globals already run by the loaded Lean module |
| `LeanCapabilities` | User and shim libraries plus resolved capability symbols | Library handles and symbol table storage | Lean-side initialized module state |
| `LeanSession` | One imported `Lean.Environment` as `Obj<'lean>` | One Lean refcount on the environment via `Obj::Drop` / `lean_dec` | Anything Lean retained globally while importing modules |
| `Obj<'lean>` | One owned Lean reference count | That count via `lean_dec`; clones balance with `lean_inc` | Persistent or compacted Lean objects whose refcount is not used |
| `SessionPool` entry | A retained `Obj<'lean>` environment keyed by imports | Dropped when evicted or when the pool is dropped | Process-global Lean import/module state already created |

Lean source supports the split. The public runtime reference says foreign code must initialize the Lean runtime before
calling Lean code, and the FFI reference describes Lean-owned objects as reference-counted. The local Lean source at
`/Users/jcreinhold/Code/lean4/src/initialize/init.cpp` initializes core libraries once for the process;
`/Users/jcreinhold/Code/lean4/src/include/lean/lean.h` states that compact-region and persistent objects do not use
normal reference counting; `/Users/jcreinhold/Code/lean4/src/library/module.cpp` loads `.olean` data through compacted
regions. Module initializers also carry idempotent process/module state: `crates/lean-rs/src/module/initializer.rs`
documents Lean's `_G_initialized` short-circuit, and Lean's `Compiler.InitAttr` tracks already-run interpreted module
initializers.

References:

- Lean Reference Manual, [Run-Time Code](https://lean-lang.org/doc/reference/latest/Run-Time-Code/)
- Lean Reference Manual, [Foreign Function Interface](https://lean-lang.org/doc/reference/latest/Run-Time-Code/Foreign-Function-Interface/)

## Reproducer Workload

The default workload is intentionally RSS-shaped, not latency-shaped:

- `N = 192` fresh import/drop acquisitions through `SessionPool::with_capacity(runtime, 0)`.
- `N = 192` bounded-pool acquisitions through `SessionPool::with_capacity(runtime, 4)`.
- `M = 512` `query_declarations_bulk` calls over 16 fixture names.
- `K = 512` elaboration calls over a 50/50 mix of successful and diagnostic-producing terms.
- Snapshots before/after runtime init, host/capability load, import loops, bulk introspection, elaboration, drop, and a
  short steady-state pause.

Override knobs are environment-only:

```sh
LEAN_RS_LONG_SESSION_IMPORTS=64 \
LEAN_RS_LONG_SESSION_BULK=512 \
LEAN_RS_LONG_SESSION_ELAB=512 \
LEAN_RS_LONG_SESSION_POOL_CAPACITY=4 \
LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY=16 \
LEAN_RS_NUM_THREADS=1 \
cargo run --release -p lean-rs-host --example long_session_memory
```

This is not a Criterion bench. Criterion is the right surface for operation latency. Here the question is retained RSS
after lifetime boundaries and after `Drop`; a single long-running process with named checkpoints answers that directly.

## Measured Outcome

Measured on macOS aarch64, `lean_version=4.29.1`, `lean_resolved_version=4.29.1`, with
`LEAN_RS_NUM_THREADS=1`.

The default workload reproduced the single-process import pathology before it reached the pooled, bulk, or elaboration
phases. Command:

```sh
LEAN_RS_NUM_THREADS=1 LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY=16 \
  cargo run --release -p lean-rs-host --example long_session_memory
```

Observed checkpoints from that run:

| Checkpoint | RSS KiB |
| --- | ---: |
| `start` | 5,056 |
| `after_runtime_init` | 47,648 |
| `after_host_capabilities` | 50,496 |
| `fresh_import_drop_16` | 3,726,752 |
| `fresh_import_drop_32` | 3,849,856 |
| `fresh_import_drop_48` | 2,901,984 |
| `fresh_import_drop_64` | 2,386,784 |

The process exited with status `-1` immediately after `fresh_import_drop_64` in the Codex desktop runner. The earlier
checkpoint already shows that repeated fresh imports in one process move RSS from about 50 MiB to multiple GiB even
though the pool capacity is zero and every session environment is dropped.

To let the reproducer reach every phase, the same run was repeated with `LEAN_RS_LONG_SESSION_IMPORTS=64`. That variant
completed:

| Phase | Selected checkpoints | Outcome |
| --- | --- | --- |
| Fresh import/drop, capacity 0 | `fresh_import_drop_16=4,372,352`, `fresh_import_drop_64=4,248,112`, `after_fresh_import_drop=4,121,040` KiB | 64 imports, 64 dropped environments |
| Bounded pool, capacity 4 | `after_bounded_pool_warm=3,699,472`, `bounded_pool_16=3,698,160`, `bounded_pool_64=3,696,816` KiB | 4 imports, 64 reuses, flat during reuse |
| Bulk introspection | `bulk_introspection_16=3,662,176`, `bulk_introspection_512=3,662,224` KiB | 512 bulk calls, flat |
| Elaboration | `elaboration_16=3,668,672`, `elaboration_512=3,532,464` KiB | 512 calls, 256 ok / 256 diagnostic failures, no growth |
| After drops and pause | `after_drop_sessions_pools=3,532,464`, `steady_state_after_pause=3,487,472` KiB | Dropping pools/sessions did not restore the pre-import RSS baseline |

`PoolStats` for the completed run:

```text
pool_stats=fresh_import_drop imports_performed=64 reused=0 acquired=64 released_to_pool=0 released_dropped=64
pool_stats=bounded_pool imports_performed=4 reused=64 acquired=68 released_to_pool=68 released_dropped=0
pool_stats=mixed_pool_before_drop imports_performed=1 reused=0 acquired=1 released_to_pool=1 released_dropped=0
```

Conclusion: the per-test pathology is not only a test-suite artefact. It reproduces in a single long-lived process when
that process repeatedly imports fresh environments and drops them. The same run did **not** show cumulative growth from
bulk declaration queries or elaboration once an imported environment was reused.

## Attributed Sources

Attributed to `LeanSession` / `Obj<'lean>` lifetime:

- A live `LeanSession` owns one imported environment `Obj<'lean>`.
- A `SessionPool` retains those environment objects up to capacity. The bounded-pool run proves this path: four fresh
  imports warm a four-slot pool, then 64 acquires reuse those entries with no fresh import and no RSS growth.
- Dropping a session or evicted pool entry runs `Obj::Drop`, which calls `lean_dec`; the capacity-zero run proves the
  Rust pool does not retain the environments (`released_dropped=64`).

Attributed to process / Lean runtime lifetime:

- Repeated fresh `Lean.importModules` calls grow RSS even when every imported environment is dropped immediately.
- Dropping the pool and session after the completed run did not return RSS to the pre-import baseline.
- The growth therefore cannot be explained by Rust-side `SessionPool` retention alone.

Open questions:

- RSS alone does not split the retained bytes among Lean's interned names, globally registered module state, compacted
  `.olean` regions, and mimalloc arena retention.
- The completed run used the in-tree fixture, not mathlib or a large downstream project.
- The measurement is for the current resolved toolchain only (`4.29.1`); cross-toolchain behavior is out of scope here.
- macOS RSS is noisy under memory pressure and compression, so the exact KiB values are local. The phase-level shape is
  the useful signal.

## Consumer Pattern Until Prompt 32

For long-lived consumers, reuse imported environments. Keep a small `SessionPool` keyed by the import set and run
introspection, elaboration, kernel checks, and `MetaM` calls against pooled sessions. The measured workload shows those
steady operations do not accumulate RSS once the environment is warm.

Avoid repeatedly creating fresh imported environments in one process. If a workload must sweep many distinct import
sets, put a process boundary around the sweep: restart the worker after a bounded number of fresh imports. On this
machine, 64 fresh imports of the fixture workload were already enough to push RSS into multi-GiB territory; use a lower
limit for larger module sets.

Pool draining can release Rust-owned environment references, but it should not be treated as an RSS reset. Prompt 32
should therefore narrow toward documented pool-drain/process-cycling support unless it finds a Lean-supported way to
reclaim process-global import state safely.
