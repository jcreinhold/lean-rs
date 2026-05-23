# Generic Interop Shims

`lean-rs-interop-shims` is the reusable Lean package below `lean-rs-host-shims`.
It contains ABI helper code that belongs to Lean/Rust interop itself, not to
the theorem-prover host session model.

## Package Boundary

The packaged L1 copy lives under
[`crates/lean-rs/shims/lean-rs-interop-shims/`](../../crates/lean-rs/shims/lean-rs-interop-shims/).
`lean-rs-host` carries its own bundled copy under
[`crates/lean-rs-host/shims/lean-rs-interop-shims/`](../../crates/lean-rs-host/shims/lean-rs-interop-shims/)
so the host crate can build and load its shims without reaching into another crate's source
directory at runtime.
Its public Lean namespace is `LeanRsInterop`.

Current modules:

- `LeanRsInterop.Callback.Tick`: tick callback helper for
  `LeanCallbackHandle<LeanProgressTick>`.
- `LeanRsInterop.Callback.String`: string callback helper for
  `LeanCallbackHandle<LeanStringEvent>`.
- `LeanRsInterop.Callback`: roll-up module that imports both payload-specific
  helper namespaces.

Name, byte, and object helpers belong in this package when a prompt adds a real
caller. They are not present yet, because unused helpers would widen the shim
surface without hiding any current complexity.

## Callback Helper

The callback helpers own the reusable ABI mechanism:

```lean
LeanRsInterop.Callback.Tick.call : USize -> USize -> UInt64 -> UInt64 -> BaseIO UInt8
LeanRsInterop.Callback.Tick.loop : USize -> USize -> UInt64 -> IO UInt8
LeanRsInterop.Callback.String.call : USize -> USize -> @& String -> BaseIO UInt8
LeanRsInterop.Callback.String.loop : USize -> USize -> Array String -> IO UInt8
```

The first `USize` is an opaque Rust callback handle. The second is the Rust
trampoline value returned by `LeanCallbackHandle::abi_trampoline()`. The tick
helper calls `lean_rs_interop_tick_callback_call`, which delivers a
`(current, total)` payload. The string helper calls
`lean_rs_interop_string_callback_call`, which delivers a borrowed Lean
`String` payload. Both helpers return the `UInt8` status produced by the Rust
trampoline. The raw trampoline signature is crate-owned; Lean code should only
pass the opaque values through the payload-specific helpers. The `loop`
functions are small conveniences used by downstream-style fixtures.

Lean code treats the handle and trampoline as opaque tokens. Lifetime,
reentrancy, and Rust panic containment are Rust-side registry contracts; see
[`10-callback-registry.md`](10-callback-registry.md).

## Relationship To Host Shims

`lean-rs-host-shims` still owns the 27 + 4 `lean_rs_host_*` theorem-prover
policy symbols. Host-specific environment queries, elaboration, kernel
checking, and `MetaM` services stay out of `LeanRsInterop`.

The host compatibility export is loaded from the host dylib directly, so it
keeps a local Lean loop and compiles the bundled generic package's C helper
source into the host shared facet.

The host package keeps its test export:

```text
lean_rs_interop_test_callback_loop : USize -> USize -> UInt64 -> IO UInt8
```

That export loops over the same `lean_rs_interop_tick_callback_call` C helper
used by `LeanRsInterop.Callback.Tick.call`. This preserves the Rust test symbol
while moving the callback C helper source into the generic package.

## Downstream Fixture

[`fixtures/interop-shims/`](../../fixtures/interop-shims/) is a small Lake
package that depends on `lean-rs-interop-shims` without importing
`lean-rs-host-shims`. Its `LeanRsInteropConsumer.Callback` module exports a
plain addition function, a tick callback loop, and a string callback loop
through the generic helpers. The fixture verifies that downstream capability
packages can use typed exports and generic callbacks without taking a host
session dependency.

Build commands:

```sh
cd crates/lean-rs/shims/lean-rs-interop-shims && lake build
cd fixtures/interop-shims && lake build
```

Rust consumers should not compute the resulting dylib paths by hand. Use
`lean_toolchain::build_lake_target` for both the generic shim target and the
consumer target; see [`12-interop-build-and-link.md`](12-interop-build-and-link.md)
and [`crates/lean-rs/examples/interop_callback.rs`](../../crates/lean-rs/examples/interop_callback.rs).
The consumer-facing recipe is
[`docs/recipes/downstream-interop.md`](../recipes/downstream-interop.md).

## Non-Goals

The package is not a broad Lean standard library for Rust interop. It should
grow only when a real `lean-rs` caller needs a helper that would otherwise
duplicate ABI, ownership, or conversion rules across packages.
