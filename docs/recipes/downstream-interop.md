# Downstream Lean/Rust Interop

Run the worked example from a clean checkout:

```sh
cargo run -p lean-rs --example interop_callback
```

The example stays below `lean-rs-host`. Rust builds a generic Lean interop shim
target and a downstream-style Lake target, opens both dylibs through
`lean-rs`, calls one ordinary Lean export, and then lets Lean call a Rust
callback through `LeanCallbackHandle`.

## Files A Consumer Needs

A downstream package needs the same pieces as
[`fixtures/interop-shims/`](../../fixtures/interop-shims/):

- A Lake package with a `lean_lib` shared facet.
- A `require lean_rs_interop_shims from ...` line pointing at the bundled
  `crates/lean-rs/shims/lean-rs-interop-shims` package when Lean will call
  Rust callbacks.
- Explicit `@[export]` definitions for every Rust-callable Lean entry point.
- A Rust build script that calls `lean_toolchain::emit_lean_link_directives_checked`
  and `lean_toolchain::build_lake_target`.
- Rust code that opens the generic shim dylib globally before opening the
  downstream capability dylib when callback helpers are used.

The build helper returns the dylib path. Consumers should pass that path through
`cargo:rustc-env=...` or another build-script output of their choosing; they
should not construct `.lake/build/lib` paths by hand.

## Calling Lean From Rust

Lean exports are explicit ABI boundaries:

```lean
@[export lean_rs_interop_consumer_add]
def add (a b : UInt64) : UInt64 :=
  a + b
```

Rust opens the dylib, initializes the Lake module, resolves the exported
symbol, and calls it through a typed handle:

```rust
let library = LeanLibrary::open(runtime, env!("MY_CAPABILITY_DYLIB"))?;
let module = library.initialize_module("my_package", "MyCapability")?;
let add = module.exported::<(u64, u64), u64>("lean_rs_interop_consumer_add")?;
let answer = add.call(20, 22)?;
```

The argument tuple and return type are checked by `lean-rs`'s sealed ABI traits.
Unsupported Rust types fail at compile time.

## Calling Rust From Lean

Callbacks use the generic interop shim package:

```lean
@[export lean_rs_interop_consumer_callback_loop]
def callbackLoop (handle trampoline : USize) (total : UInt64) : IO UInt8 :=
  LeanRsInterop.Callback.Tick.loop handle trampoline total
```

Rust registers a callback and passes the opaque handle plus crate-owned
trampoline to Lean:

```rust
let callback = LeanCallbackHandle::<LeanProgressTick>::register(|event| {
    eprintln!("{} / {}", event.current, event.total);
    LeanCallbackFlow::Continue
})?;
let (handle, trampoline) = callback.abi_parts();
let status = callback_loop.call(handle, trampoline, 4)?;
```

Keep the `LeanCallbackHandle` alive until Lean cannot call it again. Dropping
the handle unregisters the id; a stale Lean call returns
`LeanCallbackStatus::StaleHandle` instead of dereferencing freed Rust memory.
Callbacks run synchronously on the Lean-bound thread and must not re-enter the
same Lean call stack.

String callbacks use the same handle/trampoline lifetime rules with a different
payload helper:

```lean
@[export lean_rs_interop_consumer_string_callback_loop]
def stringCallbackLoop (handle trampoline : USize) (payloads : Array String) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline payloads
```

Rust registers `LeanCallbackHandle::<LeanStringEvent>` for that export. The
trampoline copies the borrowed Lean string into an owned Rust `String` before
calling user code. For a complete line-oriented string streaming example, see
[`string-callback-streaming.md`](string-callback-streaming.md).

## What This Is Not

This is closer to PyO3/maturin in build wiring and typed calls, but it is not
Python-style reflection. Lean does not expose a stable C API for discovering
and invoking arbitrary definitions at runtime. A Lean/Rust boundary is an
explicit `@[export]`, and a downstream crate still builds a Lake target.

Use `lean-rs-host` only when the application needs theorem-prover host policy:
sessions, imports, declaration introspection, elaboration, kernel checking, or
bounded `MetaM` services.
