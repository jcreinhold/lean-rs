# Worker Process Boundary

For the normal worker-capability path, start with [`worker-capability-runner.md`](worker-capability-runner.md). This
page explains the lower-level process-boundary mechanics and the raw row escape hatch.

Run the worker streaming example from a clean checkout:

```sh
cargo run -p lean-rs-worker --example worker_streaming
```

The example uses `LeanWorkerCapabilityBuilder` to build the downstream Lake target, start a worker child, open a worker
session, validate capability metadata, run a streaming Lean export, print JSONL-like rows, cycle the worker, and prove
that the next request succeeds in a fresh child.

Use this path when the application needs process isolation or memory cycling. Use direct same-process callbacks, such as
[`string-callback-streaming.md`](string-callback-streaming.md), when the Lean extension is trusted and same-process
execution is acceptable.

## What The Example Builds

The fixture at [`fixtures/interop-shims/`](../../fixtures/interop-shims/) depends on the generic interop shims and
exports:

```lean
@[export lean_rs_interop_consumer_worker_data_stream]
def workerDataStream (requestJson : String) (handle trampoline : USize) : IO UInt8 := ...
```

The worker runner fixes the ABI:

```text
String request_json -> USize callback_handle -> USize callback_trampoline -> IO UInt8
```

`request_json` is downstream-owned request text. The child creates an in-process `LeanCallbackHandle<LeanStringEvent>`,
passes the handle and trampoline to Lean, validates each callback string as a row envelope, and forwards rows to the
parent as `LeanWorkerDataRow` values. The callback handle never crosses the process boundary.

## Row Shape

Each callback string must be a JSON object:

```json
{
  "stream": "rows",
  "payload": {
    "kind": "done",
    "ordinal": 1
  }
}
```

The parent receives:

```rust
pub struct LeanWorkerDataRow {
    pub stream: String,
    pub sequence: u64,
    pub payload: serde_json::Value,
}
```

`stream` is a caller-defined channel name. `sequence` is assigned by the worker crates per stream inside one request.
`payload` is arbitrary JSON. The worker validates the envelope and ordering mechanics; the downstream crate owns the row
schema.

## Rust Call Site

The preferred call site uses the typed command facade. The caller defines its own serde request, row, and terminal
metadata types:

```rust
#[derive(serde::Serialize)]
struct ScanRequest {
    source: String,
}

#[derive(Clone, serde::Deserialize)]
struct ScanRow {
    kind: String,
    ordinal: u64,
}

#[derive(serde::Deserialize)]
struct ScanSummary {
    fixture: String,
    ok: bool,
}

struct Rows;

impl LeanWorkerTypedDataSink<ScanRow> for Rows {
    fn report(&self, row: LeanWorkerTypedDataRow<ScanRow>) {
        // Commit policy belongs to the caller. The row is still tentative
        // until the command returns terminal success.
        println!("{}:{} {:?}", row.stream, row.sequence, row.payload.kind);
    }
}
```

Then it builds and opens the worker-backed capability:

```rust
let mut capability = LeanWorkerCapabilityBuilder::new(
    "fixtures/interop-shims",
    "lean_rs_interop_consumer",
    "LeanRsInteropConsumer",
    ["LeanRsInteropConsumer.Callback"],
)
.validate_metadata(
    "lean_rs_interop_consumer_worker_metadata",
    serde_json::json!({"source": "worker_streaming_example"}),
)
.open()?;
```

The builder uses `lean-toolchain::build_lake_target_quiet` to materialize the Lake shared library, starts and
health-checks the worker, opens the import session once, and stores the validated metadata. The caller names the Lake
project, package, target, and imports because those are capability identity; it does not construct `.lake/build/lib`
paths, locate the worker child by hand, or repeat startup ordering. The default resolver checks `LEAN_RS_WORKER_CHILD`,
sibling Cargo profile paths, and the in-tree workspace development build. Packaged applications can set
`LEAN_RS_WORKER_CHILD` or call `worker_executable` when the child binary is shipped outside those defaults.

Then it runs the export through a worker session:

```rust
let mut session = capability.open_session(None, None)?;
let command = LeanWorkerStreamingCommand::<ScanRequest, ScanRow, ScanSummary>::new(
    "lean_rs_interop_consumer_worker_data_stream",
);
let summary = session.run_streaming_command(
    &command,
    &ScanRequest {
        source: "worker_streaming_example".to_owned(),
    },
    &Rows,
    None,
    None,
    None,
)?;
assert_eq!(summary.total_rows, 2);
assert_eq!(summary.per_stream_counts["rows"], 2);
assert_eq!(summary.metadata.unwrap().ok, true);
```

