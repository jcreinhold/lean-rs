# Callbacks and interop shims

`LeanCallbackHandle<P>` lets an explicit Lean export synchronously push a supported payload into Rust. `lean-rs` owns
registration, payload decoding, the C trampoline, panic containment, and stale-handle detection. Callbacks never cross a
process boundary: a worker parent receives progress through `LeanWorkerProgressSink` and results as typed replies.

## Registration and ABI

```rust
let callback = lean_rs::LeanCallbackHandle::<lean_rs::LeanProgressTick>::register(|tick| {
    eprintln!("{}/{}", tick.current, tick.total);
    lean_rs::LeanCallbackFlow::Continue
})?;
let (handle, trampoline) = callback.abi_parts();
```

Lean receives the pair as opaque `USize` values and passes them to a payload-specific
[`LeanRsInterop.Callback`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback.lean)
helper. A small C helper linked into the shim invokes the shared private trampoline ABI:

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
and arbitrary-object callbacks require a concrete same-process use case before adding ABI and ownership surface.

See the [string streaming recipe](../recipes/downstream-interop.md#string-callbacks) for a downstream line protocol and
[structured progress](../same-process-controls.md#progress) for the host interpretation of tick counters.

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
[panic containment](worker-processes.md).

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
[C helpers](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/shims/lean-rs-interop-shims/c/interop_callback.c),
and
[Lean payload helpers](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback)
implement the boundary.
[`callback_registry.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/tests/callback_registry.rs)
covers payload mismatch, clean stop, stale ids, owned strings, scoped progress, and contained panics;
[`callback_trampoline.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/tests/callback_trampoline.rs)
exercises the lower-level trampoline.

## Generic shim package

The `lean-rs-interop-shims` Lake package holds reusable Lean/C helpers under `LeanRsInterop`, below host-session policy.
Explicit `@[export]` calls carry the ABI; Rust owns object and callback safety. Custom typed-FFI consumers can use this
package without depending on theorem-prover services. Add helpers for a repeated ABI or ownership need.

The canonical Lean/C payload lives in
[`crates/lean-rs/shims/lean-rs-interop-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs/shims/lean-rs-interop-shims).
A Lake project outside this repository can `require` the package from the lean-rs Git repository at a release tag, with
that directory as its path. The host carries a bundled copy in
[`crates/lean-rs-host/shims/lean-rs-interop-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs-host/shims/lean-rs-interop-shims)
so its published package can prepare shims without locating another crate's source directory at runtime.

The Lean/C payload copies must be byte-identical, including modules the host does not import.
[`interop_shims_parity`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/tests/interop_shims_parity.rs)
checks this. Edit the canonical payload and synchronize the bundled copy.

| Module | Role |
| --- | --- |
| `LeanRsInterop.Callback.Tick` | Two-counter callbacks |
| `LeanRsInterop.Callback.String` | Borrowed Lean string callbacks copied to Rust |
| `LeanRsInterop.Callback` | Imports both callback helper namespaces |

```lean
LeanRsInterop.Callback.Tick.call : USize -> USize -> UInt64 -> UInt64 -> BaseIO UInt8
LeanRsInterop.Callback.Tick.loop : USize -> USize -> UInt64 -> IO UInt8
LeanRsInterop.Callback.String.call : USize -> USize -> @& String -> BaseIO UInt8
LeanRsInterop.Callback.String.loop : USize -> USize -> Array String -> IO UInt8
```

The first two values are the opaque registration id and crate-owned trampoline. The C helpers
`lean_rs_interop_tick_callback_call` and `lean_rs_interop_string_callback_call` invoke it with the matching payload and
return its status. Lean helpers stop on nonzero status, subject to the lifetime and panic rules above.

The host's `lean_rs_interop_test_callback_loop` export uses the same generic tick C helper. Its shared facet compiles
the bundled helper source instead of defining a second callback ABI.

[`fixtures/interop-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/fixtures/interop-shims) and the
[`interop_callback`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/examples/interop_callback.rs)
example check addition, ticks, and strings without host sessions. The [build contract](build-and-link.md) owns Lake
target resolution. Interop changes also need host progress, payload parity, API review, and the relevant checks in
[release](../release.md).
