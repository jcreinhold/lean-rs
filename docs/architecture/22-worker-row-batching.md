# Worker Row Batching

Prompt 81 tested whether `lean-rs-worker` should batch row frames for
mathlib-scale streams. The answer for this release is no: batching stays a
measured non-change until a workload shows it improves the full worker path.

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

The named workload was the existing 512-row interop streaming export
`lean_rs_interop_consumer_worker_data_stream_many`.

Before the private batch-frame experiment:

```text
cargo bench -p lean-rs-worker --bench row_payload -- \
  worker::row_payload::stream/typed_many_512 --quiet
time: 275.81 ms to 303.74 ms
throughput: 1.69 K rows/s to 1.86 K rows/s

cargo test -p lean-rs-worker --test streaming_runner \
  large_stream_records_live_forwarding_throughput_and_rss -- --nocapture
large_stream rows=512 elapsed_ms=142 rows_per_sec=3590.5
rss_before_kib=Some(48000) rss_after_kib=Some(346848)
```

The private batch-frame prototype used bounded groups of up to 64 rows and
kept the public API unchanged. It did not improve the broader worker path:

```text
time: 296.70 ms to 339.81 ms
throughput: 1.51 K rows/s to 1.73 K rows/s
debug large-stream probe: 407 ms, 1257 rows/s
```

After removing the prototype, the typed stream recovered:

```text
time: 261.74 ms to 270.41 ms
throughput: 1.89 K rows/s to 1.96 K rows/s
large_stream rows=512 elapsed_ms=155 rows_per_sec=3298.9
rss_before_kib=Some(48112) rss_after_kib=Some(347024)
```

The microbenchmark added by prompt 81 also compares per-row raw JSON framing
with simulated batch sizes:

```text
cargo bench -p lean-rs-worker --bench row_payload -- \
  worker::row_payload::protocol_batching --quiet
per_row_raw_value: 829.78 us to 838.39 us
batch_16_raw_value: 900.46 us to 945.11 us
batch_64_raw_value: 839.94 us to 849.64 us
```

The micro result does not justify adding a public batch sink, and the broader
worker result rejects private batch frames for now.

## Decision

Do not add `LeanWorkerDataBatch`, a typed batch sink, or private `DataRowBatch`
frames in this release. The existing live per-row protocol keeps stronger
failure behavior: rows are forwarded as soon as Lean emits them, fatal exits
after a row can still be reported after the parent observes that row, and
sink-driven cancellation remains row-boundary precise.

Prompt 82 may still benchmark a different data-plane format. Any future batch
work must show both a microbenchmark win and a broader pool/lease scenario win
before adding public surface area.
