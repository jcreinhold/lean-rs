# Performance

Benchmarks and the regression-detection recipe. Numbers themselves are not tracked here;
they are non-portable across machines and ship with each capture.

## Run the benches

```sh
cargo bench -p lean-rs --bench hot_paths
cargo bench -p lean-rs-host --bench session
cargo bench -p lean-rs-worker --bench row_payload
cargo bench -p lean-rs-worker --bench worker_capability
```

`hot_paths` covers `lean_rs::module` and `lean_rs::abi`: `LeanExported::call`
and the `String`/`Vec<String>` round-trip decoders. `session` covers
`LeanSession::*`: `query_declarations_bulk`, the three `declaration_*_bulk`
5k-vs-loop comparisons, `elaborate_small`, `run_meta_whnf`, and
`SessionPool` hits.

`row_payload` covers the worker row transport hot path: JSON tree rows versus
validated raw-JSON rows, typed command decode, row throughput, and allocation
pressure. It also contains `worker::row_payload::protocol_batching`, the
prompt-81 guard that compares simulated per-row raw JSON frames with batched
raw JSON frames before any batch protocol is added, and
`worker::row_payload::data_plane`, the prompt-82 guard comparing JSON,
raw JSON, simulated binary envelopes, MessagePack, and CBOR before any worker
format is changed. `worker_capability` covers
the downstream-shaped worker fixture:
cold startup, first import, import-once streaming, cancellation latency,
fatal-exit recovery, worker cycling, row throughput, and memory growth.

Progress changes must benchmark the no-progress path explicitly. The retained
fast path for bulk introspection is:

```sh
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --save-baseline before
# ... make progress changes ...
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --baseline before
```

`progress = None` should stay within Criterion noise because it allocates no
callback handle and dispatches the same bulk shim as before.

Worker row-performance changes must benchmark the worker row path explicitly:

```sh
cargo bench -p lean-rs-worker --bench row_payload -- --save-baseline before
# ... make row transport changes ...
cargo bench -p lean-rs-worker --bench row_payload -- --baseline before
```

Do not add public worker batch sinks or private row-batch protocol frames from
a microbenchmark alone. Prompt 81 measured `protocol_batching` and the broader
`typed_many_512` worker stream; batching did not improve the real worker path,
so row delivery remains per-row for this release. See
[`docs/architecture/22-worker-row-batching.md`](architecture/22-worker-row-batching.md).
Do not replace the worker data-plane format from a microbenchmark alone either.
Prompt 82 measured small-row and large-row format candidates and kept the
current raw-JSON typed decode path until an end-to-end worker workload proves a
protocol replacement. See
[`docs/architecture/23-worker-data-plane-format.md`](architecture/23-worker-data-plane-format.md).

Capability-layer changes should also run the downstream-shaped scenario bench:

```sh
cargo bench -p lean-rs-worker --bench worker_capability -- --sample-size 10
```

Record parent/child RSS alongside throughput with:

```sh
cargo run --release -p lean-rs-worker --example worker_capability_probe
cargo run --release -p lean-rs-worker --example row_perf_probe
```

Prompt 47 release-hardening capture on macOS / Lean 4.29.1:

```text
cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --save-baseline prompt47-before
baseline time: [99.764 µs 100.01 µs 100.36 µs]

cargo bench -p lean-rs-host --bench session -- \
  host::session::declaration_kind_bulk_vs_loop/bulk_5000 --baseline prompt47-before
comparison time: [100.58 µs 101.11 µs 101.52 µs]
Criterion: No change in performance detected.
```

The cold-path probes (`runtime_init`, `library_open`, `module_initialize`) are not Criterion
benches because they only fire once per process. Run them via:

```sh
cargo build --release -p lean-rs --example cold_probe
for i in $(seq 1 25); do ./target/release/examples/cold_probe; done
```

Output is one `name=<workload> elapsed_us=<u64>` line per stage.

## Long-session RSS reproducer

`long_session_memory` is the retained-memory counterpart to the latency benches. It runs one
long-lived process through fresh imports, pooled reuse, bulk introspection, and elaboration,
printing RSS checkpoints and `SessionPool` counters:

```sh
LEAN_RS_NUM_THREADS=1 cargo run --release -p lean-rs-host --example long_session_memory
```

This is deliberately not a Criterion bench. Criterion answers per-iteration latency questions;
this workload answers whether RSS returns at lifetime boundaries after `LeanSession`,
`SessionPool`, and `Obj<'lean>` drops. See
[`docs/safety/long-session-memory.md`](safety/long-session-memory.md) for the measured
`LEAN_RESOLVED_VERSION` result and consumer guidance.

## Detect a regression

Save a baseline before any change you suspect of moving the numbers:

```sh
cargo bench -p lean-rs-host --bench session -- --save-baseline before
# ... make changes ...
cargo bench -p lean-rs-host --bench session -- --baseline before
```

Criterion's report calls out per-workload deltas with confidence intervals. Treat any change
outside the 95% CI on a workload that exercises the changed code path as load-bearing; treat
uniform shifts across unrelated workloads as CPU-state noise between captures.

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

dhat sees Rust allocations only. Lean's internal mimalloc is statically
linked into `libleanrt.a` and is invisible to `#[global_allocator]`;
allocation numbers capture ABI conversions, `Vec`/`String` buffers, and
error-message bounding on the host stack, not the Lean kernel heap.
