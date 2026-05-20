# Callback ABI Spike

Prompt 40 proved the minimum Lean-to-Rust callback ABI. The public registry
that uses this ABI is documented in
[`10-callback-registry.md`](10-callback-registry.md).

## ABI Shape

The test-only host shim export is:

```text
lean_rs_interop_test_callback_loop : USize -> USize -> UInt64 -> IO UInt8
```

The first `USize` is an opaque Rust-owned handle. The second is a Rust
function pointer. Lean does not call the function pointer directly. It calls
`lean_rs_interop_tick_callback_call`, a tiny C helper linked into the shim dylib.
The helper casts the second `USize` to:

```c
uint8_t (*)(uintptr_t handle, uint64_t current, uint64_t total)
```

and invokes it with the opaque handle and integer payload.

This proves the reusable mechanism without requiring a process-global exported
Rust symbol. The Rust test owns the handle and the trampoline function pointer;
the shim only knows the stable C ABI shape.

The C helper source and Lean callback call primitive live in the generic
`lean-rs-interop-shims` package bundled under `crates/lean-rs/shims/`. The host
shim export remains as a compatibility test symbol and loops over the same C
helper in the host crate's bundled shim copy.

## Thread And Reentrancy Contract

The callback runs synchronously on the Lean-bound thread. It must not re-enter a
`LeanSession` or call another Lean export through the same runtime stack. Later
public callback handles must preserve that rule at the API boundary.

The handle is opaque to Lean. In the spike it is a pointer to a test-local Rust
probe whose lifetime covers the whole Lean call. The public registry replaces
that test-local pointer with an RAII registration handle.

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

- Generic Lean helper: `crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback.lean`
- C helper: `crates/lean-rs/shims/lean-rs-interop-shims/c/interop_callback.c`
- Host test export: `crates/lean-rs-host/shims/lean-rs-host-shims/LeanRsHostShims/Interop.lean`
- Rust fixture: `crates/lean-rs/tests/callback_trampoline.rs`
- Sanitizer job: `.github/workflows/sanitizer.yml`

## Status

The spike proves event order and panic-boundary behavior. It deliberately leaves
these items to the public registry and later interop prompts:

- object payload conversion beyond `(current, total)` integers;
- host progress events.
