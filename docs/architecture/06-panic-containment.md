# Panic Containment

Lean internal panics are contained by a process boundary, not by a `LeanSession` state transition. If a kernel
assertion, generated `unreachable`, runtime overflow panic, or `panic!` compiled with `LEAN_ABORT_ON_PANIC=1` fires
during a session call, the host process may terminate. Consumers that need to survive that failure class must run Lean
work in a child worker process and treat worker exit as the recovery signal.

No `LeanError::SessionPoisoned`, `LeanDiagnosticCode::SessionPoisoned`, `LeanRuntime::recycle`, or in-process
`LeanSandbox` API exists, and the sections below give the soundness argument for why.

## Caller Contract

The public `LeanSession` methods keep the existing typed contracts for ordinary Lean failures:

| Failure class | Caller observes |
| --- | --- |
| Lean `IO.throw` from an export returning `IO α` | `Err(LeanError::LeanException(_))` |
| Parse, elaboration, kernel-check, or bounded `MetaM` rejection reported by the shim | A typed value such as `LeanElabFailure`, `LeanKernelOutcome`, or `LeanMetaResponse` |
| ABI mismatch, missing symbol, malformed return value, or host invariant failure | `Err(LeanError::Host(_))` |
| Lean internal panic, `panic!` with `LEAN_ABORT_ON_PANIC=1`, generated unreachable, C++ foreign unwind, `std::exit`, or `abort` | The process may terminate; no Rust error is guaranteed |

The same rule applies to `LeanSession::call_capability`: if the called Lean export returns through its normal C ABI,
`lean-rs` decodes it. If the export terminates the process or unwinds as a foreign exception through the non-unwinding C
ABI, `LeanSession` does not recover.

## Why The Boundary Is The Process

`LeanSession` dispatches through `unsafe extern "C"` function pointers resolved from Lake-built shared libraries. Rust's
`std::panic::catch_unwind` catches unwinding Rust panics in the same Rust runtime; it does not catch aborting panics,
and it does not provide a sound recovery contract for foreign exceptions crossing a non-unwinding C ABI. The Rust
Reference also classifies unwinding into Rust through the wrong FFI ABI as undefined behavior.

Lean's runtime panic paths are not a single Rust-style unwind mechanism. Across the supported window two paths in the
upstream sources cover the cases:

