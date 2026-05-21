# Worker Row Batching

Worker row frames are per-row, not batched. A measurement campaign on
mathlib-scale streams showed batching neither improves throughput nor pays for
the failure-mode complexity it adds. Batching stays out until a workload shows
it improves the full worker path.

## Boundary

The public row interface remains unchanged:

- typed downstream commands use `LeanWorkerStreamingCommand<Req, Row, Summary>`;
- schema-less callers can use `LeanWorkerDataRow`;
- raw JSON rows remain an internal fast path for typed decoding;
- no caller learns protocol frames, child pipes, worker ids, or batch
  boundaries.

This preserves the capability-layer boundary from
[`19-worker-capability-layer.md`](19-worker-capability-layer.md). Worker
throughput work must deepen the worker implementation; it should not ask
downstream tools to coordinate process or framing mechanics.

## Measurement

The workload was the 512-row interop streaming export
`lean_rs_interop_consumer_worker_data_stream_many`. Three variants of the worker, same
fixture:

| Variant | `stream/typed_many_512` time | bench throughput | large-stream probe |
| --- | --- | --- | --- |
| baseline (per-row) | 275.81–303.74 ms | 1.69–1.86 K rows/s | 3590.5 rows/s |
| with private 64-row batch frames | 296.70–339.81 ms | 1.51–1.73 K rows/s | 1257.4 rows/s |
| baseline (per-row), after removing the prototype | 261.74–270.41 ms | 1.89–1.96 K rows/s | 3298.9 rows/s |

The protocol-batching microbenchmark
(`worker::row_payload::protocol_batching`) compares per-row raw JSON framing against
simulated batch sizes at the protocol layer alone:

| Frame shape | Time |
| --- | --- |
| per-row raw JSON | 829.78–838.39 µs |
| batched 16 raw JSON | 900.46–945.11 µs |
| batched 64 raw JSON | 839.94–849.64 µs |

The micro result does not justify adding a public batch sink, and the broader worker result
rejects private batch frames.

Reproduce with:

```sh
cargo bench -p lean-rs-worker --bench row_payload -- \
  worker::row_payload::stream/typed_many_512 --quiet
cargo bench -p lean-rs-worker --bench row_payload -- \
  worker::row_payload::protocol_batching --quiet
cargo test -p lean-rs-worker --test streaming_runner \
  large_stream_records_live_forwarding_throughput_and_rss -- --nocapture
```

## Decision

Do not add `LeanWorkerDataBatch`, a typed batch sink, or private `DataRowBatch`
frames in this release. The existing live per-row protocol keeps stronger
failure behavior: rows are forwarded as soon as Lean emits them, fatal exits
after a row can still be reported after the parent observes that row, and
sink-driven cancellation remains row-boundary precise.

A future data-plane format change may still be benchmarked separately. Any future batch
work must show both a microbenchmark win and a broader pool/lease scenario win
before adding public surface area.
