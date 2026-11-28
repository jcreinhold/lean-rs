# Performance

Use the workload that exercises the changed path and compare captures on the same machine. For retained import memory,
CPU profiles, and generated baseline reports, use the
[profiling guide](https://github.com/jcreinhold/lean-rs/blob/main/profiling/README.md). For deployment budgets, use
[production hosting](production-hosting.md).

## Criterion benchmarks

```sh
cargo bench -p lean-rs --bench hot_paths
cargo bench -p lean-rs-host --bench session
```

| Bench | Paths measured |
| --- | --- |
| `hot_paths` | Cached typed-export dispatch and string/array conversion. |
| `session` | Bulk declarations, progress callbacks, bulk-versus-loop introspection, elaboration, meta dispatch, and cached module-query decoding. |

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

## Worker operating probes

```sh
cargo run --release -p lean-rs-worker-child --example proof_candidate_probe
cargo run --release -p lean-rs-worker-child --example header_cache_probe
```

`proof_candidate_probe` runs proof candidates through the real worker session against the checked-in fixture and prints
ordered statuses, JSON size, and child RSS samples beside wall-clock latency. `header_cache_probe` exercises the header
cache over an existing project without building its modules. Compare before/after on the same machine; report the named
workload, relevant payload evidence, and parent/child RSS or explicit unavailable status. Worker cycling under an import
budget belongs to the [profiling guide](https://github.com/jcreinhold/lean-rs/blob/main/profiling/README.md).

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
