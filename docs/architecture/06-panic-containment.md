# Panic Containment

A child process is the recovery boundary for fatal Lean failures. A session call may terminate its process after an
internal panic, generated unreachable path, runtime assertion, foreign unwind, `std::exit`, or `abort`. Applications
that must survive those failures use the worker crates and recover through the supervisor.

## Failure channels

| Failure | Caller observes |
| --- | --- |
| Lean `IO.throw` returned through the normal ABI | `LeanError::LeanException` |
| Parse, elaboration, kernel, or bounded `MetaM` rejection | A typed result such as `LeanElabFailure`, `LeanKernelOutcome`, or `LeanMetaResponse` |
| ABI, loading, conversion, or host invariant failure | `LeanError::Host` |
| Unwinding Rust callback panic | Callback `Panic` status and bounded `LeanError` |
| Fatal Lean failure, aborting Rust panic, or foreign unwind | No in-process Rust error is guaranteed |

`catch_unwind` catches Rust unwinds, not process exit or abort, and supplies no sound recovery contract for a C++
exception crossing a non-unwinding C ABI. Lean has several panic mechanisms: runtime exit, abort-on-panic, and
`lean_unreachable`'s C++ exception path. They do not share the `IO` error channel decoded by `LeanIo<T>`.

After such a failure, Rust cannot establish which owned ABI arguments Lean consumed, whether reference counts remain
valid, or whether global initializer, import, task-manager, and allocator state is consistent. The process-lived `'lean`
anchor also cannot invalidate every existing handle and then start a new runtime. Neither session poisoning nor
`SessionPool::drain()` proves recovery. Draining only releases normal cached references; see
[long-session memory](../safety/long-session-memory.md).

## Worker panic defaults

[`LeanWorkerConfig`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-worker-parent/src/supervisor.rs) supplies two child environment defaults:

- `LEAN_ABORT_ON_PANIC=1` makes Lean internal panics terminate rather than return inhabited default values that could be
  mistaken for real query results.
- `LEAN_BACKTRACE=0` skips panic-time backtrace generation. That path may call Lean's `Lean.Compiler.NameDemangling`
  export; a minimal embedding cannot assume its transitive initialization is available at the moment of failure.

Explicit config `.env(...)` entries override these defaults. A caller opting into Lean backtraces owns that dependency.
Skipping the whole backtrace block avoids depending on exactly which C-to-Lean callbacks a future backtrace renderer
uses. The panic message still goes to captured child stderr. In-process applications own their own environment policy.

## Prompt fatal-exit detection

On Unix, [`run_worker_child_stdio`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-worker-child/src/child.rs) installs a `SIGABRT` handler that
writes a short marker and calls `_exit(134)`. It also sets `RLIMIT_CORE=0` and, on Linux, `PR_SET_DUMPABLE=0` as
fallback measures. This policy travels with downstream child binaries using the same entry point, independently of their
supervisor.

The reason is operationally specific. Pipe-based Linux `core_pattern` handlers can keep a dying child's IPC descriptors
open while draining its core image. Recorded runner delays were roughly 30–110 seconds, enough to race the supervisor's
30-second timeout. The child source records about 107 seconds without either fallback and about 23 seconds with
`setrlimit` alone; bypassing the signal-default path closes pipes immediately. Typed fatal-exit errors and captured
stderr are the supported diagnostics; worker children do not promise core files.

[`protocol.rs::fatal_exit_after_partial_rows_is_reported_as_worker_failure`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-worker-child/tests/protocol.rs)
runs a Lean panic after partial rows and requires fatal-exit detection within 10 seconds. The fixture export is
[`lean_rs_fixture_panic_unit`](https://github.com/jcreinhold/lean-rs/blob/main/fixtures/lean/LeanRsFixture/Effects.lean). These are ordinary worker containment
tests; the [sanitizer workflow](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/sanitizer.yml) does not ASan-instrument in-process Lean tests.

## Read-only queries and degraded results

Read-only resolution still elaborates and renders Lean data. Rendering a captured proof state can reach a pure
`MetavarContext.getDecl` panic when a referenced metavariable declaration is missing. Catching Lean exceptions around a
renderer does not catch that panic. The safeguards have distinct responsibilities:

1. The shim screens structurally reachable metavariables with total `findDecl?` checks before rendering and reports a
   degraded result. It cannot prove that delayed assignments hide no further dangling references.
2. Optional child RSS taint reclassifies non-positive verification outcomes near a configured memory ceiling.
3. For supported read-only verify and batched-query operations, the supervisor catches a fatal child exit, restarts the
   child, and returns an unavailable/`BudgetExceeded` result. Other errors and a failed restart still propagate.

The exact policies live in [module-query projection](09-info-tree-projection.md). They prevent or classify failures at
the layer that has the needed information; they do not make an interrupted Lean process reusable.

An ambiguous name alone is not evidence of this panic. The
[`verify_declaration_ambiguous_open_reference_does_not_restart_child`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-worker-child/tests/typed_session.rs)
regression opens two namespaces defining the same short name, renders the ambiguity diagnostic, returns both candidates,
and asserts zero restarts. That result warrants keeping ordinary ambiguity on the normal typed path rather than adding
an unsubstantiated panic guard.

## References

- [Rust `catch_unwind`](https://doc.rust-lang.org/std/panic/fn.catch_unwind.html)
- [Rust panic and FFI unwinding](https://doc.rust-lang.org/stable/reference/panic.html)
- [Lean FFI and initialization](https://lean-lang.org/doc/reference/latest/Run-Time-Code/Foreign-Function-Interface/)
- [Lean reference counting](https://lean-lang.org/doc/reference/latest/Run-Time-Code/Reference-Counting/)
