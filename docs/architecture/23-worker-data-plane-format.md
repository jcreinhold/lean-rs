# Worker Data-Plane Format

The current JSON and raw-JSON row data plane is kept. Format candidates
(MessagePack, CBOR, a manual binary envelope) were measured against large
worker streams; none earned a protocol or public-API change.

## Boundary

The normal downstream surface remains the worker capability layer:

- callers run typed commands through capability sessions or pool leases;
- typed commands decode downstream-owned request, row, and summary types;
- `LeanWorkerDataRow` and raw rows remain lower-level escape hatches;
- callers do not learn protocol frames, child pipe bytes, child ids, or
  callback handles.

The data-plane format is an implementation detail below that surface. Changing
it is justified only by an end-to-end worker workload, not by a standalone
encoding microbenchmark.

L1 callback payload expansion is deliberately unrelated. Same-process
callbacks remain the `lean-rs` mechanism layer. Worker row throughput is a
worker IPC question and should be solved with worker framing, scheduling,
chunking, and measured encoding changes.

## Measurement

The benchmark command was:

```text
cargo bench -p lean-rs-worker --bench row_payload -- \
  worker::row_payload::data_plane --quiet
```

It compares `serde_json::Value`, `Box<serde_json::value::RawValue>`, batched
raw JSON, a manual binary envelope carrying raw JSON payload bytes,
MessagePack, and CBOR. The two named row shapes were:

- `small_rows_8192`: many small declaration-like rows;
- `large_rows_512`: fewer 4 KiB payload rows.

One representative frame size from each shape:

```text
data_plane_size shape=small_rows_8192 value_json=97 raw_json=97 binary_json_payload=76 messagepack=40 cbor=76
data_plane_size shape=large_rows_512 value_json=4154 raw_json=4154 binary_json_payload=4133 messagepack=4108 cbor=4139
```

Median throughput (mid-range estimate, sorted by small-row speed):

| Format             | small_rows_8192 | large_rows_512 |
| ------------------ | --------------: | -------------: |
| binary_json_payload| 3.36 M rows/s   | 0.90 M rows/s  |
| messagepack_typed  | 2.66 M rows/s   | 1.24 M rows/s  |
| batched_raw_json_64| 2.30 M rows/s   | 0.62 M rows/s  |
| raw_json (current) | 2.29 M rows/s   | 0.61 M rows/s  |
| serde_json::Value  | 1.42 M rows/s   | 0.40 M rows/s  |
| cbor_typed         | 1.37 M rows/s   | 0.87 M rows/s  |

End-to-end remeasurement of the current worker path:

```sh
cargo bench -p lean-rs-worker --bench row_payload -- \
  worker::row_payload::stream/typed_many_512 --quiet
# typed_many_512 ≈ 2 K rows/s

cargo run --release -p lean-rs-worker --example row_perf_probe
# worker_data_stream_many (512 rows): ~2.4 K rows/s, parent RSS ~6 MiB,
#   child RSS growth ~400 MiB
# worker_data_stream_large_payload (1 row): ~200 ms,
#   child RSS growth ~370 MiB
```

The encoding microbenchmarks dominate the end-to-end numbers by an order of
magnitude: the bottleneck is elsewhere on the worker path, not in the row
encoder. Re-capture on the same hardware before declaring a regression.

## Decision

Keep the current public worker data plane and raw-JSON typed decode path.

The microbenchmarks identify two useful future candidates, but not a single
format that should replace the worker protocol now:

- the manual binary envelope is the best small-row microbenchmark;
- MessagePack is the best large-row typed microbenchmark;
- CBOR does not win either workload;
- raw JSON remains much faster than `serde_json::Value` while preserving the
  existing typed command facade.

The winning microbenchmarks do not yet have a Lean-side, end-to-end worker
path. Implementing either a binary envelope or MessagePack now would add
protocol complexity before proving that it improves the real child process
stream. The Lean-side worker helpers in
[`24-lean-side-worker-streaming.md`](24-lean-side-worker-streaming.md) remove
envelope boilerplate while keeping the current raw-JSON row path. Future format
work must show a broader worker or pool/lease benchmark win before changing the
protocol or public API.

No `LeanBytesEvent`, object callback, raw frame API, MessagePack API, CBOR
API, or binary row API is exposed.
