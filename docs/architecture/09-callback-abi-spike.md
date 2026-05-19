# Callback ABI Spike

Prompt 40 proves the minimum Lean-to-Rust callback ABI. It does not add a
public callback registry.

## ABI Shape

The test-only shim export is:

```text
lean_rs_interop_test_callback_loop : USize -> USize -> UInt64 -> IO UInt8
```

The first `USize` is an opaque Rust-owned handle. The second is a Rust
function pointer. Lean does not call the function pointer directly. It calls
`lean_rs_interop_callback_call`, a tiny C helper linked into the shim dylib.
The helper casts the second `USize` to:

```c
uint8_t (*)(uintptr_t handle, uint64_t current, uint64_t total)
```

and invokes it with the opaque handle and integer payload.

This proves the reusable mechanism without requiring a process-global exported
Rust symbol. The Rust test owns the handle and the trampoline function pointer;
the shim only knows the stable C ABI shape.

## Thread And Reentrancy Contract

The callback runs synchronously on the Lean-bound thread. It must not re-enter a
`LeanSession` or call another Lean export through the same runtime stack. Later
public callback handles must preserve that rule at the API boundary.

The handle is opaque to Lean. In the spike it is a pointer to a test-local Rust
probe whose lifetime covers the whole Lean call. Prompt 41 will replace that
test-local pointer with an RAII registry handle.

## Panic Boundary

Rust panics must not unwind into Lean or across a non-unwinding C ABI. The
test trampoline catches Rust panics before returning to the C helper and reports
failure as a `UInt8` status. Lean internal panics remain process-scoped; see
[`06-panic-containment.md`](06-panic-containment.md).

The relevant rules are the Rust Reference's FFI unwinding table and
`std::panic::catch_unwind`: `catch_unwind` catches Rust unwinding panics, not
aborts, and non-unwinding C ABI boundaries are not a recovery surface for
foreign unwinds. Lean's FFI model still uses explicit `@[extern]` and
`@[export]` ABI boundaries.

## Files

- Lean shim: `lake/lean-rs-host-shims/LeanRsHostShims/Interop.lean`
- C helper: `lake/lean-rs-host-shims/c/interop_callback.c`
- Rust fixture: `crates/lean-rs/tests/callback_trampoline.rs`
- Sanitizer job: `.github/workflows/sanitizer.yml`

## Status

The spike proves event order and panic-boundary behavior. It deliberately leaves
these items out of scope:

- public callback registration;
- callback handle ownership and drop semantics;
- object payload conversion beyond `(current, total)` integers;
- host progress events.

Those belong to the callback registry and progress prompts.