- `runtime/object.cpp`: `lean_internal_panic` prints and exits; `lean_panic_impl` aborts when `LEAN_ABORT_ON_PANIC` is
  set. As of Lean 4.30 (upstream [PR #12539](https://github.com/leanprover/lean4/pull/12539)), `lean_panic_impl` also
  calls `print_backtrace`, which delegates demangling to the Lean-side `@[export]` `lean_demangle_bt_line_cstr` from
  `Lean.Compiler.NameDemangling`. See **Decoupling from Lean's panic-time runtime callbacks** below.
- `runtime/debug.h`: `lean_unreachable()` throws Lean's C++ `unreachable_reached` exception.

Those mechanisms bypass the error channel that `LeanIo<T>` decodes. A session-poisoning API would catch only a subset of
the failures it claimed to contain, masking the rest behind a falsely-typed recovery.

## Soundness Argument

After an internal Lean panic, `lean-rs` cannot prove enough to continue in the same process.

**Reference counts.** `Obj<'lean>` assumes each Rust-owned handle has one valid Lean reference and that `Drop` may later
call `lean_dec`. If a panic or foreign unwind interrupts Lean while it is transferring or consuming owned C-ABI
arguments, Rust cannot know which Lean references were incremented, decremented, or installed into another object.

**Lean global state.** Module initializer flags, interned names, imported environment tables, options, task-manager
state, and runtime allocator state are process-global or runtime-global. A panic in the middle of a mutation leaves no
public Lean recovery primitive that proves those globals are consistent.

**The `'lean` lifetime.** `LeanRuntime::init()` creates the process-once anchor for every `Obj<'lean>` and semantic
handle. There is no safe operation that invalidates all existing `'lean` values, tears down Lean, and creates a fresh
incompatible `'lean` inside the same process.

**The sealed `meta::*` registry.** The Rust registry only proves that a request is routed to one of the pre-registered
MetaM exports. It does not prove that the Lean elaborator state behind that export is valid after an internal panic.

**`SessionPool` entries.** A pool entry is an owned `Lean.Environment` handle. `SessionPool::drain()` can release cached
environment references under normal execution, but it cannot validate that an environment returned from a partially
panicked Lean call is semantically or refcount-wise intact.

The safe contract is therefore negative and explicit: process termination is the containment boundary for Lean internal
panics. Use a child process when the application must continue.

## Rejected Alternatives

**Session poisoning with `catch_unwind`.** Rejected. It would require wrapping every public `LeanSession` method,
marking the session poisoned on a caught panic, and returning `LeanError::SessionPoisoned` thereafter. That does not
cover `abort`, `std::exit`, `LEAN_ABORT_ON_PANIC`, or the C++ exception path from `lean_unreachable()`, and it cannot
prove refcount or Lean-global integrity after a foreign unwind crosses the C ABI.

**`LeanSandbox` child-process API.** Valid architecture, not shipped. A child process is the right containment
mechanism, but the IPC protocol needed to make it a `LeanSession`-shaped contract is a separate host product.
Downstreams with service-level containment needs should run their own worker process around `lean-rs` today.

**In-process runtime recycling.** Rejected. Lean runtime state is process-bound, and `lean-rs` cannot prove all old
`'lean` values are gone before recreating the runtime. See [Long-Session Memory](../safety/long-session-memory.md) for
the lifetime argument.

## Verification Fixture

`crates/lean-rs-host/tests/panic_containment.rs` re-runs its own test binary as a child process with
`LEAN_ABORT_ON_PANIC=1` and `LEAN_BACKTRACE=0`, then calls the fixture export `lean_rs_fixture_panic_unit`. The parent
asserts that the child exits unsuccessfully. This keeps the normal test runner alive while pinning the documented
process-level behavior.

The sanitizer workflow also runs this fixture under Linux AddressSanitizer.

## Decoupling from Lean's panic-time runtime callbacks

Lean 4.30 ([PR #12539](https://github.com/leanprover/lean4/pull/12539)) rewrote the C runtime's panic-time backtrace
handler to call into a Lean-implemented demangler (`@[export] lean_demangle_bt_line_cstr` from
`Lean.Compiler.NameDemangling`). The PR's stated invariant is that this is safe because `print_backtrace` is only called
from `lean_panic_impl` (soft panics), where the Lean runtime is expected to be in a normal execution state.

That invariant holds for the Lean compiler and lake projects, which always load the full compiler stdlib. **It does not
hold for embedders.** A `lean-rs-worker` child process intentionally embeds a minimal Lean: it loads `libleanshared.so`
plus a small capability dylib chain, and cannot guarantee that the modules a future Lean panic handler decides to call
back into are initialized when user code panics. The observed symptom on Linux is that `lean_panic_impl` calls
`print_backtrace` → `lean_demangle_bt_line_cstr` and hangs before reaching `abort_on_panic()`; the parent's request
times out instead of observing a fatal exit.

`lean-rs-worker` and the host-stack verification fixture therefore pin a structural boundary: **no Lean code may run
from the C panic handler in a worker child.** The boundary is enforced with `LEAN_BACKTRACE=0`, which `lean_panic_impl`
checks *before* calling `print_backtrace`:

```cpp
if (g_panic_messages) {
    panic_eprintln(msg, size, force_stderr);                    // always
    char * bt_env = getenv("LEAN_BACKTRACE");
    if (!bt_env || strcmp(bt_env, "0") != 0) {
        panic_eprintln("backtrace:", force_stderr);
        print_backtrace(force_stderr);                          // <- entire C->Lean re-entry block
    }
}
abort_on_panic();
```

With `LEAN_BACKTRACE=0` set, the panic message still prints to the child's stderr and the abort still fires; only the
backtrace generation (and any C→Lean callback inside it) is skipped.

`LEAN_BACKTRACE=0` is chosen, not `LEAN_BACKTRACE_RAW=1`, for two reasons:

- **Wider availability.** `LEAN_BACKTRACE` is present in 4.26+; `LEAN_BACKTRACE_RAW` was introduced in 4.29.1 with the
  PR that wired in the Lean demangler. The supported toolchain window spans the older variable.
- **Narrower dependency on upstream internals.** `LEAN_BACKTRACE_RAW=1` runs `print_backtrace` and only skips the
  demangler call. If a future upstream change adds another C→Lean callback elsewhere inside `print_backtrace`,
  `LEAN_BACKTRACE_RAW` would not protect against it. `LEAN_BACKTRACE=0` skips the entire block, so the boundary survives
  upstream reshuffles to what `print_backtrace` does internally.

The `LeanWorkerConfig` docstring states the policy at the public surface. The supervisor's `Command::env` defaults apply
before any explicit `LeanWorkerConfig::env(...)` entries, so a caller who has independently arranged for the demangler
module to be initialized can opt back into a demangled backtrace with `.env("LEAN_BACKTRACE", "1")`.

In-process embedders that use `LeanHost` directly (not via worker) are not affected by this default — they own their own
process environment, and the host shim's `import Lean` transitively initializes `Lean.Compiler.NameDemangling`, so the
panic-time demangler callback resolves cleanly. The worker child is the case that needs the explicit boundary.

## Decoupling from the kernel's core-dump pipe handler

`LEAN_ABORT_ON_PANIC=1` turns a Lean internal panic into `abort()` → `SIGABRT`. The parent supervisor recognises that
fatal exit by reading EOF on the child's stdout and translating it to `LeanWorkerError::ChildPanicOrAbort { exit }`.
That round trip is fast — *unless* the kernel suspends the dying child to feed its core image to a pipe-based
`core_pattern` handler.

On GitHub Actions `ubuntu-latest`, the runner inherits Ubuntu's default `core_pattern`, which pipes the core image to
`apport` (or `systemd-coredump` on newer images). For a worker child that has loaded `libleanshared.so` plus a
capability dylib chain, the kernel holds the dying process's file descriptors open while it streams the image to the
handler. Measured delays on the runner are 30–110 seconds; the supervisor's 30-second per-request timeout fires first
and the parent reports `Timeout { operation, duration }` instead of the typed fatal exit.

The contained workloads have no use for a core file: typed errors (`ChildPanicOrAbort`, `Worker { code, message }`) and
the captured child stderr already cover the supported diagnostic surface. The fix is to suppress core dumps in every
worker child: `child::disable_core_dumps` calls `setrlimit(RLIMIT_CORE, {0, 0})` at the top of the child entry point so
any subsequent `SIGABRT` terminates the process immediately, closing the IPC pipes and letting the parent observe EOF on
normal IPC timescales. The same call is a Windows no-op (Windows does not use POSIX rlimits or `core_pattern`).

This boundary lives in the child binary rather than in `LeanWorker::spawn` because the policy belongs to "any process
shipped as a `lean-rs-worker` child," including downstream binaries written using `run_worker_child_stdio`. Spawning the
child from a different supervisor (the private `__test_support::WorkerProcess`, a downstream service) still inherits the
boundary because it is baked into `run_stdio`. No public API change is required.

Regression cover: `crates/lean-rs-worker/tests/protocol.rs::fatal_exit_after_partial_rows_is_reported_as_worker_failure`
asserts that panic-to-fatal-exit detection completes within 10 seconds. Without the rlimit fix, the same test takes
30–110 seconds on Linux runners with `apport`.

## References

- Rust `std::panic::catch_unwind`: <https://doc.rust-lang.org/std/panic/fn.catch_unwind.html>
- Rust Reference, panic and FFI unwinding: <https://doc.rust-lang.org/stable/reference/panic.html>
- Lean Reference, FFI ABI and initialization:
  <https://lean-lang.org/doc/reference/latest/Run-Time-Code/Foreign-Function-Interface/>
- Lean Reference, reference counting: <https://lean-lang.org/doc/reference/latest/Run-Time-Code/Reference-Counting/>
