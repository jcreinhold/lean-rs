# Worker Data-Plane Format

Prompt 82 measured whether `lean-rs-worker` should replace the current JSON
and raw-JSON row data plane for large worker streams. The decision for this
release is no protocol or public API change.

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

Throughput on the small-row workload:

```text
serde_json_value:      5.7373-5.8135 ms, 1.4091-1.4279 M rows/s
raw_json:              3.5722-3.5941 ms, 2.2793-2.2932 M rows/s
batched_raw_json_64:   3.5449-3.5619 ms, 2.2999-2.3109 M rows/s
binary_json_payload:   2.4341-2.4430 ms, 3.3533-3.3655 M rows/s
messagepack_typed:     3.0760-3.0906 ms, 2.6506-2.6632 M rows/s
cbor_typed:            5.9818-5.9955 ms, 1.3664-1.3695 M rows/s
```

Throughput on the large-row workload:

```text
serde_json_value:      1.2839-1.2900 ms, 396.91-398.80 K rows/s
raw_json:              831.45-838.79 us, 610.40-615.79 K rows/s
batched_raw_json_64:   826.58-829.55 us, 617.20-619.42 K rows/s
binary_json_payload:   566.89-568.96 us, 899.88-903.17 K rows/s
messagepack_typed:     411.70-415.39 us, 1.2326-1.2436 M rows/s
cbor_typed:            582.83-587.13 us, 872.03-878.48 K rows/s
```

The broader current worker path was also remeasured:

```text
cargo bench -p lean-rs-worker --bench row_payload -- \
  worker::row_payload::stream/typed_many_512 --quiet
typed_many_512: 249.83-265.96 ms, 1.9251-2.0494 K rows/s
```

Parent and child RSS were recorded with:

```text
cargo run --release -p lean-rs-worker --example row_perf_probe
```

The same run produced:

```text
workload=worker_data_stream_many rows=512
elapsed_ms=211.445
rows_per_second=2421.4
parent_alloc_blocks=1593
parent_alloc_bytes=158521
parent_rss_before_kib=Some(5824)
parent_rss_after_kib=Some(5952)
child_rss_before_kib=Some(345456)
child_rss_after_kib=Some(743904)

workload=worker_data_stream_large_payload rows=1
elapsed_ms=199.585
rows_per_second=5.0
parent_alloc_blocks=45
parent_alloc_bytes=40752
parent_rss_before_kib=Some(5952)
parent_rss_after_kib=Some(6112)
child_rss_before_kib=Some(743904)
child_rss_after_kib=Some(1117664)
```

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
stream. Prompt 83 should address Lean-side chunking and emission ergonomics;
future format work must then show a broader worker or pool/lease benchmark win
before changing the protocol or public API.

No `LeanBytesEvent`, object callback, raw frame API, MessagePack API, CBOR API,
or binary row API is added by this prompt.
