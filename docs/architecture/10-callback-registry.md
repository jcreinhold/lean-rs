# Callback Registry

`lean-rs` exposes a narrow Rust callback registry for Lean-to-Rust calls. This
is an L1 primitive, not a `LeanSession` feature.

## Public Shape

Rust code registers a closure for one sealed payload type:

```rust
let callback = lean_rs::LeanCallbackHandle::<lean_rs::LeanProgressTick>::register(|tick| {
    eprintln!("{}/{}", tick.current, tick.total);
    lean_rs::LeanCallbackFlow::Continue
})?;
let (handle, trampoline) = callback.abi_parts();
```

A Lean export that follows the callback ABI accepts the two values as `USize`:

```text
opaque_handle : USize
trampoline    : USize
```

Lean treats both values as opaque and passes them to `LeanRsInterop.Callback`,
which invokes the Rust trampoline through the generic C helper. Callers cannot
supply their own trampoline function pointer through the public API.

## Payloads

`LeanCallbackHandle<P>` is generic over `P: LeanCallbackPayload`, but
`LeanCallbackPayload` is sealed. Downstream crates can use supported payloads;
they cannot add new ABI shapes or decode raw Lean objects themselves.

The supported payloads are:

```rust
pub struct LeanProgressTick {
    pub current: u64,
    pub total: u64,
}

pub struct LeanStringEvent {
    pub value: String,
}
```

`LeanProgressTick` is the old two-counter payload under a precise name. The
deprecated `LeanCallbackEvent` alias remains temporarily for compatibility, but
new code should use `LeanProgressTick`.

`LeanStringEvent` copies a borrowed Lean `String` into an owned Rust `String`
before user code runs. No Lean object lifetime escapes the trampoline.

## Flow And Lifetime

Callbacks return `LeanCallbackFlow`. `Continue` lets the Lean-side callback loop
continue; `Stop` asks Lean to stop cleanly and return
`LeanCallbackStatus::Stopped`.

`LeanCallbackHandle<P>` is an RAII registration. Dropping it unregisters the id.
Lean may call the handle only while the Rust value is alive. If a stale id is
called after drop, the trampoline returns `LeanCallbackStatus::StaleHandle`
instead of dereferencing freed Rust memory.

The handle is `Send + Sync`. The registered closure must be
`Fn(P) -> LeanCallbackFlow + Send + Sync + 'static` because Lean may invoke it
on the Lean-bound worker thread, and registry lookup clones an internal `Arc`
before running the callback.

## Panic And Reentrancy

Rust panics must not cross Lean or C frames. The registry trampoline catches
unwinding Rust panics, records a `LeanError` with
`HostStage::CallbackPanic`, and returns `LeanCallbackStatus::Panic` to Lean.
Aborting Rust panics and Lean internal panics remain process-scoped.

Callbacks run synchronously on the Lean-bound thread. A callback invoked during
a Lean call must not call back into the same `LeanSession` or re-enter the same
Lean call stack. The registry is a one-way callback mechanism; it does not make
the host session reentrant.

## Diagnostics

The trampoline returns `LeanCallbackStatus` as a `UInt8` status:

| Status | Byte | Meaning |
| --- | ---: | --- |
| `Ok` | `0` | The callback ran successfully and asked Lean to continue |
| `StaleHandle` | `1` | Lean called a dropped callback id |
| `Panic` | `2` | The callback panicked and Rust contained the unwind |
| `WrongPayload` | `3` | Lean used a handle with the wrong payload trampoline |
| `Stopped` | `4` | The callback asked Lean to stop cleanly |

For `Panic`, callers can inspect `LeanCallbackHandle::last_error()` while the
handle is still alive. The recorded error has diagnostic code
`lean_rs.internal` and stage `CallbackPanic`.

## Files

- Registry: `crates/lean-rs/src/callback.rs`
- Generic Lean helper: `crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback.lean`
- Tests: `crates/lean-rs/tests/callback_registry.rs`