Rows are live: the worker forwards each row while Lean produces it. They remain tentative until terminal success. If a
caller needs atomic commit, it should buffer rows in its sink and commit them only after `run_streaming_command` returns
`Ok`. The terminal summary reports total rows, per-stream counts, elapsed time, and optional downstream-defined metadata
decoded into the caller's summary type.

`LeanWorkerSession::run_data_stream` remains available for low-level fixtures and schema-less callers. It returns raw
`LeanWorkerDataRow` values with `serde_json::Value` payloads. Production downstream code should prefer typed commands so
row and summary decode errors are reported with command, stream, and sequence context.

The typed path is also the high-throughput path. Internally the worker keeps row payloads as validated raw JSON until it
decodes them into the caller's row type. The raw `LeanWorkerDataRow` escape hatch still parses payloads into
`serde_json::Value`, which is convenient for ad hoc inspection but costs more on large streams.

Request timeout is configured on `LeanWorkerConfig` or changed on a live worker or session:

```rust
let config = LeanWorkerConfig::new(worker_child)
    .request_timeout(LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING);
```

Startup timeout only covers the child handshake. Request timeout covers one request after it has been sent, including
live rows, diagnostics, progress, and terminal response. If it expires, the supervisor kills and replaces the child,
returns `LeanWorkerError::Timeout`, records `LeanWorkerRestartReason::RequestTimeout`, and invalidates the open session.

## Failure And Lifecycle Rules

Malformed row JSON, missing `stream`, and missing `payload` are typed worker errors. Nonzero downstream status bytes are
typed worker errors. Lean internal panics and aborts kill the child, not the parent; the supervisor reports the fatal
child exit.

Progress and diagnostics use typed worker messages. They do not share the row schema. A streaming request may take a
`LeanWorkerDiagnosticSink` for diagnostic events; data rows remain downstream data. Use cancellation when the caller
knows it wants to stop and can wait for a row or progress boundary. Use request timeout as a parent-enforced watchdog
for unresponsive Lean work; it may kill the child without cooperative cleanup.

Capability discovery uses metadata and doctor exports, not row streams. The worker's own protocol facts come from
`LeanWorker::runtime_metadata`. A downstream capability can also expose JSON-returning exports with ABI
`String -> IO String`; `LeanWorkerSession::capability_metadata` decodes command names, capability names, semantic
versions, optional Lean version text, and extra JSON, while `LeanWorkerSession::capability_doctor` decodes pass,
warning, and error diagnostics. The worker validates the generic envelope, but the downstream crate decides which
versions affect cache keys.

`LeanWorkerCapabilityBuilder::validate_metadata` runs the metadata export during setup when the caller wants metadata as
a startup contract. Doctor checks remain an explicit request because they may be slower or environment-specific.

Cycling the worker is the memory-reset boundary. `SessionPool::drain()` remains an in-process cache operation; it is not
an RSS reset.

## lean-dup Readiness

For a downstream tool such as `lean-dup`, this replaces ad hoc runtime Lean subprocess management:

- Lake build stays build-time and uses `lean-toolchain`.
- Worker startup replaces hand-written `Command` setup for runtime Lean work.
- JSONL-like rows are projected from `LeanWorkerDataRow` by the downstream tool; the worker crates do not define
  `lean-dup` business objects.
- Progress and diagnostics use typed worker channels, not stdout conventions.
- Metadata and doctor checks report cache/support facts without baking `lean-dup` command semantics into the worker
  crates.
- Fatal exits become typed worker failures that the parent can classify.
- Cancellation and timeout policy are caller decisions layered over worker requests.

The result is a process boundary with structured rows, not a `lean-dup` protocol embedded in the worker crates.

## Operational Fixture

For a larger worker-capability check, run:

```sh
cargo run --release -p lean-rs-worker --example worker_capability_probe
```

The probe uses fixture exports with command-like names `version`, `doctor`, `extract`, `features`, `index`, and `probe`.
These names exercise the same operational shape as a downstream semantic worker, but the schemas are generic: rows
contain small declaration, feature, and probe-like JSON objects owned by the fixture, not `lean-dup` business types.

The probe records:

- cold builder startup;
- first import/session opening;
- import-once streaming row throughput;
- cancellation latency at a row boundary;
- fatal child-exit recovery;
- explicit worker cycling;
- parent and child RSS samples.

Use it as an envelope check for the worker capability layer. It is not a `lean-dup` parity benchmark. If you want a
local comparison against an existing subprocess worker, pass the command explicitly:

```sh
LEAN_RS_WORKER_COMPARE_COMMAND='cargo run -p lean-dup -- --help' \
  cargo run --release -p lean-rs-worker --example worker_capability_probe
```

Record the exact command, revisions, and output limits with any comparison. The comparison command is outside the The
worker crates contract.
