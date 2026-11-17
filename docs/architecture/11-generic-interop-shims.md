# Generic Interop Shims

The `lean-rs-interop-shims` crate packages reusable Lean/C ABI helpers under the `LeanRsInterop` namespace. It sits
below theorem-prover host policy: Rust owns object and callback safety, these helpers carry explicit ABI calls, and
`lean-rs-host` adds sessions, elaboration, kernel checking, queries, and progress policy.

## Why keep generic shims

Lean interop uses explicit `@[export]` boundaries rather than reflection over arbitrary declarations. Removing shims
would force Rust to reconstruct Lean operations from runtime layouts. Keeping every helper in the host package would
instead make ordinary typed-FFI consumers depend on theorem-prover services or duplicate callback wiring. A reusable
mechanism package avoids both problems.

Build helpers in `lean-toolchain` hide Lake naming, linking, and caching. Generic callbacks belong in `lean-rs`; host
phase names and cancellation checkpoints belong in `lean-rs-host`. New helpers should hide a real repeated ABI or
ownership concern. Unused name, byte, and object helpers do not earn public surface merely by being possible.

## Package ownership

The canonical crate and Lean/C payload live in
[`crates/lean-rs/shims/lean-rs-interop-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs/shims/lean-rs-interop-shims).
`materialize_source_package` copies that payload into a caller-owned build root. The host carries a bundled copy in
[`crates/lean-rs-host/shims/lean-rs-interop-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs-host/shims/lean-rs-interop-shims) so its
published package can prepare shims without locating another crate's source directory at runtime.

The Lean/C payload copies must be byte-identical, including modules the host does not import.
[`interop_shims_parity`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/tests/interop_shims_parity.rs) checks this. Edit the canonical
payload and synchronize the bundled copy. Rust crate scaffolding (`Cargo.toml`, `src/`, README, and licenses) is
intentionally excluded from payload parity.

| Module | Role |
| --- | --- |
| `LeanRsInterop.Callback.Tick` | Two-counter callbacks |
| `LeanRsInterop.Callback.String` | Borrowed Lean string callbacks copied to Rust |
| `LeanRsInterop.Callback` | Imports both callback helper namespaces |
| `LeanRsInterop.Worker.Stream` | Worker row/diagnostic/terminal envelope helpers |

The worker helper contract lives in [Lean envelope helpers](18-worker-data-streaming.md#lean-envelope-helpers).

## Callback helpers

```lean
LeanRsInterop.Callback.Tick.call : USize -> USize -> UInt64 -> UInt64 -> BaseIO UInt8
LeanRsInterop.Callback.Tick.loop : USize -> USize -> UInt64 -> IO UInt8
LeanRsInterop.Callback.String.call : USize -> USize -> @& String -> BaseIO UInt8
LeanRsInterop.Callback.String.loop : USize -> USize -> Array String -> IO UInt8
```

The first two values are the opaque registration id and crate-owned trampoline. The C helpers
`lean_rs_interop_tick_callback_call` and `lean_rs_interop_string_callback_call` invoke that trampoline with the matching
payload and return its status. Lean helpers stop on nonzero status. Lifetime, payload validation, reentrancy, and Rust
panic rules belong to the [callback registry](10-callback-registry.md).

Host shims keep theorem-prover services outside this package. Their compatibility test export,
`lean_rs_interop_test_callback_loop`, runs a local loop through the same generic tick C helper. The host shared facet
compiles the bundled helper source rather than introducing another Rust callback ABI.

## Downstream use and validation

[`fixtures/interop-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/fixtures/interop-shims) imports the generic package without the host shims. It
exports addition, tick callbacks, and string callbacks. The fixture and
[`interop_callback`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/examples/interop_callback.rs) example check that typed FFI and callbacks work
without a host-session dependency.

Use `lean_toolchain::build_lake_target` to build and locate shared targets rather than calculating dylib paths. See the
[downstream recipe](../recipes/downstream-interop.md) and [build/link contract](12-interop-build-and-link.md). Manual
Lean package checks, when needed, are:

```sh
(cd crates/lean-rs/shims/lean-rs-interop-shims && lake build)
(cd fixtures/interop-shims && lake build)
```

Interop changes also need the callback tests, host progress checks, API review, and relevant release validation in
[the release checklist](../release.md).
