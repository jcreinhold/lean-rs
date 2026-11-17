# Concurrency and the Lean Task Runtime

Lean-derived values stay on their originating OS thread. Rust scheduling happens around complete synchronous Lean
operations; Lean's own task manager remains an implementation detail of those operations.

## Thread affinity

`LeanRuntime` contains `PhantomData<*mut ()>` and is `!Send + !Sync`. Object handles carry a borrow of that runtime,
directly or through `Obj<'lean>`, and inherit the restriction. It covers semantic handles, `LeanHost`,
`LeanCapabilities`, `LeanSession`, `LeanEvidence`, `SessionPool`, `PooledSession`, and `LeanThreadGuard`. The
[compile-fail suite](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/tests/compile_fail/runtime_is_not_send_or_sync.rs) pins the handle chain;
no unsafe `Send` or `Sync` implementation relaxes it.

`LeanRuntime::init()` returns a process-lived borrow. The `'lean` parameter still records the dependency in every handle
and prevents a value from escaping a shorter runtime view. A `'static` lifetime does not make a handle thread-safe.

Plain Rust projections can cross threads: `EvidenceStatus`, `ProofSummary`, diagnostics, errors, statistics, and
`LeanCancellationToken`. A container such as `LeanResult<T>` is transferable only when its payload is. In particular,
`LeanKernelOutcome<'lean>` contains `LeanEvidence<'lean>` and stays on the Lean thread. Send its `status()` or a
`ProofSummary` instead.

## Initialization and explicit attachment

Call `LeanRuntime::init()` on each thread that constructs Lean handles. Initialization runs once per process; every
successful call also marks the calling thread as permitted in the Rust thread-local dispatch check. Do not capture a
runtime reference from a coordinator in a worker closure: `&LeanRuntime` is itself `!Send`.

`LeanThreadGuard::attach(runtime)` explicitly calls `lean_initialize_thread`; dropping the guard calls
`lean_finalize_thread`. Every additional Rust-created thread that uses Lean must hold this guard around all Lean work
and the destruction of its Lean handles, then drop it on that same thread. Nested guards are legal and balance attach
depth. The thread that performed the first process initialization and threads started by Lean already have Lean's
runtime context and need no additional guard.

The distinction matters: later `init()` calls update the Rust thread-local permission marker; they do not themselves
call the C attach function. A successful later `init()` therefore does not replace the required guard on a Rust-created
worker thread. Typed export dispatch has a debug-only attachment assertion; it is a diagnostic aid rather than a
release-mode safety boundary. The implementation is in
[runtime initialization](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/src/runtime/init.rs) and
[thread attachment](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/src/runtime/thread.rs).

## Lean task manager

`LeanRuntime::init` starts the task manager after runtime initialization and the end-of-bootstrap marker. Services that
run `Language.Lean.processCommands`, including kernel checks and module processing, need it.

Set `LEAN_RS_NUM_THREADS` to a positive integer before the first `init()` to override Lean's compiled-in worker
heuristic. The first call captures the value; invalid values warn on the `lean_rs` tracing target and use Lean's
default. Later environment changes do not reconfigure the manager. Account for this count when several worker processes
run concurrently. Repository test runs default to one Lean task worker; see [testing](../testing.md).

The task manager and runtime live until process exit. `lean-rs` does not finalize or recycle them in-process. Lean
`Task α` values are not a public Rust API; Rust-side scheduling uses Rust primitives.

## Per-thread session pools

`SessionPool` uses single-threaded interior mutability and retains environment handles on its owning thread. Reuse keys
include the canonical Lake root, ordered imports, and import profile; the pool also rejects environments whose
extension-registration stamp is stale. Import order is preserved because it affects Lean imports.

Use one host/capability/session pool per Rust worker. Separate workers may execute against their own environments, but
host import and source-processing paths that touch global Lean import/cache state share a process-local mutex. This is
not a claim that all Lean operations serialize, nor does it serialize separate worker processes.

## Async embedding

Submit a complete Lean job to a bounded blocking executor or dedicated worker thread:

1. Inside each additional Rust-created blocking or dedicated thread, call `LeanRuntime::init()`, attach a
   `LeanThreadGuard`, and then construct the host, capabilities, and sessions there.
2. Complete all Lean calls and convert their results to owned Rust data on that same thread.
3. Drop all Lean handles before the guard, then return the projection through a channel or blocking-task join handle.
   Keep handles within the synchronous scope. The first process-initialization thread has the attachment exception
   described above.

A Tokio `spawn_blocking` closure is one possible adapter. Do not use async-runtime worker start/stop hooks to attach
blocking workers: those hooks do not establish the lifetime of each blocking task. A dedicated thread can keep its own
host and session pool across many jobs; an arbitrary blocking-task scheduler must not migrate those values between
threads.

`!Send` values cannot be retained across an `await` in a future that must be `Send`. Even on a local executor, keep the
Lean operation synchronous and on its attached thread. Async cancellation does not pre-empt a running Lean call; see
[cooperative cancellation](../same-process-controls.md#cancellation). Use the worker crates for process timeouts and fatal-exit
containment.
