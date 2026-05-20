# Streaming Strings From Lean

Run the example from a clean checkout:

```sh
cargo run -p lean-rs --example string_streaming
```

This recipe stays below `lean-rs-host`. It shows a downstream Lake target
streaming JSONL-like rows to Rust through
`LeanCallbackHandle<LeanStringEvent>`. `lean-rs` owns the callback handle,
trampoline, string copy, stale-handle status, and panic boundary. The row schema
belongs to the downstream application.

## Lean Export

The fixture under [`fixtures/interop-shims/`](../../fixtures/interop-shims/)
depends only on the generic interop shims:

```lean
require «lean_rs_interop_shims» from "../../crates/lean-rs/shims/lean-rs-interop-shims"
```

It exports a function that accepts the opaque callback handle and trampoline
values, then sends a fixed sequence of strings through the string helper:

```lean
@[export lean_rs_interop_consumer_jsonl_stream]
def jsonlStream (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline jsonlRows
```

Each row is just a `String`. The example uses JSONL-like text because that is a
common worker protocol shape, but neither Lean nor Rust parses the row.

## Rust Call Site

Rust builds the generic shim target and the downstream fixture target with
`lean_toolchain::build_lake_target`, opens the generic shim dylib globally, then
opens the consumer dylib normally.

The callback is typed to the string payload:

```rust
let rows = Arc::new(Mutex::new(Vec::new()));
let callback_rows = Arc::clone(&rows);
let callback = LeanCallbackHandle::<LeanStringEvent>::register(move |event| {
    callback_rows.lock().unwrap().push(event.value);
    LeanCallbackFlow::Continue
})?;
let (handle, trampoline) = callback.abi_parts();
let status = stream.call(handle, trampoline)?;
```

Keep the `LeanCallbackHandle<LeanStringEvent>` alive until the Lean export has
returned and cannot call the handle again. Dropping the handle unregisters the
id. A stale call returns `LeanCallbackStatus::StaleHandle`; it does not
dereference freed Rust memory.

The trampoline copies the borrowed Lean string into an owned Rust `String`
before it invokes user code, so no Lean object lifetime escapes the callback
boundary.

## Relationship To Progress

String streaming and host progress use the same callback registry, but they are
different payloads:

- `LeanProgressTick` carries `(current, total)` counters. `lean-rs-host` maps it
  into `LeanProgressEvent` for session progress.
- `LeanStringEvent` carries one owned Rust `String`. Downstream code can use it
  for line-oriented protocols such as JSONL.

Payloads are sealed. Downstream crates cannot implement arbitrary callback
payloads or pass raw callback pointers to Lean.

## Limits

This is not a transport framework. `lean-rs` does not define a JSON schema,
parse rows, multiplex streams, or retry failed callbacks. It provides the typed
Lean-to-Rust string callback boundary; application protocol belongs above it.
