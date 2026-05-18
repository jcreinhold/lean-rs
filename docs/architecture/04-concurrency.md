# Concurrency and the Lean task runtime

This document is the concurrency contract for `lean-rs`. It states which
types may travel between threads, what must happen on a worker thread
before it calls Lean, how the Lean task manager is initialized, and how
the sync-first API embeds inside async runtimes such as Tokio.

The thesis is short: every Lean-derived handle is per-thread; Rust enforces
that structurally; thread attach is a public RAII type owned by
`lean_rs::runtime`; and the rest of the design follows.

## Thread affinity

The Lean 4 C runtime is per-thread: each OS thread that runs Lean code
must be attached via `lean_initialize_thread` and detached via
`lean_finalize_thread`, and Lean objects allocated by one thread are not
freely portable to another. `lean-rs` mirrors that into Rust at the type
level.

The anchor type, `LeanRuntime`, contains a `PhantomData<*mut ()>`. Raw
pointers are neither `Send` nor `Sync`, and `PhantomData` inherits both;
so `LeanRuntime` is `!Send + !Sync`. Every safe handle on the public
surface either holds `&'lean LeanRuntime` directly or transitively
(through `Obj<'lean>`, which carries `PhantomData<&'lean LeanRuntime>`),
and inherits the same restriction. The auto-trait inference is
structural — there are no `unsafe impl Send` or `unsafe impl Sync`
anywhere in the crate.

The handles covered by this rule are:

- `LeanRuntime`
- `LeanThreadGuard<'lean>`
- `LeanHost<'lean>`, `LeanCapabilities<'lean, 'h>`, `LeanSession<'lean, 'c>`
- `LeanName<'lean>`, `LeanLevel<'lean>`, `LeanExpr<'lean>`,
  `LeanDeclaration<'lean>`, `LeanEvidence<'lean>`
- `SessionPool<'lean>`, `PooledSession<'lean, 'p, 'c>`

Each is pinned at compile time by
`crates/lean-rs/tests/compile_fail/runtime_is_not_send_or_sync.rs`. A
regression — for example, an accidental `impl Send` introduced by a
refactor — is caught by the `.stderr` snapshot before the change can
merge.

### `'lean` cascade: belt-and-braces atop `OnceLock`

The `'lean` lifetime parameter that cascades through every handle type
(`Obj<'lean>`, `LeanExpr<'lean>`, `LeanSession<'lean, 'c>`, …) is a
structural belt-and-braces atop the `OnceLock`-backed
`LeanRuntime::init`. In practice the runtime always resolves to
`&'static LeanRuntime` — no caller in `lean-rs`, `lean-rs-host`, the
workspace fixture, or either downstream proof binds `'lean` to a
non-static lifetime; `OnceLock` makes the runtime a process-once
singleton. The lifetime parameter still pays for itself by preventing
one bug class — a handle outliving the runtime borrow it was
constructed against — that `OnceLock` alone wouldn't catch in the rare
embedder that wraps a scoped runtime view (e.g., a future per-task
LeanRuntime). The cost is lifetime noise at signatures; the benefit is
structural rejection of an entire shape of misuse. The decision is to
keep it.

## What can cross threads

The crate's data types — types that carry information *about* a Lean
result but no Lean refcount — are deliberately `Send + Sync` by their
own auto-trait derivations and may travel between worker threads as
freely as any other Rust value. These include:

- `LeanError`, `LeanResult<T>` (per `LeanError`'s `Clone + Send + Sync`
  derivation, with `T: Send + Sync` as the usual constraint),
- `EvidenceStatus`, `LeanKernelOutcome<'lean>` (the `'lean` argument is
  just a marker — the type itself carries no Lean refcount),
- `ProofSummary` (a bounded byte buffer),
- `LeanDiagnostic`, `LeanPosition`, `LeanSeverity`,
- `SessionStats`, `PoolStats`.

A typical workflow shape: a worker thread runs Lean work to completion
inside its `LeanThreadGuard` scope, projects the result to one of these
plain Rust types (a `ProofSummary`, a `LeanKernelOutcome`, an error),
and sends the projection back to a coordinator thread over a channel.
The Lean handles never leave the worker.

## Thread attach and detach

`LeanThreadGuard<'lean>` is the public RAII type that owns one
attach/detach pair. Construction via `LeanThreadGuard::attach(runtime)`
attaches the calling OS thread; the guard's `Drop` detaches. Both calls
are forwarded to the underlying Lean entry points
(`lean_initialize_thread` / `lean_finalize_thread`) inside
`lean_rs::runtime::thread`.

Three rules:

1. Every OS thread that Lean did not start, and that calls into Lean,
   must hold a `LeanThreadGuard` for the duration of its Lean work.
2. The thread that called `LeanRuntime::init` does **not** need a guard;
   it is implicitly attached for the rest of its lifetime. (Lean's own
   initialization is performed on that thread; treating it as a "main"
   thread is the convention.)
3. A guard must be dropped on the same thread that attached it. The
   `!Send` claim on `LeanThreadGuard` makes that structural — the
   compiler refuses to let one cross a thread boundary.

Nested attaches are legal: a worker that re-enters `attach` (for example,
inside a callback) gets a second guard, and the per-thread attach depth
balances on the matching `Drop`.

To catch misuse early, every host-call funnel
(`crate::module::LeanExported::call`, which all typed Lean dispatches
route through) inserts a debug-only `debug_assert_attached`. A worker
thread that forgets its guard panics with a clear Rust message in debug
builds; in release builds the assertion compiles away and the program
behaves identically to the underlying Lean assertion.

