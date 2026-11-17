# Same-process Lean/Rust interop

Use `lean-rs` directly for trusted Lean extensions that call Rust before a Lean export returns. For ordinary
Rust-to-Lean calls, prefer the manifest-backed [shipped-crate recipe](ship-crate-with-lean.md). For process isolation,
timeouts, cycling, and typed rows, use the [worker capability runner](worker-capability-runner.md).

Run the complete in-tree examples:

```sh
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example string_streaming
```

The shared-source build helpers require a configured writer through `LEAN_HOST_MCP_WRITER_EXECUTABLE`; the examples do
not configure it themselves. Both build the generic interop shim and
[`fixtures/interop-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/fixtures/interop-shims), open their shared libraries, and call explicit Lean exports.
The fixture's Lake dependency points at the repository's bundled shim copy; a published consumer should materialize the
package from `lean-rs-interop-shims` as described in the
[generic shim contract](../architecture/callbacks.md#generic-shim-package).

## Build and load

A consumer needs a Lake `lean_lib` shared target and an `@[export]` definition for each Rust-callable entry point. A
Cargo build script can use `CargoLeanCapability` to build the target and emit a manifest, or use
`emit_lean_link_directives_checked` and `build_lake_target` for lower-level control. Use the returned artifact paths
instead of constructing `.lake/build/lib` paths.

The callback examples use a dependency bundle so the shim symbols are visible when the consumer library loads:

```rust,ignore
let runtime = LeanRuntime::init()?;
let bundle = LeanLibraryBundle::open(
    runtime,
    env!("MY_CAPABILITY_DYLIB"),
    [LeanLibraryDependency::path(env!("LEAN_RS_INTEROP_SHIMS_DYLIB"))
        .export_symbols_for_dependents()
        .initializer("lean_rs_interop_shims", "LeanRsInterop")],
)?;
let module = bundle.initialize_module("my_package", "MyCapability")?;
```

Here the environment variables are compile-time outputs supplied by the consumer's build script. The full examples build
the two targets directly and use the returned paths.

## Call Lean

An exported Lean function defines the ABI:

```lean
@[export lean_rs_interop_consumer_add]
def add (a b : UInt64) : UInt64 :=
  a + b
```

The low-level example resolves it with an explicit safety obligation:

```rust,ignore
// SAFETY: this symbol's compiled C ABI is `(UInt64, UInt64) -> UInt64`.
let add = unsafe {
    module.exported_unchecked::<(u64, u64), u64>("lean_rs_interop_consumer_add")
}?;
let answer = add.call(20, 22)?;
```

Sealed ABI traits reject unsupported Rust representations at compile time; they cannot verify that a caller-chosen
signature matches an arbitrary symbol. Use `CargoLeanCapability::export_signature` and `LeanCapability::exported` for
manifest-checked dispatch when the crate owns that contract.

## Receive progress callbacks

Lean accepts the opaque callback id and the crate-owned trampoline address:

```lean
@[export lean_rs_interop_consumer_callback_loop]
def callbackLoop (handle trampoline : USize) (total : UInt64) : IO UInt8 :=
  LeanRsInterop.Callback.Tick.loop handle trampoline total
```

Rust registers a typed callback and passes its ABI parts:

```rust,ignore
// SAFETY: this export has ABI `(USize, USize, UInt64) -> IO UInt8`.
let callback_loop = unsafe {
    module.exported_unchecked::<(usize, usize, u64), LeanIo<u8>>(
        "lean_rs_interop_consumer_callback_loop",
    )
}?;
let callback = LeanCallbackHandle::<LeanProgressTick>::register(|event| {
    eprintln!("{} / {}", event.current, event.total);
    LeanCallbackFlow::Continue
})?;
let (handle, trampoline) = callback.abi_parts();
let status = callback_loop.call(handle, trampoline, 4)?;
```

Decode the returned byte with `LeanCallbackStatus::from_abi` and check `callback.last_error()` when reporting failures;
the runnable examples check the status. Keep the callback handle alive until Lean can no longer call it. Dropping the
handle unregisters the id; a later call returns `StaleHandle` instead of dereferencing freed Rust memory.

Callbacks run synchronously on the Lean-bound thread. They must not re-enter the active Lean call stack. The registry
catches unwinding Rust panics before the C boundary; it does not recover a fatal Lean runtime exit.

## String callbacks

The string example uses the same lifetime and status rules with `LeanStringEvent`:

```lean
@[export lean_rs_interop_consumer_jsonl_stream]
def jsonlStream (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline jsonlRows
```

```rust,ignore
let rows = Arc::new(Mutex::new(Vec::<String>::new()));
let callback_rows = Arc::clone(&rows);
let callback = LeanCallbackHandle::<LeanStringEvent>::register(move |event| {
    match callback_rows.lock() {
        Ok(mut rows) => {
            rows.push(event.value);
            LeanCallbackFlow::Continue
        }
        Err(_) => LeanCallbackFlow::Stop,
    }
})?;
let (handle, trampoline) = callback.abi_parts();
let status = stream.call(handle, trampoline)?;
```

`stream` is the fixture's `lean_rs_interop_consumer_jsonl_stream`, resolved as
`exported_unchecked::<(usize, usize), LeanIo<u8>>` under the same signature obligation. The trampoline copies each
borrowed Lean string into an owned Rust `String` before invoking the callback. No Lean object lifetime escapes.

The example treats JSONL-like text as strings; it defines no row schema or transport. Payloads are sealed:
`LeanProgressTick` carries counters, while `LeanStringEvent` carries one string. Host progress maps the counters into
`LeanProgressEvent`; worker streaming adds its own validated envelopes and IPC above string callbacks.
