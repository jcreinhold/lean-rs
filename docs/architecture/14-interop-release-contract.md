# Interop Release Contract

The reusable interop stack is stable enough for `0.1.x` consumers when they stay inside the documented boundaries:
explicit Lean exports, typed Rust calls, crate-owned callback handles, bundled shim packages, and `lean-toolchain` build
helpers. The model is close to PyO3/maturin in the parts Lean exposes: a Rust crate can build a Lean shared-library
target, load it, call typed exports, and pass an opaque Rust callback handle back into Lean. It is not Python-style
reflection.

## Source Of Truth

Consumer-facing contracts live in these documents:

- [`../recipes/downstream-interop.md`](../recipes/downstream-interop.md): L1 interop without `lean-rs-host`.
- [`../recipes/string-callback-streaming.md`](../recipes/string-callback-streaming.md): L1 same-process Lean-to-Rust
  string callbacks without `lean-rs-host`.
- [`../recipes/worker-capability-runner.md`](../recipes/worker-capability-runner.md): the worker-facing command path for
  live rows, diagnostics, terminal summaries, timeouts, and cycling.
- [`03-host-stack.md`](03-host-stack.md): L2 `LeanHost` / `LeanCapabilities` / `LeanSession` surface and host method
  signatures.
- [`10-callback-registry.md`](10-callback-registry.md): callback handle lifetime, panic, and reentrancy rules.
- [`11-generic-interop-shims.md`](11-generic-interop-shims.md): generic Lean shim ownership.
- [`12-interop-build-and-link.md`](12-interop-build-and-link.md): `build_lake_target` cache, diagnostics, and Cargo
  output rules.
- [`13-structured-progress.md`](13-structured-progress.md): `LeanProgressSink` semantics.
- [`../lean-rs-host-capability-contract.md`](../lean-rs-host-capability-contract.md): fixed host shim symbol contract.

Implementation notes, spike rationale, and rejected designs remain in [`08-reusable-interop.md`](08-reusable-interop.md)
and [`09-callback-abi-spike.md`](09-callback-abi-spike.md). They explain why the current boundary exists, but the
documents above are the release contract consumers should follow.

## What The Stack Provides

`lean-rs` provides the L1 primitive: a `LeanRuntime`, loaded `LeanLibrary` / `LeanModule` handles, typed `LeanExported`
calls, semantic object handles, structured errors, and `LeanCallbackHandle<P>` for synchronous same-process Lean-to-Rust
callbacks. Callback handles carry only opaque ABI values and a crate-owned trampoline; downstream code does not pass
arbitrary function pointers to Lean. Payloads are a sealed family owned by `lean-rs`; current payloads are
`LeanProgressTick` and `LeanStringEvent`. This is the mechanism layer. Worker-class interfaces should expose
`lean-rs-worker` typed commands and row sinks instead of callback handles.

`lean-toolchain` provides the build-script path: link directives for the active Lean toolchain and `build_lake_target`
for Lake shared-library targets. It owns Lake dylib naming, cache metadata, Cargo rerun directives, and typed link/build
diagnostics.

`lean-rs-host` provides theorem-prover policy above L1: sessions, imports, declaration introspection, source ranges,
filtered listing, elaboration, kernel checking, bounded `MetaM`, pooling, cooperative cancellation, and structured
progress. It ships the host and generic shim sources it needs, builds them on demand, and opens them beside the consumer
capability dylib.

`lean-rs-worker` provides the process-boundary product interface for worker-style tools: builder-managed capability
startup, typed commands, live rows, diagnostic sinks, terminal summaries, request timeouts, and worker cycling. It may
use L1 callbacks inside the child, but the parent-facing API is worker IPC, not cross-process callbacks.

## What It Does Not Provide

The stack does not discover or invoke arbitrary Lean definitions by reflection. Every cross-language entry point is an
explicit `@[export]` with a supported ABI shape.

The stack does not make Lean internal panics recoverable in-process. Rust callback panics are caught at the callback
trampoline boundary; Lean `panic!`, `abort`, `std::exit`, and foreign unwinds remain process-scoped.

The stack does not provide cross-process callback handles. Handles are valid only in the process that registered them
and only while the Rust `LeanCallbackHandle` is alive.

The stack does not require new callback payloads for worker JSON row streaming. Worker rows already travel over the
worker protocol as JSON or validated raw JSON; callback payload expansion is only for same-process L1 interop needs.

The stack is not shimless. The release boundary is fewer, deeper, crate-owned shims: generic interop shims for callbacks
and host shims for theorem-prover policy.

## Release Gates

Before a release that changes interop or host progress, run:

```sh
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example string_streaming
cargo run -p lean-rs-host --example progress
cargo run -p lean-rs-worker --example worker_capability_runner
cargo test -p lean-rs --test callback_trampoline -- --nocapture
cargo test -p lean-rs --test callback_registry -- --nocapture
cargo test -p lean-rs-host --test progress -- --nocapture
cargo test -p lean-rs-worker --test streaming_runner -- --nocapture
cargo test -p lean-rs-worker --test typed_command -- --nocapture
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --baseline <saved-baseline>
cargo bench -p lean-rs-worker --bench row_payload -- --baseline <saved-baseline>
cargo bench -p lean-rs-worker --bench worker_capability -- --sample-size 10
```

The sanitizer workflow must continue to run the callback trampoline, callback registry, panic containment, host
progress, and worker fatal-exit fixtures on Linux ASan. Public API baselines under [`../api-review/`](../api-review/)
are regenerated in the same commit as any intentional public surface change.
