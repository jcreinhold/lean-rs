# Callback Registry

`LeanCallbackHandle<P>` lets an explicit Lean export synchronously push a supported payload into Rust. `lean-rs` owns
registration, payload decoding, the C trampoline, panic containment, and stale-handle detection. Worker parent callers
use typed commands and row sinks; callbacks may implement the child-local mechanism.

## Registration and ABI

```rust
let callback = lean_rs::LeanCallbackHandle::<lean_rs::LeanProgressTick>::register(|tick| {
    eprintln!("{}/{}", tick.current, tick.total);
    lean_rs::LeanCallbackFlow::Continue
})?;
let (handle, trampoline) = callback.abi_parts();
```

Lean receives the pair as opaque `USize` values and passes them to a payload-specific
[`LeanRsInterop.Callback`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback.lean) helper. A small
C helper linked into the shim invokes the shared private trampoline ABI:

```c
uint8_t (*)(uintptr_t handle, uint8_t payload_tag,
           uint64_t arg0, uint64_t arg1, lean_object *payload)
```

The tick helper supplies tag `0`, the two counters, and a null object payload. The string helper supplies tag `1` and a
borrowed Lean string. These private C arguments differ from the Lean helper signatures below the public opaque pair.
This requires no globally exported Rust symbol. Consumers pass the crate-owned trampoline, not an arbitrary function
pointer; payload helpers own the matching ABI shape.

## Payload family

`LeanCallbackPayload` is sealed. Each handle accepts one supported shape:

| Payload | Fields | Ownership |
| --- | --- | --- |
| `LeanProgressTick` | `current: u64`, `total: u64` | Plain counters; no host progress policy |
| `LeanStringEvent` | `value: String` | Borrowed Lean string copied into owned Rust UTF-8 before user code runs |

A single enum was rejected because every callback would receive variants it cannot use, making wrong payloads ordinary
application branching. User-defined payload implementations were rejected because they would expose object lifetimes,
reference counts, and trampoline safety to consumers. The sealed family keeps those obligations in `lean-rs`. Byte-array
and arbitrary-object callbacks require a concrete same-process use case before adding ABI and ownership surface. Worker
JSON rows already have their own transport and do not require a new callback payload.

See the [string streaming recipe](../recipes/downstream-interop.md#string-callbacks) for a downstream line protocol and
[structured progress](13-structured-progress.md) for the host interpretation of tick counters.

## Lifetime and threads

Dropping a handle unregisters its id. Lean may use it only while the Rust registration is alive; a later call returns
`StaleHandle` without dereferencing freed context. Handles are process-local and cannot cross an IPC boundary.

`LeanCallbackHandle<P>` is `Send + Sync`, and registered closures must be
`Fn(P) -> LeanCallbackFlow + Send + Sync + 'static`. Registry lookup clones an internal `Arc` before invoking the
closure. A concurrent unregister prevents future lookup; an already-resolved owned callback may complete.

`LeanProgressCallback<'a>` is a scoped specialization whose closure may borrow its caller. The wrapper is
`!Send + !Sync`, owns the borrowed context, and unregisters before releasing it. Keep it alive for the entire
synchronous Lean call using its ABI pair. It also decodes host progress results of shape `Except UInt8 T`, keeping raw
status handling out of host code.

## Flow, panic, and reentrancy

`Continue` lets a cooperating Lean loop proceed; `Stop` requests a clean stop. Helpers must stop and propagate any
nonzero status. A callback cannot forcibly interrupt arbitrary Lean work.

Callbacks execute synchronously on the calling Lean-bound thread. They must not re-enter the same `LeanSession` or Lean
call stack. Queue expensive UI or IPC work for another thread. The registry provides one-way callbacks, not session
reentrancy.

The trampoline catches unwinding Rust panics, records a bounded `LeanError` at `HostStage::CallbackPanic`, and returns
`Panic`. Aborting Rust panics and fatal Lean failures remain process-scoped; see
[panic containment](06-panic-containment.md).

| Status | Byte | Meaning |
| --- | ---: | --- |
| `Ok` | `0` | Callback ran and requested continuation |
| `StaleHandle` | `1` | Registration no longer exists |
| `Panic` | `2` | Rust contained a callback unwind |
| `WrongPayload` | `3` | Handle was used with the wrong payload trampoline |
| `Stopped` | `4` | Callback requested a clean stop |

`last_error()` exposes a recorded panic while the handle is alive; its diagnostic code is `lean_rs.internal`.

## Implementation and checks

The [registry](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/src/callback.rs),
[C helpers](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/shims/lean-rs-interop-shims/c/interop_callback.c), and
[Lean payload helpers](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback) implement the boundary.
[`callback_registry.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/tests/callback_registry.rs) covers payload mismatch, clean stop, stale
ids, owned strings, scoped progress, and contained panics;
[`callback_trampoline.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/tests/callback_trampoline.rs) exercises the lower-level trampoline.
