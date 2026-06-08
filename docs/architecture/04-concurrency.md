# Concurrency and the Lean Task Runtime

The concurrency contract for `lean-rs`. Lean is per-thread; `LeanRuntime` is `!Send`; every handle inherits that
structurally; thread attach is a public RAII type owned by `lean_rs::runtime`. The rest of the design follows.

## Thread affinity

`LeanRuntime` contains a `PhantomData<*mut ()>`, so it is `!Send + !Sync`. Every safe handle on the public surface
either holds `&'lean LeanRuntime` directly or transitively (through `Obj<'lean>`, which carries
`PhantomData<&'lean LeanRuntime>`), and inherits the same restriction. The auto-trait inference is structural—there are
no `unsafe impl Send` or `unsafe impl Sync` anywhere in the crate.

Handles covered:

- `LeanRuntime`, `LeanThreadGuard<'lean>`
- `LeanHost<'lean>`, `LeanCapabilities<'lean, 'h>`, `LeanSession<'lean, 'c>`
- `LeanName<'lean>`, `LeanLevel<'lean>`, `LeanExpr<'lean>`, `LeanDeclaration<'lean>`, `LeanEvidence<'lean>`
- `SessionPool<'lean>`, `PooledSession<'lean, 'p, 'c>`

Each is pinned at compile time by `crates/lean-rs-host/tests/compile_fail/runtime_is_not_send_or_sync.rs`. An accidental
`impl Send` from a refactor is caught by the `.stderr` snapshot in the pinned `compile-fail` CI job
([`.github/workflows/compile-fail.yml`](../../.github/workflows/compile-fail.yml), `RUN_TRYBUILD=1`) before the change
can merge.

### Why the `'lean` parameter, on top of `OnceLock`

In practice the runtime resolves to `&'static LeanRuntime`: `OnceLock` makes it a process-once singleton, and no current
caller binds `'lean` to a non-static lifetime. The parameter forecloses one bug class `OnceLock` alone cannot catch—a
handle outliving a scoped runtime view (e.g., a future per-task `LeanRuntime`). The cost is signature noise.

## What can cross threads

Data types—types that carry information *about* a Lean result but no Lean refcount—are `Send + Sync` by auto-trait
derivation and may travel freely:

- `LeanCancellationToken` (contains only `Arc<AtomicBool>`; it carries no Lean handle and is checked cooperatively by
  the worker thread).
- `LeanError`, `LeanResult<T>` (per `LeanError`'s `Clone + Send + Sync` derivation, with `T: Send + Sync` as the usual
  constraint).
- `EvidenceStatus`, `LeanKernelOutcome<'lean>` (the `'lean` argument is a marker; the type carries no Lean refcount).
- `ProofSummary` (a bounded byte buffer).
- `LeanDiagnostic`, `LeanPosition`, `LeanSeverity`.
- `SessionStats`, `PoolStats`.

Typical workflow: a worker runs Lean work to completion inside its `LeanThreadGuard` scope, projects the result to one
of these plain Rust types, and sends the projection back to a coordinator thread over a channel. Lean handles never
leave the worker.

Cancellation is the asymmetric exception to "results travel back": the coordinator sends a `LeanCancellationToken` clone
into the worker before the operation starts, then may call `cancel()` from another thread. The session itself still
stays on the worker. See [`07-cooperative-cancellation.md`](07-cooperative-cancellation.md).

## Thread attach and detach

`LeanThreadGuard<'lean>` is the public RAII type owning one attach/detach pair. `LeanThreadGuard::attach(runtime)`
attaches the calling OS thread (forwarded to `lean_initialize_thread`); the guard's `Drop` detaches
(`lean_finalize_thread`). Three rules:

1. Every OS thread that Lean did not start, and that calls into Lean, must hold a `LeanThreadGuard` for the duration of
   its Lean work.
