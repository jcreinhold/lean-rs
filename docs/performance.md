# Performance

Benchmarks and the regression-detection recipe. Numbers themselves are not tracked here; they are non-portable across
machines and ship with each capture.

## Run the benches

```sh
cargo bench -p lean-rs --bench hot_paths
cargo bench -p lean-rs-host --bench session
cargo bench -p lean-rs-worker-child --bench row_payload
cargo bench -p lean-rs-worker-child --bench worker_capability
```

`hot_paths` covers `lean_rs::module` and `lean_rs::abi`: `LeanExported::call` and the `String`/`Vec<String>` round-trip
decoders. `session` covers `LeanSession::*`: `query_declarations_bulk`, the three `declaration_*_bulk` 5k-vs-loop
comparisons, `elaborate_small`, `run_meta_whnf`, and `SessionPool` hits.

`row_payload` covers the worker row transport hot path: JSON tree rows versus validated raw-JSON rows, typed command
decode, row throughput, and allocation pressure. Two guards live alongside it: `worker::row_payload::protocol_batching`
compares per-row raw JSON frames with batched raw JSON frames so any future batch protocol must justify itself against
the current per-row baseline. `worker::row_payload::data_plane` compares JSON, raw JSON, simulated binary envelopes,
MessagePack, and CBOR so any future format change must show a measured win. `worker_capability` covers the
downstream-shaped worker fixture: cold startup, first import, import-once streaming, cancellation latency, fatal-exit
recovery, worker cycling, row throughput, and memory growth.

Progress changes must benchmark the no-progress path explicitly. The retained fast path for bulk introspection is:

```sh
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --save-baseline before
# ... make progress changes ...
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --baseline before
```

`progress = None` should stay within Criterion noise because it allocates no callback handle and dispatches the same
bulk shim as before.

Worker row-performance changes must benchmark the worker row path explicitly:

```sh
cargo bench -p lean-rs-worker-child --bench row_payload -- --save-baseline before
# ... make row transport changes ...
cargo bench -p lean-rs-worker-child --bench row_payload -- --baseline before
```

Do not add public worker batch sinks or private row-batch protocol frames from a microbenchmark alone. The
`protocol_batching` and `typed_many_512` measurements showed no improvement over per-row delivery on the real worker
path; row delivery stays per-row. See
[`docs/architecture/22-worker-row-batching.md`](architecture/22-worker-row-batching.md). Do not replace the worker
data-plane format from a microbenchmark alone either. Small- and large-row format candidates were measured; the raw-JSON
typed decode path stays until an end-to-end worker workload justifies a replacement. See
[`docs/architecture/23-worker-data-plane-format.md`](architecture/23-worker-data-plane-format.md).

The Lean-side worker envelope helper fixture runs through the typed command path:

```sh
cargo test -p lean-rs-worker-child --test typed_command helper_ -- --nocapture
```

The focused chunked-stream test prints the named helper workload as `helper_chunked_stream rows=<n> chunks=<n>
chunk_size=<n> parallelism=1 elapsed_ms=<n>`. Keep `parallelism=1`; Rust worker pools are the supported parallelism
boundary. A safe Lean-side parallel chunk emitter would have to be proved out before raising this.

Capability-layer changes should also run the downstream-shaped scenario bench:

```sh
cargo bench -p lean-rs-worker-child --bench worker_capability -- --sample-size 10
```

Record parent/child RSS alongside throughput with:

```sh
cargo run --release -p lean-rs-worker-child --example worker_capability_probe
cargo run --release -p lean-rs-worker-child --example row_perf_probe
```

The mathlib-scale fixture runs through the planner, pool, session lease, and typed command path:

```sh
cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
cargo run -p lean-rs-worker-child --example mathlib_scale_probe
cargo bench -p lean-rs-worker-child --bench worker_capability -- mathlib_scale
```

Set `LEAN_RS_MATHLIB_ROOT=/path/to/mathlib4` and `LEAN_RS_MATHLIB_SCALE_LIMIT=<n>` to use a real mathlib module list as
the planning workload. The fixture emits deterministic generic rows, so only claim mathlib-scale behavior for the parts
actually measured: planning, pool leases, session reuse, row throughput, cancellation, fatal-exit recovery, worker
cycling, and RSS sampling availability.