## Task manager

The Lean task manager is required for any capability that runs
`Language.Lean.processCommands` (notably `LeanSession::kernel_check`),
which asserts `g_task_manager` on entry. `LeanRuntime::init` therefore
starts the task manager as part of its process-once initialization
sequence, after `lean_initialize_runtime_module` and `lean_initialize`.

Worker count is Lean's compiled-in default (typically one worker per
hardware core) unless the `LEAN_RS_NUM_THREADS` environment variable is
set to a positive integer **before** the first call to
`LeanRuntime::init`. The first call captures the value; later changes
to the variable have no effect, because the task manager is
process-lived and `init` is idempotent. Invalid values fall back to
Lean's default with a `tracing::warn!` against the `lean_rs` target.

Set `LEAN_RS_NUM_THREADS` when several Lean-using processes run side by
side (CI test matrices, batch workers, multi-tenant services) so the
sum of their worker pools does not oversubscribe cores. The workspace
ships `LEAN_RS_NUM_THREADS = "1"` as a cargo `[env]` default for this
reason; tests run under `cargo nextest run` with a 4-process cap, so
the product is at most 4 Lean workers across the whole suite. See
[`docs/testing.md`](../testing.md) for the test-runner side of this
contract.

The crate does not currently expose a programmatic `with_workers(n)`
constructor; the env var is the single supported override. Adding a
programmatic API later is a strict refinement (env var stays as the
operator/ops escape hatch); no breaking change to plan around.

The task manager is process-lived. `lean-rs` does not call
`lean_finalize_task_manager`; Lean tears the manager down at process
exit, in the same way it tears down the runtime itself.

The Lean `Task` *value* type (Lean-level `Task α`) is intentionally not
part of the public Rust surface. The `lean_task_*` spawn/get/bind/map
functions exist in `lean.h` but are not exposed through `lean-rs-sys`.
Rust-side concurrency is handled with Rust primitives; Lean tasks are an
internal implementation detail of capabilities that need them.

## `SessionPool` under concurrency

`SessionPool<'lean>` is a per-thread free-list for `LeanSession`
instances bucketed by their imports key. Its interior mutability is
`RefCell<PoolInner<'lean>>` + `Cell<PoolStats>`; that, combined with the
inherited `!Send + !Sync` of `LeanRuntime`, makes the pool firmly
single-threaded.

The intended deployment is one pool per worker thread, all anchored to
the shared `&'static LeanRuntime` returned by `LeanRuntime::init`.
Workers acquire and release sessions independently; the pool itself
never crosses a thread boundary. Cross-thread session sharing is
unsupported and prevented at compile time.

If a workload needs more capacity than one pool can sustain, the answer
is more workers (each with its own pool), not a shared pool.

## Embedding inside async runtimes (Tokio / smol / async-std)

`lean-rs` is intentionally sync-first. The recommended pattern for
hosting it inside an async service is a dedicated bounded thread pool
where each worker holds a `LeanThreadGuard` for its lifetime and owns
its own `LeanHost` / `LeanCapabilities` / `SessionPool`. Async code
submits jobs to that pool via the runtime's blocking-task primitive
(`tokio::task::spawn_blocking` targeting a dedicated runtime built with
`Builder::new_multi_thread().on_thread_start(...).on_thread_stop(...)`,
or an equivalent in smol / async-std), and receives back a `Send`-able
Rust projection (a `ProofSummary`, `LeanKernelOutcome`, or `LeanError`).
The sketch is:

```ignore
// One-time setup, off the async thread pool.
let runtime = lean_rs::LeanRuntime::init()?;
let lean_pool = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(num_lean_workers)
    .on_thread_start(move || {
        // Hold the guard in thread-local storage for the worker's
        // lifetime so every blocking task on this thread sees the
        // thread as attached.
        WORKER_GUARD.with(|cell| {
            cell.borrow_mut()
                .replace(lean_rs::LeanThreadGuard::attach(runtime));
        });
    })
    .on_thread_stop(|| {
        WORKER_GUARD.with(|cell| {
            cell.borrow_mut().take();
        });
    })
    .build()?;

// In async code:
let summary: lean_rs::ProofSummary = lean_pool
    .spawn_blocking(move || run_one_lean_job(/* ... */))
    .await??;
```

Two rules carry over from the sync API and must be followed:

1. **No Lean-derived value crosses an `await`.** `LeanSession`,
   `LeanExpr`, etc. are `!Send`; the compiler will refuse, but the
   programmer must arrange the work so a complete unit happens inside
   one `spawn_blocking` closure.
2. **The return value must be `Send + Sync` Rust data.** A
   `LeanKernelOutcome` or `ProofSummary` is the natural unit; raw
   handles are not.

No async helpers ship as part of this prompt. The sketch above is
intentionally minimal: it shows what to wire up, not a published API.

## Why not async-first

Making `lean-rs` itself `async fn`-shaped would push every Lean call
through an executor poll, multiply the surface, and provide no benefit
that an embedder can't get from the pattern above. Lean is synchronous
inside a thread; the natural place to absorb that into an async service
is at the thread boundary, not inside the binding crate. Embedders that
need fine-grained async cancellation around a Lean call can wrap the
blocking task themselves; the building block is `spawn_blocking`, not
yet-another `Future`.