2. The thread that called `LeanRuntime::init` does **not** need a guard; it is implicitly attached for the rest of its
   lifetime. (Lean's own initialization runs on that thread; treating it as a "main" thread is the convention.)
3. A guard must be dropped on the same thread that attached it. The `!Send` claim on `LeanThreadGuard` makes that
   structural—the compiler refuses to let one cross a thread boundary.

Nested attaches are legal: a worker that re-enters `attach` (e.g., inside a callback) gets a second guard; the
per-thread attach depth balances on the matching `Drop`.

Every host-call funnel (`crate::module::LeanExported::call`, which all typed Lean dispatches route through) inserts a
debug-only `debug_assert_attached`. A worker that forgets its guard panics with a clear Rust message in debug; release
builds compile the assertion away.

## Task manager

The Lean task manager is required for any capability that runs `Language.Lean.processCommands` (notably
`LeanSession::kernel_check`), which asserts `g_task_manager` on entry. `LeanRuntime::init` therefore starts the task
manager as part of its process-once init sequence, after `lean_initialize_runtime_module` and `lean_initialize`.

Worker count defaults to Lean's compiled-in heuristic (typically one per core). Override by setting
`LEAN_RS_NUM_THREADS` to a positive integer **before** the first `LeanRuntime::init` call; the first call captures the
value and later changes have no effect, because the task manager is process-lived and `init` is idempotent. Invalid
values fall back to Lean's default with a `tracing::warn!` against the `lean_rs` target.

Set `LEAN_RS_NUM_THREADS` when several Lean-using processes run side by side (CI test matrices, batch workers,
multi-tenant services) so the sum of their pools does not oversubscribe cores. The workspace ships
`LEAN_RS_NUM_THREADS = "1"` as a cargo `[env]` default; tests run under `cargo nextest run` with a 1-process cap, for at
most 1 Lean worker across the suite. See [`docs/testing.md`](../testing.md) for the test-runner side.

The crate does not currently expose a programmatic `with_workers(n)` constructor; the env var is the single supported
override. Adding a programmatic API later is a strict refinement.

The task manager is process-lived. `lean-rs` does not call `lean_finalize_task_manager`; Lean tears it down at process
exit, like the runtime itself.

The Lean `Task` *value* type (Lean-level `Task α`) is intentionally not part of the public Rust surface. `lean_task_*`
exists in `lean.h` but is not exposed through `lean-rs-sys`. Rust-side concurrency uses Rust primitives; Lean tasks are
an internal implementation detail of capabilities that need them.

## `SessionPool` under concurrency

`SessionPool<'lean>` is a per-thread free-list for `LeanSession` instances bucketed by their imports key. Interior
mutability is `RefCell<PoolInner<'lean>>` + `Cell<PoolStats>`; combined with the inherited `!Send + !Sync` of
`LeanRuntime`, the pool is firmly single-threaded.

Intended deployment: one pool per worker, all anchored to the shared `&'static LeanRuntime` returned by
`LeanRuntime::init`. Workers acquire and release independently; the pool itself never crosses a thread boundary.
Cross-thread session sharing is unsupported and prevented at compile time. If a workload needs more capacity than one
pool can sustain, the answer is more workers (each with its own pool), not a shared pool.

## Embedding in async runtimes (Tokio / smol / async-std)

**Sync-first, with a dedicated bounded thread pool.** Each worker holds a `LeanThreadGuard` for its lifetime and owns
its own `LeanHost` / `LeanCapabilities` / `SessionPool`. Async code submits jobs to that pool via the runtime's
blocking-task primitive and receives back a `Send`-able Rust projection.

```rust
// One-time setup, off the async pool.
let runtime = lean_rs::LeanRuntime::init()?;
let lean_pool = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(num_lean_workers)
    .on_thread_start(move || {
        WORKER_GUARD.with(|cell| {
            cell.borrow_mut()
                .replace(lean_rs::LeanThreadGuard::attach(runtime));
        });
    })
    .on_thread_stop(|| {
        WORKER_GUARD.with(|cell| { cell.borrow_mut().take(); });
    })
    .build()?;

// In async code:
let summary: lean_rs::ProofSummary = lean_pool
    .spawn_blocking(move || run_one_lean_job(/* ... */))
    .await??;
```

Two rules carry over from the sync API:

1. **No Lean-derived value crosses an `await`.** `LeanSession`, `LeanExpr`, etc. are `!Send`; the compiler will refuse.
   The programmer must arrange the work so a complete unit happens inside one `spawn_blocking` closure.
2. **The return value must be `Send + Sync` Rust data.** A `LeanKernelOutcome` or `ProofSummary` is the natural unit;
   raw handles are not.

No async helpers ship as part of `lean-rs`. The sketch shows what to wire up, not a published API.

### Why not async-first

Making `lean-rs` itself `async fn`-shaped would push every Lean call through an executor poll, multiply the surface, and
provide no benefit over the pattern above. Lean is synchronous inside a thread; the natural place to absorb that into an
async service is at the thread boundary, not inside the binding crate. Embedders needing fine-grained async cancellation
around a Lean call wrap the blocking task themselves—the building block is `spawn_blocking`, not yet-another `Future`.