The same probe records pool snapshots and bounded row-delivery backpressure. The `single_worker`, `pool_max_2`, and
`post_cycle` lines print active workers, warm leases, queue depth, stream request outcomes, delivered rows, payload
bytes, stream elapsed time, and backpressure counters. The `slow_sink` line runs a deliberately slow row sink and
records parent RSS before/after, child RSS, delivered row count, payload bytes, and backpressure waits/failures:

```sh
cargo build -p lean-rs-worker-child --bin lean-rs-worker-child
cargo run -p lean-rs-worker-child --example mathlib_scale_probe
```

Rows are not dropped under backpressure. A delivered row is still tentative until terminal success. Use the snapshot
counters as operating evidence and terminal summaries as committed row counts.

The `lean-dup`-class readiness fixture exercises the full path end to end:

```sh
cargo run -p lean-rs-worker-child --example lean_dup_readiness
```

It drives the planner, pool, session lease, and typed command facade for generic `version`, `doctor`, `extract`,
`features`, `index`, and `probe` command shapes, and records row throughput, diagnostics, progress, terminal summaries,
timeout/cancellation/fatal-exit recovery, explicit cycling, backpressure, pool stats, parent/child RSS when available,
and optional subprocess comparison status. Treat its rows as generic fixture data, not downstream schemas.

The supported scale path is planner → pool → session lease → typed command → live rows → terminal summary → pool stats.
Any throughput change must report a named workload, row counts, throughput, allocation or payload-size evidence where
relevant, and parent/child RSS (or explicit RSS-unavailable status). The standing scale contract is
[`docs/architecture/28-production-scale-release.md`](architecture/28-production-scale-release.md).

Sample capture (macOS, Lean 4.29.1, `declaration_kind_bulk_vs_loop/bulk_5000`): baseline 100.0 µs vs. comparison 101.1
µs; Criterion reports no change. Re-capture on the same hardware before declaring a regression.

The cold-path probes (`runtime_init`, `library_open`, `module_initialize`) are not Criterion benches because they only
fire once per process. Run them via:

```sh
cargo build --release -p lean-rs --example cold_probe
for i in $(seq 1 25); do ./target/release/examples/cold_probe; done
```

Output is one `name=<workload> elapsed_us=<u64>` line per stage.

## Long-session RSS reproducer

`long_session_memory` is the retained-memory counterpart to the latency benches. It runs one long-lived process through
fresh imports, pooled reuse, bulk introspection, and elaboration, printing RSS checkpoints and `SessionPool` counters:

```sh
LEAN_RS_NUM_THREADS=1 cargo run --release -p lean-rs-host --example long_session_memory
```

This is deliberately not a Criterion bench. Criterion answers per-iteration latency questions; this workload answers
whether RSS returns at lifetime boundaries after `LeanSession`, `SessionPool`, and `Obj<'lean>` drops. See
[`docs/safety/long-session-memory.md`](safety/long-session-memory.md) for the measured `LEAN_RESOLVED_VERSION` result
and consumer guidance.

## Detect a regression

Save a baseline before any change you suspect of moving the numbers:

```sh
cargo bench -p lean-rs-host --bench session -- --save-baseline before
# ... make changes ...
cargo bench -p lean-rs-host --bench session -- --baseline before
```

Criterion's report calls out per-workload deltas with confidence intervals. Treat any change outside the 95% CI on a
workload that exercises the changed code path as load-bearing; treat uniform shifts across unrelated workloads as
CPU-state noise between captures.

## Allocation snapshots

dhat output for the conversion-heavy workloads, when an allocation question needs an answer:

```sh
# Run from the workspace root.
WORKSPACE=$(pwd)
cargo build --release -p lean-rs --example alloc_probe
mkdir -p /tmp/dhat-runs && cd /tmp/dhat-runs
"$WORKSPACE/target/release/examples/alloc_probe" string_identity_4096
# dhat-heap.json carries per-call-site backtraces; inspect with
# https://nnethercote.github.io/dh_view/dh_view.html
```

dhat sees Rust allocations only. Lean's internal mimalloc is statically linked into `libleanrt.a` and is invisible to
`#[global_allocator]`; allocation numbers capture ABI conversions, `Vec`/`String` buffers, and error-message bounding on
the Rust service layer, not the Lean kernel heap.
