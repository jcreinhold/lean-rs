# Reusable Interop

`lean-rs` should make Lean/Rust interop feel closer to PyO3 or maturin in the
places where Lean's runtime model allows it: typed Rust calls into Lean exports,
build helpers that hide Lake path rules, reusable callback handles, and worked
downstream examples. It must not claim Python-style reflection. Lean does not
provide a stable C API for looking up and invoking arbitrary Lean declarations
by name; cross-language entry points are explicit `@[export]` ABI boundaries.

The architecture therefore keeps shims, but makes them fewer, deeper, and
reusable. The goal is not "shimless" interop. The goal is a stable interop
boundary that hides callback handles, trampoline safety, object conversion, and
Lake wiring from downstream crates.

## Chosen Boundary

The interop stack has five layers:

- `lean-rs-sys` owns raw Lean C runtime symbols and opaque raw types.
- `lean-rs` owns typed object ownership, exported-function calls, and callback
  handles.
- Generic Lean interop shims own reusable ABI helpers for callbacks and, when
  real callers need them, strings, names, and object plumbing.
- `lean-rs-host` owns theorem-prover host policy: sessions, declaration
  introspection, elaboration, kernel checking, and progress events.
- `lean-toolchain` owns Lake and build-script ergonomics.

This split keeps volatile decisions low in the stack. A callback handle's
lifetime, a trampoline's panic boundary, or Lake's dylib naming rules should be
implemented once and reused. Higher layers should talk in their own terms:
`LeanSession`, declaration filters, cancellation tokens, and progress events.

## Rejected Designs

**No shims.** This design would require a Lean equivalent of Python's
reflective C API: runtime declaration lookup, dynamic invocation by name,
and stable object introspection from C. A source survey of the Lean
runtime found the opposite shape: Lean exposes explicit exported symbols
and an internal runtime API, and theorem-prover operations such as
elaboration, `MetaM`, and server-style workflows live in Lean code. Rust
should not reconstruct those semantics through raw object layouts.

**Host-specific shims only.** The host-specific only design would keep all
callback-bearing work in `lean-rs-host`. The current host shims are appropriate
for host policy, but callbacks, progress, and downstream capability examples
would each need the same lower-level machinery: opaque Rust handles, ABI
trampolines, panic containment rules, reentrancy limits, and string/name
conversion helpers. Repeating those details in every host feature would make
each shim shallow and would expose ABI decisions to consumers.

**Generic interop shims plus host policy.** This is the chosen design. The
generic shim layer supplies reusable mechanisms. `lean-rs-host` then builds
theorem-prover policy on top without owning the callback substrate itself.

## Progress Reporting

Progress is host policy, not a low-level callback primitive.
`LeanProgressSink` reports `LeanSession` phases such as import,
bulk-introspection progress, or kernel-check progress. It is implemented over
the generic callback substrate.

That ordering prevents a host-only callback path from becoming the de facto
interop API. The same substrate must serve downstream Lean extensions that do
not use `lean-rs-host`.

## Contract

New interop surfaces must pass the deep-module test:

- Public APIs expose Rust concepts that callers need, not raw callback pointers
  or Lake path policy.
- Generic mechanisms stay below host policy.
- Panic, abort, and reentrancy limits are documented at the callback boundary.
- Performance claims name the workload and measurement command.

The generic shim package is documented in
[`11-generic-interop-shims.md`](11-generic-interop-shims.md). The build-script
helper path and cache contract are documented in
[`12-interop-build-and-link.md`](12-interop-build-and-link.md). The downstream
recipe is documented in
[`../recipes/downstream-interop.md`](../recipes/downstream-interop.md).
Structured host progress is documented in
[`13-structured-progress.md`](13-structured-progress.md).
Shim packaging is crate-owned: `lean-rs` ships the L1 generic interop shims,
and `lean-rs-host` ships the host shims it loads.
