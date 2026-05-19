# Performance

Benchmarks and the regression-detection recipe. Numbers themselves are not tracked here;
they are non-portable across machines and ship with each capture.

## Run the benches

```sh
cargo bench -p lean-rs --bench hot_paths
cargo bench -p lean-rs-host --bench session
```

`hot_paths` covers `lean_rs::module` and `lean_rs::abi` — `LeanExported::call` and the
`String`/`Vec<String>` round-trip decoders. `session` covers `LeanSession::*` —
`query_declarations_bulk`, `elaborate_small`, `run_meta_whnf`, and `SessionPool` hits.

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
cargo build --release -p lean-rs --features dhat-heap --example alloc_probe
mkdir -p /tmp/dhat-runs && cd /tmp/dhat-runs
"$WORKSPACE/target/release/examples/alloc_probe" string_identity_4096
# dhat-heap.json carries per-call-site backtraces; inspect with
# https://nnethercote.github.io/dh_view/dh_view.html
```

dhat sees Rust allocations only. Lean's internal mimalloc is statically linked into
`libleanrt.a` and is invisible to `#[global_allocator]`; allocation numbers capture host-stack
churn (ABI conversions, `Vec`/`String` buffers, error-message bounding), not kernel-side heap.
