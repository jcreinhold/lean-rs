# Worker Process Boundary

Run the worker streaming example from a clean checkout:

```sh
cargo run -p lean-rs-worker --example worker_streaming
```

The example builds the downstream Lake target, starts a worker child, opens a
worker session, runs a streaming Lean export, prints JSONL-like rows, cycles the
worker, and proves that the next request succeeds in a fresh child.

Use this path when the application needs process isolation or memory cycling.
Use direct L1 callbacks, such as
[`string-callback-streaming.md`](string-callback-streaming.md), when the Lean
extension is trusted and same-process execution is acceptable.

## What The Example Builds

The fixture at [`fixtures/interop-shims/`](../../fixtures/interop-shims/)
depends on the generic interop shims and exports:

```lean
@[export lean_rs_interop_consumer_worker_data_stream]
def workerDataStream (requestJson : String) (handle trampoline : USize) : IO UInt8 := ...
```

The worker runner fixes the ABI:

```text
String request_json -> USize callback_handle -> USize callback_trampoline -> IO UInt8
```

`request_json` is downstream-owned request text. The child creates an
in-process `LeanCallbackHandle<LeanStringEvent>`, passes the handle and
trampoline to Lean, validates each callback string as a row envelope, and
forwards rows to the parent as `LeanWorkerDataRow` values. The callback handle
never crosses the process boundary.

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

`stream` is a caller-defined channel name. `sequence` is assigned by
`lean-rs-worker` per stream inside one request. `payload` is arbitrary JSON.
The worker validates the envelope and ordering mechanics; the downstream crate
owns the row schema.

## Rust Call Site

The caller implements a request-local sink:

```rust
struct Rows;

impl LeanWorkerDataSink for Rows {
    fn report(&self, row: LeanWorkerDataRow) {
        println!(
            "{}",
            serde_json::json!({
                "stream": row.stream,
                "sequence": row.sequence,
                "payload": row.payload,
            })
        );
    }
}
```

Then it runs the export through a worker session:

```rust
let summary = session.run_data_stream(
    "lean_rs_interop_consumer_worker_data_stream",
    &serde_json::json!({"source": "worker_streaming_example"}),
    &Rows,
    None,
    None,
    None,
)?;
assert_eq!(summary.total_rows, 2);
assert_eq!(summary.per_stream_counts["rows"], 2);
```

Rows are live: the worker forwards each row while Lean produces it. They remain
tentative until terminal success. If a caller needs atomic commit, it should
buffer rows in its sink and commit them only after `run_data_stream` returns
`Ok`. The terminal summary reports total rows, per-stream counts, elapsed time,
and optional downstream-defined metadata.

Request timeout is configured on `LeanWorkerConfig` or changed on a live worker
or session:

```rust
let config = LeanWorkerConfig::new(worker_child)
    .request_timeout(LEAN_WORKER_REQUEST_TIMEOUT_LONG_RUNNING);
```

Startup timeout only covers the child handshake. Request timeout covers one
request after it has been sent, including live rows, diagnostics, progress, and
terminal response. If it expires, the supervisor kills and replaces the child,
returns `LeanWorkerError::Timeout`, records
`LeanWorkerRestartReason::RequestTimeout`, and invalidates the open session.

## Failure And Lifecycle Rules

Malformed row JSON, missing `stream`, and missing `payload` are typed worker
errors. Nonzero downstream status bytes are typed worker errors. Lean internal
panics and aborts kill the child, not the parent; the supervisor reports the
fatal child exit.

Progress and diagnostics use typed worker messages. They do not share the row
schema. A streaming request may take a `LeanWorkerDiagnosticSink` for
diagnostic events; data rows remain downstream data. Use cancellation when the
caller knows it wants to stop and can wait for a row or progress boundary. Use
request timeout as a parent-enforced watchdog for unresponsive Lean work; it
may kill the child without cooperative cleanup.

Cycling the worker is the memory-reset boundary. `SessionPool::drain()` remains
an in-process cache operation; it is not an RSS reset.

## lean-dup Readiness

For a downstream tool such as `lean-dup`, this replaces ad hoc runtime Lean
subprocess management:

- Lake build stays build-time and uses `lean-toolchain`.
- Worker startup replaces hand-written `Command` setup for runtime Lean work.
- JSONL-like rows are projected from `LeanWorkerDataRow` by the downstream
  tool; `lean-rs-worker` does not define `lean-dup` business objects.
- Progress and diagnostics use typed worker channels, not stdout conventions.
- Fatal exits become typed worker failures that the parent can classify.
- Cancellation and timeout policy are caller decisions layered over worker
  requests.

The result is a process boundary with structured rows, not a `lean-dup`
protocol embedded in `lean-rs-worker`.
