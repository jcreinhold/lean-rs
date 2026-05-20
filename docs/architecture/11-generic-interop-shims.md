# Generic Interop Shims

`lean-rs-interop-shims` is the reusable Lean package below `lean-rs-host-shims`.
It contains ABI helper code that belongs to Lean/Rust interop itself, not to
the theorem-prover host session model.

## Package Boundary

The package lives at [`lake/lean-rs-interop-shims/`](../../lake/lean-rs-interop-shims/).
Its public Lean namespace is `LeanRsInterop`.

Current modules:

- `LeanRsInterop.Callback`: callback invocation helper for the L1
  `lean_rs::LeanCallbackHandle` ABI.

String, name, and object helpers belong in this package when a prompt adds a
real caller. They are not present yet, because unused helpers would widen the
shim surface without hiding any current complexity.

## Callback Helper

The callback helper owns the reusable ABI mechanism:

```lean
LeanRsInterop.Callback.call : USize -> USize -> UInt64 -> UInt64 -> BaseIO UInt8
LeanRsInterop.Callback.loop : USize -> USize -> UInt64 -> IO UInt8
```

The first `USize` is an opaque Rust callback handle. The second is the Rust
trampoline value returned by `LeanCallbackHandle::abi_trampoline()`.
`LeanRsInterop.Callback.call` calls the linked C symbol
`lean_rs_interop_callback_call`, which invokes the Rust trampoline with
`(handle, current, total)` and returns a `UInt8` status. `loop` is a small
convenience used by downstream-style fixtures.

Lean code treats the handle and trampoline as opaque tokens. Lifetime,
reentrancy, and Rust panic containment are Rust-side registry contracts; see
[`10-callback-registry.md`](10-callback-registry.md).

## Relationship To Host Shims

`lean-rs-host-shims` still owns the 18 + 4 `lean_rs_host_*` theorem-prover
policy symbols. Host-specific environment queries, elaboration, kernel
checking, and `MetaM` services stay out of `LeanRsInterop`.

The prompt-40 host compatibility export is loaded from the host dylib directly,
so it keeps a local Lean loop and compiles the generic package's C helper
source into the host shared facet. The source of truth remains
`lake/lean-rs-interop-shims/c/interop_callback.c`; the host package only links
that helper for the compatibility export.

The host package keeps its prompt-40 test export:

```text
lean_rs_interop_test_callback_loop : USize -> USize -> UInt64 -> IO UInt8
```

That export loops over the same `lean_rs_interop_callback_call` C helper used
by `LeanRsInterop.Callback.call`. This preserves the Rust test symbol while
moving the callback C helper source into the generic package.

## Downstream Fixture

[`fixtures/interop-shims/`](../../fixtures/interop-shims/) is a small Lake
package that depends on `lean-rs-interop-shims` without importing
`lean-rs-host-shims`. Its `LeanRsInteropConsumer.Callback` module exports a
callback loop through the generic helper. The fixture verifies that downstream
capability packages can use the generic interop package without taking a host
session dependency.

Build commands:

```sh
cd lake/lean-rs-interop-shims && lake build
cd fixtures/interop-shims && lake build
```

## Non-Goals

The package is not a broad Lean standard library for Rust interop. It should
grow only when a real `lean-rs` caller needs a helper that would otherwise
duplicate ABI, ownership, or conversion rules across packages.
