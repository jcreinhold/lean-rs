# Callback Registry

`lean-rs` exposes a narrow Rust callback registry for Lean-to-Rust calls. This is a same-process primitive, not a
`LeanSession` feature and not the worker-facing data streaming API. A callback handle is the low-level same-process
mechanism used when an exported Lean function must push data into Rust before it returns. Worker-style callers should
normally use the worker crates' typed commands and row sinks; the worker child may use callbacks internally, but parent
callers do not receive or pass callback handles.

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

Lean treats both values as opaque and passes them to a payload-specific helper such as `LeanRsInterop.Callback.Tick` or
`LeanRsInterop.Callback.String`. The helper invokes the Rust trampoline through its matching C symbol. Callers cannot
supply their own trampoline function pointer through the public API.

### Underlying ABI

Lean never calls the Rust trampoline pointer directly. It passes the opaque handle and trampoline `USize` values to a
small C helper linked into the shim dylib (for the tick payload, `lean_rs_interop_tick_callback_call`), which casts the
trampoline to its stable C signature and invokes it:

```c
uint8_t (*)(uintptr_t handle, uint64_t current, uint64_t total)
```

This keeps the mechanism free of any process-global exported Rust symbol: the Rust caller owns both the handle and the
trampoline, and the shim knows only the C ABI shape.

## Payloads

`LeanCallbackHandle<P>` is generic over `P: LeanCallbackPayload`, but `LeanCallbackPayload` is sealed. Downstream crates
can use supported payloads; they cannot add new ABI shapes or decode raw Lean objects themselves.

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

`LeanProgressTick` is the two-counter payload. It carries no host policy by itself; `lean-rs-host` interprets it as
progress when reporting long-running session work.

`LeanStringEvent` copies a borrowed Lean `String` into an owned Rust `String` before user code runs. No Lean object
lifetime escapes the trampoline. Trusted same-process line-oriented protocols can use it directly; see
[`../recipes/string-callback-streaming.md`](../recipes/string-callback-streaming.md) for a JSONL-like example.

## Flow And Lifetime

Callbacks return `LeanCallbackFlow`. `Continue` lets the Lean-side callback loop continue; `Stop` asks Lean to stop
cleanly and return `LeanCallbackStatus::Stopped`.

`LeanCallbackHandle<P>` is an RAII registration. Dropping it unregisters the id. Lean may call the handle only while the
Rust value is alive. If a stale id is called after drop, the trampoline returns `LeanCallbackStatus::StaleHandle`
instead of dereferencing freed Rust memory.

The handle is `Send + Sync`. The registered closure must be `Fn(P) -> LeanCallbackFlow + Send + Sync + 'static` because
Lean may invoke it on the Lean-bound worker thread, and registry lookup clones an internal `Arc` before running the
callback.

`LeanProgressCallback<'a>` is the scoped progress specialization used by `lean-rs-host`. Its closure may borrow from the
caller, so the value is not `Send` or `Sync`; it must stay alive for exactly the synchronous Lean call that receives its
`(handle, trampoline)` pair. The type owns the borrowed context and unregisters the callback handle before releasing
that context. It also decodes host progress shim results of shape `Except UInt8 T`, so host code does not inspect raw
callback status bytes.

## Panic And Reentrancy

Rust panics must not cross Lean or C frames. The registry trampoline catches unwinding Rust panics, records a
`LeanError` with `HostStage::CallbackPanic`, and returns `LeanCallbackStatus::Panic` to Lean. Aborting Rust panics and
Lean internal panics remain process-scoped.

Callbacks run synchronously on the Lean-bound thread. A callback invoked during a Lean call must not call back into the
same `LeanSession` or re-enter the same Lean call stack. The registry is a one-way callback mechanism; it does not make
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

For `Panic`, callers can inspect `LeanCallbackHandle::last_error()` while the handle is still alive. The recorded error
has diagnostic code `lean_rs.internal` and stage `CallbackPanic`.

## Files

- Registry: `crates/lean-rs/src/callback.rs`
- Generic Lean helpers: `crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback/Tick.lean` and
  `crates/lean-rs/shims/lean-rs-interop-shims/LeanRsInterop/Callback/String.lean`
- C trampoline helper: `crates/lean-rs/shims/lean-rs-interop-shims/c/interop_callback.c`
- Tests: `crates/lean-rs/tests/callback_registry.rs` and `crates/lean-rs/tests/callback_trampoline.rs`
