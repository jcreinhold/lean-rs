# Panic Containment

Lean internal panics are contained by a process boundary, not by a
`LeanSession` state transition. If a kernel assertion, generated
`unreachable`, runtime overflow panic, or `panic!` compiled with
`LEAN_ABORT_ON_PANIC=1` fires during a session call, the host process may
terminate. Consumers that need to survive that failure class must run
Lean work in a child worker process and treat worker exit as the recovery
signal.

This document records the prompt-33 decision. No `LeanError::SessionPoisoned`,
`LeanDiagnosticCode::SessionPoisoned`, `LeanRuntime::recycle`, or in-process
`LeanSandbox` API exists.

## Caller Contract

The public `LeanSession` methods keep the existing typed contracts for
ordinary Lean failures:

| Failure class | Caller observes |
| --- | --- |
| Lean `IO.throw` from an export returning `IO α` | `Err(LeanError::LeanException(_))` |
| Parse, elaboration, kernel-check, or bounded `MetaM` rejection reported by the shim | A typed value such as `LeanElabFailure`, `LeanKernelOutcome`, or `LeanMetaResponse` |
| ABI mismatch, missing symbol, malformed return value, or host invariant failure | `Err(LeanError::Host(_))` |
| Lean internal panic, `panic!` with `LEAN_ABORT_ON_PANIC=1`, generated unreachable, C++ foreign unwind, `std::exit`, or `abort` | The process may terminate; no Rust error is guaranteed |

The same rule applies to `LeanSession::call_capability`: if the called
Lean export returns through its normal C ABI, `lean-rs` decodes it. If the
export terminates the process or unwinds as a foreign exception through the
non-unwinding C ABI, `LeanSession` does not recover.

## Why The Boundary Is The Process

`LeanSession` dispatches through `unsafe extern "C"` function pointers
resolved from Lake-built shared libraries. Rust's `std::panic::catch_unwind`
catches unwinding Rust panics in the same Rust runtime; it does not catch
aborting panics, and it does not provide a sound recovery contract for
foreign exceptions crossing a non-unwinding C ABI. The Rust Reference also
classifies unwinding into Rust through the wrong FFI ABI as undefined
behavior.

Lean's runtime panic paths are not a single Rust-style unwind mechanism.
At Lean 4.29.1, local source inspection shows:

- `/Users/jcreinhold/Code/lean4/src/runtime/object.cpp`: `lean_internal_panic`
  prints the panic and exits, and `lean_panic_impl` aborts when
  `LEAN_ABORT_ON_PANIC` is set.
- `/Users/jcreinhold/Code/lean4/src/runtime/debug.h`: `lean_unreachable()`
  throws Lean's C++ `unreachable_reached` exception.

Those mechanisms bypass the error channel that `LeanIo<T>` decodes. A
session-poisoning API would therefore catch only a subset of the failures
it claimed to contain, while giving callers the impression that the
runtime and environment were still trustworthy.

## Soundness Argument

After an internal Lean panic, `lean-rs` cannot prove enough to continue in
the same process.

**Reference counts.** `Obj<'lean>` assumes each Rust-owned handle has one
valid Lean reference and that `Drop` may later call `lean_dec`. If a panic
or foreign unwind interrupts Lean while it is transferring or consuming
owned C-ABI arguments, Rust cannot know which Lean references were
incremented, decremented, or installed into another object.

**Lean global state.** Module initializer flags, interned names, imported
environment tables, options, task-manager state, and runtime allocator
state are process-global or runtime-global. A panic in the middle of a
mutation leaves no public Lean recovery primitive that proves those globals
are consistent.

**The `'lean` lifetime.** `LeanRuntime::init()` creates the process-once
anchor for every `Obj<'lean>` and semantic handle. There is no safe
operation that invalidates all existing `'lean` values, tears down Lean,
and creates a fresh incompatible `'lean` inside the same process.

**The sealed `meta::*` registry.** The Rust registry only proves that a
request is routed to one of the pre-registered MetaM exports. It does not
prove that the Lean elaborator state behind that export is valid after an
internal panic.

**`SessionPool` entries.** A pool entry is an owned `Lean.Environment`
handle. `SessionPool::drain()` can release cached environment references
under normal execution, but it cannot validate that an environment returned
from a partially panicked Lean call is semantically or refcount-wise intact.

The safe contract is therefore negative and explicit: process termination
is the containment boundary for Lean internal panics. Use a child process
when the application must continue.

## Rejected Alternatives

**Session poisoning with `catch_unwind`.** Rejected. It would require
wrapping every public `LeanSession` method, marking the session poisoned on
a caught panic, and returning `LeanError::SessionPoisoned` thereafter. That
does not cover `abort`, `std::exit`, `LEAN_ABORT_ON_PANIC`, or the C++
exception path from `lean_unreachable()`, and it cannot prove refcount or
Lean-global integrity after a foreign unwind crosses the C ABI.

**`LeanSandbox` child-process API.** Valid architecture, but not shipped in
this prompt. A child process is the right containment mechanism, but adding
an IPC protocol would be a new host product surface rather than a
`LeanSession` contract. Downstreams with service-level containment needs
should run their own worker process around `lean-rs` today.

**In-process runtime recycling.** Rejected by prompt 32 and still rejected
here. Lean runtime state is process-bound, and `lean-rs` has no sound way
to prove all old `'lean` values are gone before recreating the runtime.

## Verification Fixture

`crates/lean-rs-host/tests/panic_containment.rs` re-runs its own test
binary as a child process with `LEAN_ABORT_ON_PANIC=1`, then calls the
fixture export `lean_rs_fixture_panic_unit`. The parent asserts that the
child exits unsuccessfully. This keeps the normal test runner alive while
pinning the documented process-level behavior.

The sanitizer workflow also runs this fixture under Linux AddressSanitizer.

## References

- Rust `std::panic::catch_unwind`: <https://doc.rust-lang.org/std/panic/fn.catch_unwind.html>
- Rust Reference, panic and FFI unwinding: <https://doc.rust-lang.org/stable/reference/panic.html>
- Lean Reference, FFI ABI and initialization: <https://lean-lang.org/doc/reference/latest/Run-Time-Code/Foreign-Function-Interface/>
- Lean Reference, reference counting: <https://lean-lang.org/doc/reference/latest/Run-Time-Code/Reference-Counting/>
