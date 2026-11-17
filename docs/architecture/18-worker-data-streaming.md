# Worker Data Streaming

Worker streams carry downstream-owned JSON rows while a request runs. The worker owns framing, per-stream ordering,
backpressure, cancellation, diagnostics, and terminal completion. Downstream code owns stream names, row schemas,
validation, persistence, and interpretation.

Use `LeanWorkerStreamingCommand<Req, Row, Summary>` for typed commands. The schema-less escape hatch is
`run_data_stream`, whose public row type is:

```rust
pub struct LeanWorkerDataRow {
    pub stream: String,
    pub sequence: u64,
    pub payload: serde_json::Value,
}
```

Sequences are assigned per stream within one request. Protocol frames carry validated raw JSON; typed commands decode it
directly into `Row`, avoiding an intermediate `serde_json::Value`. Only the schema-less API constructs that tree.
Callback handles and runtime-bound Lean objects remain inside the child.

## Completion and backpressure

Rows reach the sink live and remain tentative until the request returns terminal success. `LeanWorkerStreamSummary`
records total and per-stream row counts, elapsed time, and optional downstream terminal metadata. Discard partial output
when atomic commit is required and the request fails, times out, is cancelled, or loses its child.

Diagnostics and progress use separate sinks. Sink panics are contained by the worker API. A bounded reader event buffer
propagates slow-sink pressure through the child pipe; committed streams never drop rows. The parent request watchdog
also covers silent streams. See [pool observability](20-worker-pool.md#observability-and-backpressure) for counters and
[the capability recipe](../recipes/worker-capability-runner.md) for normal use.

Raw stdout JSONL would expose framing, EOF, crash, and completion rules to each consumer. Cross-process callback
handles would violate the callback registry's process-local lifetimes. Worker rows hide both sets of mechanics.

## Lean envelope helpers

`LeanRsInterop.Worker.Stream` ships with `lean-rs-interop-shims`. It provides:

| Helper | Purpose |
| --- | --- |
| `jsonString` | Escape a Lean string as JSON |
| `row` | Wrap a stream name and caller-supplied payload JSON |
| `diagnostic` | Build a diagnostic envelope |
| `progress`, `chunkProgress` | Build progress envelopes |
| `metadata` | Build a terminal metadata envelope |
| `emitAll` | Send an envelope array through the child-local string callback |
| `countChunks` | Count chunks for a downstream-owned plan |

Capability code builds its envelope array locally and calls `emitAll` once. Request parsing, payload JSON validity,
command algorithms, and chunk scheduling remain downstream responsibilities. Pool selection, leases, cancellation,
and timeouts remain Rust worker responsibilities.

The helper surface records a specific shared-library constraint: generic chunk loops, source macros expanding those
loops, and single-envelope helpers allocating one-element arrays caused child SIGSEGVs in the Lean 4.29.1 worker path.
The retained fixture pattern factors out envelope construction and `emitAll`, while keeping chunk scheduling in the
capability. That historical failure is a reason to recheck this path before adding convenience runners.

## Per-row framing evidence

The protocol keeps per-row frames. A private 64-row batch prototype did not improve the 512-row export
`lean_rs_interop_consumer_worker_data_stream_many`:

| Variant | `stream/typed_many_512` | Bench throughput | Large-stream probe |
| --- | --- | --- | --- |
| Per-row baseline | 275.81–303.74 ms | 1.69–1.86 K rows/s | 3590.5 rows/s |
| Private 64-row batches | 296.70–339.81 ms | 1.51–1.73 K rows/s | 1257.4 rows/s |
| Per-row after prototype removal | 261.74–270.41 ms | 1.89–1.96 K rows/s | 3298.9 rows/s |

The protocol-only `worker::row_payload::protocol_batching` comparison also failed to justify batching:

| Frame shape | Time |
| --- | --- |
| Per-row raw JSON | 829.78–838.39 µs |
| Batched 16 raw JSON | 900.46–945.11 µs |
| Batched 64 raw JSON | 839.94–849.64 µs |

Reproduce the current baseline with:

```sh
cargo bench -p lean-rs-worker-child --bench row_payload -- \
  worker::row_payload::stream/typed_many_512 --quiet
cargo bench -p lean-rs-worker-child --bench row_payload -- \
  worker::row_payload::protocol_batching --quiet
cargo nextest run -p lean-rs-worker-child --test streaming_runner \
  large_stream_records_live_forwarding_throughput_and_rss --no-capture
```

The removed prototype is a recorded comparison, not a current benchmark mode. No public batch sink or private
`DataRowBatch` frame is implemented. A future batch design needs both a protocol benchmark win and a broader worker or
pool/lease win while preserving live delivery, fatal-exit reporting, and row-boundary cancellation.

## Data-plane format evidence

Encoding candidates were compared on `small_rows_8192` (small declaration-like rows) and `large_rows_512` (4 KiB payload
rows). Representative frame sizes in bytes:

| Shape | JSON tree | Raw JSON | Binary envelope with JSON payload | MessagePack | CBOR |
| --- | ---: | ---: | ---: | ---: | ---: |
| `small_rows_8192` | 97 | 97 | 76 | 40 | 76 |
| `large_rows_512` | 4154 | 4154 | 4133 | 4108 | 4139 |

Recorded median throughput, estimated from each reported range:

| Format | Small rows | Large rows |
| --- | ---: | ---: |
| `binary_json_payload` | 3.36 M rows/s | 0.90 M rows/s |
| `messagepack_typed` | 2.66 M rows/s | 1.24 M rows/s |
| `batched_raw_json_64` | 2.30 M rows/s | 0.62 M rows/s |
| `raw_json` (current) | 2.29 M rows/s | 0.61 M rows/s |
| `serde_json::Value` | 1.42 M rows/s | 0.40 M rows/s |
| `cbor_typed` | 1.37 M rows/s | 0.87 M rows/s |

The end-to-end worker capture was much slower: about 2 K rows/s for `typed_many_512`; `row_perf_probe` reported about
2.4 K rows/s, parent RSS about 6 MiB, and child RSS growth about 400 MiB for the 512-row stream. Its one-row large-payload
case took about 200 ms with about 370 MiB child RSS growth.

```sh
cargo bench -p lean-rs-worker-child --bench row_payload -- \
  worker::row_payload::data_plane --quiet
cargo run --release -p lean-rs-worker-child --example row_perf_probe
```

These captures did not record enough platform/revision metadata to serve as portable baselines. Re-capture on the same
hardware before claiming a regression. The encoding benchmarks suggest candidates but do not identify the end-to-end
bottleneck: neither the binary envelope nor MessagePack has a measured Lean-to-parent replacement path. Keep raw JSON
until a full worker workload justifies a protocol change. No binary, MessagePack, or CBOR row API is exposed. Peer and
harness authors can use the codec in `lean-rs-worker-protocol`; parent-facing callers use typed commands and row sinks.
