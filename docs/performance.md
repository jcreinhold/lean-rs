# Performance

Use the workload that exercises the changed path and compare captures on the same machine. For retained import memory,
CPU profiles, and generated baseline reports, use the [profiling guide](https://github.com/jcreinhold/lean-rs/blob/main/profiling/README.md). For deployment budgets,
use [production hosting](production-hosting.md).

## Criterion benchmarks

```sh
cargo bench -p lean-rs --bench hot_paths
cargo bench -p lean-rs-host --bench session
cargo bench -p lean-rs-worker-child --bench row_payload
cargo bench -p lean-rs-worker-child --bench worker_capability
```

| Bench | Paths measured |
| --- | --- |
| `hot_paths` | Cached typed-export dispatch and string/array conversion. |
| `session` | Bulk declarations, progress callbacks, bulk-versus-loop introspection, elaboration, meta dispatch, cached module-query decoding, and pool hits. |
| `row_payload` | JSON representations, typed row decoding, allocations, batching candidates, and data-plane formats. |
| `worker_capability` | Startup, import, streaming, shutdown, cancellation, fatal-exit recovery, cycling, and scale fixtures. |

Useful filters for changes to the safe ABI boundary:

| Path | Filter |
| --- | --- |
| Cached export floor | `module::scalar_dispatch_u32_add` |
| String decode | `abi::string_roundtrip` |
| Array string decode | `abi::array_string_roundtrip` |
| Host dispatch | `host::session::query_declarations_bulk` |
| Host object decoding | `host::process::module_query_batch_cached_decode` |
| Progress delivery | `host::session::query_declarations_bulk_progress` |

Save a baseline before editing, then compare it after the change:

```sh
cargo bench -p lean-rs-host --bench session -- --save-baseline before
# Make the change, then run on the same machine:
cargo bench -p lean-rs-host --bench session -- --baseline before
```

Investigate workload-specific changes outside Criterion's confidence interval. A uniform shift across unrelated paths
can reflect CPU state; recapture before attributing it to code.

Progress changes must also measure the no-progress path:

```sh
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --save-baseline before
# After the change:
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --baseline before
```

With `progress = None`, bulk introspection allocates no callback handle and uses the ordinary bulk shim. A retained
macOS/Lean 4.29.1 capture measured 100.0 µs before and 101.1 µs after, with Criterion reporting no change. This is
historical local evidence, not a portable latency target.

## Worker transport

Measure row changes on both representation and real worker paths:

```sh
cargo bench -p lean-rs-worker-child --bench row_payload -- --save-baseline before
# After the change:
cargo bench -p lean-rs-worker-child --bench row_payload -- --baseline before
cargo bench -p lean-rs-worker-child --bench worker_capability -- --sample-size 10
```

`worker::row_payload::protocol_batching` and `typed_many_512` found no improvement that justified replacing per-row
worker delivery. The `data_plane` workloads compare JSON, validated raw JSON, simulated binary envelopes, MessagePack,
and CBOR. A microbenchmark alone does not justify a new wire format or public batch sink: demonstrate an end-to-end
worker win. The decisions are recorded under
[row batching](architecture/worker-streaming.md#per-row-framing-evidence) and
[data-plane format](architecture/worker-streaming.md#data-plane-format-evidence).

The Lean-side envelope/chunk helpers use the typed-command tests:

```sh
cargo nextest run -p lean-rs-worker-child --test typed_command -E 'test(helper_)'
```

The chunked helper reports `helper_chunked_stream rows=<n> chunks=<n> chunk_size=<n> parallelism=1 elapsed_ms=<n>`.
Lean-side emission stays sequential; worker pools supply supported parallelism.

## Worker operating probes

```sh
cargo run --release -p lean-rs-worker-child --example worker_capability_probe
cargo run --release -p lean-rs-worker-child --example row_perf_probe
cargo run --release -p lean-rs-worker-child --example proof_candidate_probe
cargo run -p lean-rs-worker-child --example mathlib_scale_probe
```

These cover startup/import cost, warm dispatch, row throughput, proof-candidate batch outcomes, shutdown/cancellation,
fatal-exit recovery, cycling, pool reuse, RSS availability, and slow-sink backpressure. Compare before/after on the same
machine; report the named workload, row counts, throughput, relevant allocation/payload evidence, and parent/child RSS
or explicit unavailable status.

The scale fixture exercises builder → pool → session lease → typed command → live rows → terminal summary → pool stats
over a fixed list of 16 mathlib-style module names. Its rows are deterministic fixture data: the probe does not measure
mathlib elaboration. A corresponding Criterion filter is:

```sh
cargo bench -p lean-rs-worker-child --bench worker_capability -- mathlib_scale
```

`worker_capability_probe` drives generic `version`, `doctor`, `extract`, `features`, `index`, and `probe` commands. Its
schemas are fixture schemas. See [shape commands](testing.md#worker-shape-commands) and the
[scale contract](architecture/worker-pool.md#validation-evidence) for coverage limits.

Rows reach sinks before terminal success and remain tentative until the terminal summary. Pool snapshots expose request
outcomes, delivered rows, payload bytes, timing, and backpressure counters. Admission is synchronous, so the stable
queue-depth field is currently zero. See [diagnostics](diagnostics.md) for interpreting partial streams.

## Cold paths and allocations

Measure first runtime, library, and module startup in separate processes. Repeated library opens and initializer calls
can reuse warmed loader state or idempotent initialization, so a Criterion loop would measure a different path:

```sh
cargo build --release -p lean-rs --example cold_probe
for i in $(seq 1 25); do ./target/release/examples/cold_probe; done
```

Each stage prints `name=<workload> elapsed_us=<u64>`.

For Rust allocation attribution, run the dhat probe from a separate output directory:

```sh
WORKSPACE=$(pwd)
cargo build --release -p lean-rs --example alloc_probe
mkdir -p /tmp/dhat-runs
cd /tmp/dhat-runs
"$WORKSPACE/target/release/examples/alloc_probe" string_identity_4096
```

Inspect `dhat-heap.json` with [dh_view](https://nnethercote.github.io/dh_view/dh_view.html). It records Rust-side
conversion buffers and service allocations. Lean's internal allocator is invisible to Rust's global allocator; use the
[import-memory probes](https://github.com/jcreinhold/lean-rs/blob/main/profiling/README.md) for Lean retention and RSS.
