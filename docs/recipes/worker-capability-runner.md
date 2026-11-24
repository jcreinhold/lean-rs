# Worker capability runner

Use the worker crates for process isolation, typed commands and live rows, diagnostics, request timeouts, and worker
cycling. For Cargo packaging and an app-owned child binary, start with
[ship-crate-with-lean.md](ship-crate-with-lean.md).

The in-tree fixtures exercise the complete path:

```sh
cargo run -p lean-rs-worker-child --example worker_capability_runner
cargo run -p lean-rs-worker-child --example worker_streaming
```

Fixture build helpers require a shared-writer helper configured through `LEAN_RS_WRITER_EXECUTABLE`. The
fixture spans multiple package roots and cannot use a package-local bootstrap. These commands do not configure that
helper themselves.

## Open a capability

The builder names the Lake project, package, shared target, imports, and trusted export signatures:

```rust,ignore
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
.streaming_command_export("lean_rs_interop_consumer_worker_shape_index")
.streaming_command_export("lean_rs_interop_consumer_worker_shape_timeout_after_row")
.open()?;
```

The builder builds or reuses the Lake target, resolves and starts the worker child, checks its handshake, opens the
imports, and validates the requested metadata. It hides artifact paths, pipes, frames, and startup sequencing.
`validate_metadata` also registers that export's `String -> IO String` signature. Register each command's signature with
`streaming_command_export`, `json_command_export`, or the corresponding metadata/doctor helper before opening a
source-built capability. Packaged capabilities carry those signatures in their build manifest instead.

The default child resolver checks the configured override, sibling Cargo profile paths, and the in-tree development
build. Packaged applications should use an explicit `LeanWorkerChild` descriptor, as in the shipped-crate recipe.
`builder.check()` returns a bootstrap report; it is an active startup probe, so it can build artifacts and start a
child.

## Run a typed command

The application owns ordinary serde request, row, and summary types. The fixture uses `ShapeRequest`, `ShapeRow`, and
`ShapeSummary`:

```rust,ignore
let mut session = capability.open_session(None, Some(&progress))?;
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

A row sink implements `LeanWorkerTypedDataSink<Row>::report`. It receives the stream name, per-stream sequence, and
typed payload while Lean produces rows. Buffer tentative rows if the application needs atomic commit: commit only after
the command returns terminal success. The summary reports total rows, per-stream counts, elapsed duration, and optional
application-defined metadata.

| Channel | Purpose |
| --- | --- |
| Rows | Application data decoded into the chosen row type |
| Diagnostics | Worker/capability observations through `LeanWorkerDiagnosticSink` |
| Progress | Coarse ticks through `LeanWorkerProgressSink` |
| Terminal result | Completion status and summary that determine whether rows can be committed |

Malformed envelopes, nonzero callback status, and row or summary decoding failures are typed errors. Decode errors carry
command, stream, and sequence context. A Lean internal panic or abort can kill the child; the supervisor classifies the
exit without treating delivered rows as a successful result. Applications still own schemas, cache validity,
persistence, reporting, and domain-specific meaning.

## Timeout, cancellation, and cleanup

Set request timeout through `LeanWorkerConfig`, a builder, or a live worker/session. One request deadline includes the
bounded write and all responses; receiving progress or rows does not reset it. Startup timeout covers the child
handshake separately.

Cancellation is checked while waiting for IPC even when Lean emits no events. A dispatched cancellation or timeout
invalidates the open session and triggers child shutdown/replacement. The returned error may instead describe cleanup or
replacement failure; do not assume a replacement started merely because a timeout occurred. Reopen a session after
successful recovery. The runner demonstrates a tentative row followed by a timeout, then an explicit cycle and a
successful command.

```rust,ignore
capability.cycle()?;
let report = capability.shutdown()?;
eprintln!("worker shutdown outcome: {:?}", report.outcome);
```

`shutdown()` stops request admission, requests termination, escalates after the shutdown timeout, and reports cleanup
status. Dropping the handle performs best-effort cleanup but cannot return failures. The deprecated `terminate()` API
reports only the legacy exit view. Cycling is the process memory-reset boundary; draining an in-process `SessionPool` is
only cache eviction.

## Wire and raw rows

The Lean streaming export has a fixed ABI:

```lean
@[export lean_rs_interop_consumer_worker_data_stream]
def workerDataStream (requestJson : String) (handle trampoline : USize) : IO UInt8 := ...
```

The child creates a local `LeanCallbackHandle<LeanStringEvent>`. Each callback string is a JSON envelope:

```json
{"stream": "rows", "payload": {"kind": "done", "ordinal": 1}}
```

The callback handle never crosses the process boundary. The worker validates the envelope and assigns sequence numbers
per stream within one request. The parent exposes raw rows as:

```rust,ignore
pub struct LeanWorkerDataRow {
    pub stream: String,
    pub sequence: u64,
    pub payload: serde_json::Value,
}
```

`LeanWorkerSession::run_data_stream` supports schema-less inspection. Typed commands keep validated raw JSON until the
row decoder runs; the raw API parses payloads into `serde_json::Value`. Prefer the typed API when the schema is known.
Neither API defines a downstream tool's business protocol.

Use metadata or doctor exports, rather than a row stream, for capability discovery. Both use `String -> IO String`:
`capability_metadata` decodes commands, capabilities, versions, and extra JSON; `capability_doctor` decodes pass,
warning, and error diagnostics. Metadata validation can be part of startup; doctor remains an explicit operation. Worker
runtime facts come from `runtime_metadata()` and remain separate from application metadata.

## Operational probe

```sh
cargo run --release -p lean-rs-worker-child --example worker_capability_probe
```

The probe records cold startup, session opening, import-once row throughput, cancellation latency, fatal-exit recovery,
cycling, and parent/child RSS. Its `version`, `doctor`, `extract`, `features`, `index`, and `probe` names exercise
generic fixture schemas; they are not reserved commands or a downstream parity claim.

An explicit comparison command is optional:

```sh
LEAN_RS_WORKER_COMPARE_COMMAND='cargo run -p lean-dup -- --help' \
  cargo run --release -p lean-rs-worker-child --example worker_capability_probe
```

Record the exact command, revisions, and output limits with comparison results. The comparison command is outside the
worker contract.
