# Host examples

Run these from the workspace with `-p lean-rs-host`. They exercise same-process host services against `fixtures/lean`;
long-running import-heavy applications should use the [production worker pattern](../../../docs/production-hosting.md).

## Setup

Build the fixture before running host examples:

```sh
(cd fixtures/lean && lake build)
```

Shared-source build helpers also require a writer configured through `LEAN_HOST_MCP_WRITER_EXECUTABLE`; these examples
do not configure one themselves. See the [shipping recipe](../../../docs/recipes/ship-crate-with-lean.md) for the build
ownership requirements. Host Meta and elaboration services come from the bundled host shims, while the fixture supplies
test declarations and expressions.

The examples derive fixture paths from compile-time `env!("CARGO_MANIFEST_DIR")`. Setting that variable at runtime does
not change the compiled path. Missing artifacts fail at the build, library-load, or capability-load step that needs
them; see the reported diagnostic code and [diagnostics guide](../../../docs/diagnostics.md).

## Choose an example

| Command | Behavior and expected output |
| --- | --- |
| `cargo run -p lean-rs-host --example theorem_query` | Opens a session and contrasts `Nat.add` (`definition`) with `Nat.add_zero` (`theorem`); prints the declaration total and `ok`. The total varies by toolchain. |
| `cargo run -p lean-rs-host --example proof_check` | Kernel-checks `theorem demo_proof_check : 1 + 1 = 2 := rfl`, rechecks the evidence, and prints a bounded summary. Pretty-printed types vary by toolchain. |
| `cargo run -p lean-rs-host --example meta_query` | Elaborates `(Nat.succ 0 : Nat)`, calls `infer_type`, and handles each `LeanMetaResponse` status. A supported run prints `status=Ok service=infer_type` and `ok`. |
| `cargo run -p lean-rs-host --example progress` | Prints progress ticks, queries three declarations, then cancels through a shared token; prints `cancelled_code=lean_rs.cancelled`. |
| `cargo run -p lean-rs-host --example tour` | Composes host open, capability load, import, elaboration, kernel checking, a bulk query, and `Meta.whnf`; prints `name=<stage> elapsed_us=<u64>`. |
| `cargo run -p lean-rs-host --example lake_build_helper` | Runs a Lake shared-library build through `lean-toolchain`; prints Cargo rerun directives and the resolved `dylib=...` path (`.dylib` on macOS, `.so` on Linux). |
| `cargo run --release -p lean-rs-host --example long_session_memory` | Emits workload parameters, pool/reuse counters, and RSS checkpoints across imports, warm queries, and drops. |

The query/check and long-session examples install tracing subscribers; set `RUST_LOG=lean_rs=debug` for their structured
spans. Query/check examples propagate errors and print their diagnostic code; the tour uses direct assertions for its
walkthrough. Do not infer one error-handling policy from all examples.

If a Meta response is `Unsupported`, inspect the installed host-shim capability and rebuild those shims as needed. The
fixture does not export the host Meta contract. `TimeoutOrHeartbeat` reports the bounded Meta request's limit; inspect
its `LeanMetaOptions` before changing a budget.

The memory reproducer is bounded by repository defaults. Increase `LEAN_RS_LONG_SESSION_IMPORTS` only after inspecting
the previous peak and setting an explicit `LEAN_RS_LONG_SESSION_MAX_RSS_KIB`. RSS checks act before later imports, so
one import can overshoot a ceiling. It is a retention diagnostic, not an unbounded production soak test. Reproduction
modes and historical measurements live in [Long-session memory](../../../docs/safety/long-session-memory.md); scripted
captures live in the [profiling guide](../../../profiling/README.md).

## Callback examples

Callbacks belong to `lean-rs`, below host sessions:

```sh
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example string_streaming
```

Follow [Downstream interop](../../../docs/recipes/downstream-interop.md) for export signatures, shim packaging, callback
lifetimes, and string payloads. Read the [concurrency contract](../../../docs/architecture/concurrency.md) before
moving integration work to another OS thread.
