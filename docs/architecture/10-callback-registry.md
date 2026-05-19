# Callback Registry

`lean-rs` exposes a narrow Rust callback registry for Lean-to-Rust calls. This
is an L1 primitive, not a `LeanSession` feature.

## Public Shape

Rust code registers a closure:

```rust
let callback = lean_rs::LeanCallbackHandle::register(|event| {
    eprintln!("{}/{}", event.current, event.total);
})?;
let (handle, trampoline) = callback.abi_parts();
```

A Lean export that follows the callback ABI accepts the two values as `USize`:

```text
opaque_handle : USize
trampoline    : USize
```

Lean treats both values as opaque and passes them to the generic C helper that
invokes the Rust trampoline. Callers cannot supply their own trampoline
function pointer through the public API.

## Lifetime

`LeanCallbackHandle` is an RAII registration. Dropping it unregisters the id.
Lean may call the handle only while the Rust value is alive. If a stale id is
called after drop, the trampoline returns `LeanCallbackStatus::StaleHandle`
instead of dereferencing freed Rust memory.

The handle is `Send + Sync`. The registered closure must be
`Fn(LeanCallbackEvent) + Send + Sync + 'static` because Lean may invoke it on
the Lean-bound worker thread, and registry lookup clones an internal `Arc`
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

## Payload

The L1 payload is fixed at two `u64` counters:

```rust
pub struct LeanCallbackEvent {
    pub current: u64,
    pub total: u64,
}
```

Higher layers may interpret those counters as progress ticks or another domain
event. The registry does not decode arbitrary Lean objects inside callbacks and
does not attach theorem-prover policy to the payload.

## Diagnostics

The trampoline returns `LeanCallbackStatus` as a `UInt8` status:

| Status | Meaning |
| --- | --- |
| `Ok` | The callback ran successfully |
| `StaleHandle` | Lean called a dropped callback id |
| `Panic` | The callback panicked and Rust contained the unwind |

For `Panic`, callers can inspect `LeanCallbackHandle::last_error()` while the
handle is still alive. The recorded error has diagnostic code
`lean_rs.internal` and stage `CallbackPanic`.

## Files

- Registry: `crates/lean-rs/src/callback.rs`
- Lean ABI proof export: `lake/lean-rs-host-shims/LeanRsHostShims/Interop.lean`
- Tests: `crates/lean-rs/tests/callback_registry.rs`
