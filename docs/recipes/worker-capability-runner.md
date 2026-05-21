# Worker Capability Runner

Run the capability runner from a clean checkout:

```sh
cargo run -p lean-rs-worker --example worker_capability_runner
```

This is the normal downstream shape for a project that wants a Lean worker
process, live rows, typed decoding, diagnostics, timeout policy, terminal
completion, and explicit worker cycling without writing a subprocess protocol.
It uses the generic fixture under
[`fixtures/interop-shims/`](../../fixtures/interop-shims/); the command names
are shaped like a downstream tool, but the row schemas are deliberately small
fixture schemas.

If you are packaging your own application, start with
[`ship-crate-with-lean.md`](ship-crate-with-lean.md). That recipe shows the
`build.rs` helper, runtime capability open, and app-owned worker child binary.
This page explains the worker capability behavior using the in-tree fixture.

## What The Example Shows

The example opens a worker-backed capability with
`LeanWorkerCapabilityBuilder`:

```rust
let mut capability = LeanWorkerCapabilityBuilder::new(
    workspace.join("fixtures").join("interop-shims"),
    "lean_rs_interop_consumer",
    "LeanRsInteropConsumer",
    ["LeanRsInteropConsumer.Callback"],
)
.validate_metadata(
    "lean_rs_interop_consumer_worker_shape_metadata",
    serde_json::json!({"source": "worker_capability_runner"}),
)
.open()?;
```

The builder builds the Lake target, resolves and starts
`lean-rs-worker-child`, health-checks the child, opens the configured imports,
and optionally validates generic capability metadata. The caller still names
the Lake project, package, target, and imports because those identify the
capability. The caller does not construct `.lake/build/lib` paths, wire stdio
pipes, decode private frames, or repeat startup ordering.

Packaged applications should use the manifest-backed form shown in
[`ship-crate-with-lean.md`](ship-crate-with-lean.md). The same builder exposes
`check()` for startup probes and doctor commands; it validates the app-owned
worker child, capability manifest/artifacts, worker handshake, import session,
and optional metadata expectation before real work starts.

The row command uses the typed facade:

```rust
let command = LeanWorkerStreamingCommand::<ShapeRequest, ShapeRow, ShapeSummary>::new(
    "lean_rs_interop_consumer_worker_shape_index",
);
let summary = session.run_streaming_command(
    &command,
    &ShapeRequest::default(),
    &rows,
    Some(&diagnostics),
    None,
    Some(&progress),
)?;
```

`ShapeRequest`, `ShapeRow`, and `ShapeSummary` are ordinary serde types owned
by the downstream crate. `lean-rs-worker` owns the transport: child lifecycle,
private framing, live bounded row forwarding, typed diagnostics, progress
events, request timeout, cancellation, terminal completion, and fatal-child
classification.

Rows are live and tentative. A sink sees each row while Lean produces it, but
the caller should commit buffered rows only after `run_streaming_command`
returns terminal success. The terminal summary reports total row count,
per-stream counts, elapsed duration, and optional downstream-defined metadata.

## Channels

Keep the channels separate:

- **Rows** carry downstream data. The worker assigns per-stream sequence values
  and decodes payloads into the caller's row type.
- **Diagnostics** carry worker or capability observations. They are delivered
  through `LeanWorkerDiagnosticSink`, not through row payloads.
- **Progress** carries coarse operation ticks. It is useful for user feedback
  and cancellation boundaries, but it is not the row stream.
- **Terminal summary** says whether delivered rows are committable.

`LeanWorkerSession::run_data_stream` remains available for low-level fixtures
and schema-less tooling. Prefer typed commands for application code because
decode errors include command, stream, and sequence context.

## Timeout And Cycling

The example also runs a command that emits one tentative row and then sleeps.
It lowers the request timeout and expects `LeanWorkerError::Timeout`. On
timeout, the supervisor kills and replaces the child, records a timeout restart
reason, and invalidates the open session. The example then explicitly cycles
the worker and proves a later typed command succeeds in the fresh child.

Use cancellation when the caller knows it wants to stop and can wait for a row
or progress boundary. Use request timeout as the parent-enforced watchdog for
unresponsive Lean work.

## What Downstream Projects Own

`lean-rs-worker` does not define business commands or row schemas. A downstream
project still owns:

- semantic command names and export names;
- request, row, and terminal-summary serde types;
- cache validity rules;
- persistence and reporting;
- any ranking, indexing, or domain-specific interpretation.

The fixture uses command-like names such as `version`, `doctor`, `extract`,
`features`, `index`, and `probe` only to exercise the generic worker capability
layer. They are examples of shape, not APIs that `lean-rs-worker` reserves.
