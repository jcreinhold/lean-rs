# Performance Baseline

A regression floor captured on one machine on 2026-05-17. Numbers below are reproducible from
the commands in each section; they are not targets, service-level objectives, or comparisons
against any other transport.

Use this file to verify that an intervention moves the right path in the right direction. To
declare a regression, re-capture on the same hardware and compare.

## Capture environment

- Apple M4 Pro, 12 cores, 24 GiB RAM, macOS 26.4.1 (arm64).
- `rustc 1.95.0 (59807616e 2026-04-14)`; Lean `4.29.1` (`f72c35b3f637c8c6571d353742168ab66cc22c00`, arm64-apple-darwin24.6.0, Release).
- Lake fixture digest `d1a2ee71e75c7709daedf618d64418423cca8f29e1e30b44f54eac030758d77e` (`lean_toolchain::LAKE_FIXTURE_DIGEST`).
- Commit `0816098`, working tree clean before bench runs.
- `release` profile under `cargo bench` / `cargo run --release`. No `RUSTFLAGS` overrides; no `target-cpu=native`; no custom Rust allocator except where dhat is explicitly enabled.

## Workload index

| Workload | Where it lives | What it measures |
| --- | --- | --- |
| `runtime_init_cold` | `examples/cold_probe.rs` | First call to `LeanRuntime::init` |
| `library_open_cold` | `examples/cold_probe.rs` | First `LeanLibrary::open` on the fixture dylib |
| `module_initialize_cold` | `examples/cold_probe.rs` | First `LeanLibrary::initialize_module` for `LeanRsFixture` |
| `module::scalar_dispatch_u32_add` | `benches/hot_paths.rs` | `LeanExported<(u32,u32),u32>::call` on `lean_rs_fixture_u32_add` |
| `abi::string_roundtrip/{16,256,4096}` | `benches/hot_paths.rs` | `LeanExported<(String,),String>::call` on `lean_rs_fixture_string_identity` |
| `abi::array_string_roundtrip/{1,16,256}` | `benches/hot_paths.rs` | `LeanExported<(Vec<String>,),Vec<String>>::call` on `lean_rs_fixture_array_string_identity` |
| `host::session::query_declarations_bulk/{1,4,8,16}` | `crates/lean-rs-host/benches/session.rs` | `LeanSession::query_declarations_bulk` |
| `host::session::elaborate_small` | `crates/lean-rs-host/benches/session.rs` | `LeanSession::elaborate("(1+1 : Nat)", None, &opts)` |
| `host::meta::run_meta_whnf` | `crates/lean-rs-host/benches/session.rs` | `LeanSession::run_meta(&whnf(), <Nat.zero type>, &opts)` |
| `host::pool::session_reuse_hit` | `crates/lean-rs-host/benches/session.rs` | `SessionPool::acquire` on a warm 1-slot pool |

## Cold paths

Each `cold_probe` invocation prints one line per stage; n = 25 invocations.

| Workload | Mean | Min | Max |
| --- | ---: | ---: | ---: |
| `runtime_init_cold` | 16.4 ms | 15.9 ms | 16.9 ms |
| `library_open_cold` | 1.25 ms | 1.17 ms | 1.44 ms |
| `module_initialize_cold` | 8 µs | 6 µs | 12 µs |

```sh
cargo build --release -p lean-rs --example cold_probe
for i in $(seq 1 25); do ./target/release/examples/cold_probe; done
# or, with hyperfine on PATH (adds its own process-spawn time):
hyperfine --runs 25 './target/release/examples/cold_probe'
```

For cold-stage attribution prefer the `for` loop and aggregate the `name=… elapsed_us=…` lines.

## Hot paths—`lean_rs::module` and `lean_rs::abi`

Criterion estimates, median of 100 samples, 5 s collection window. Sorted by mean cost.

| Workload | Parameter | Mean | 95% CI |
| --- | --- | ---: | --- |
| `module::scalar_dispatch_u32_add` |—| 768 ps | 762–777 ps |
| `abi::string_roundtrip` | 16 B | 43.5 ns | 43.3–43.8 ns |
| `abi::array_string_roundtrip` | 1 elt | 83.0 ns | 82.6–83.8 ns |
| `abi::string_roundtrip` | 256 B | 130 ns | 129–130 ns |
| `abi::array_string_roundtrip` | 16 elts | 708 ns | 706–710 ns |
| `abi::string_roundtrip` | 4096 B | 1.30 µs | ±<.005 µs |
| `abi::array_string_roundtrip` | 256 elts | 10.2 µs | ±<.05 µs |

Throughput (informational; the per-iter mean is load-bearing): `string_roundtrip` at 4096 B
≈ 2.93 GiB/s; `array_string_roundtrip` at 256 elts ≈ 25 Melem/s.

```sh
cargo bench -p lean-rs --bench hot_paths
```

## Session paths—`lean_rs_host::*`

| Workload | Parameter | Mean | 95% CI | Note |
| --- | --- | ---: | --- | --- |
| `host::pool::session_reuse_hit` | warm cache | 81 ns | 77–86 ns | `Mutex::lock` + pop + rebind + drop + push; no Lean import |
| `host::session::query_declarations_bulk` | N=1 | 660 ns | 658–662 ns | one FFI call + `name_from_string` + `Obj` decode |
| `host::meta::run_meta_whnf` | type-of-`Nat.zero` | 1.93 µs | 1.92–1.96 µs | MetaM bring-up + per-iter `LeanExpr::clone` dominate; `Nat.zero`'s type is already `Sort 1` |
| `host::session::query_declarations_bulk` | N=4 | 2.40 µs | 2.39–2.42 µs | scales close to linear: `660 ns + ~600 ns/N` |
| `host::session::query_declarations_bulk` | N=8 | 4.74 µs | 4.72–4.75 µs | |
| `host::session::query_declarations_bulk` | N=16 | 10.2 µs | 9.84–10.6 µs | widest CI: 18% high-severe outliers from the per-call `Vec<LeanDeclaration>` |
| `host::session::elaborate_small` | `(1+1 : Nat)` | 464 µs | 422–506 µs | ±20% even at steady state; elaboration touches global Lean state that drifts between calls |

```sh
cargo bench -p lean-rs-host --bench session
```

## Allocation snapshots

dhat output for the conversion-heavy workloads. Each invocation runs the matching `hot_paths`
loop body for `ITERS = 1000` iterations with
`#[global_allocator] static A: dhat::Alloc = dhat::Alloc;` installed. Numbers cover the entire
process—Lean runtime bring-up, library open, module initialiser, plus 1000 workload iterations.

**Peak live ≈ 150 KiB / 20 blocks in every workload** (bring-up state: host, capabilities,
session). Treat this as the constant floor; interventions target the totals below.

| Workload | Total bytes | Total blocks | Per-iter blocks |
| --- | ---: | ---: | --- |
| `string_identity_16` | 184,535 | 2,050 | ~2 (one Rust `String` round-trip + bookkeeping) |
| `string_identity_256` | 664,776 | 2,050 | ~2 |
| `string_identity_4096` | 8,348,617 | 2,050 | ~2 |
| `array_string_1` | 214,563 | 4,052 | ~4 (per-element `String` decode + container slot) |
| `array_string_16` | 1,145,194 | 34,082 | ~34 (≈ 2N + 2) |
| `array_string_256` | 16,035,275 | 514,562 | ~514 (≈ 2N + 2) |

**Scaling.** `string_identity` block count is constant in payload size; total bytes scale
linearly with payload. `array_string` block count scales as `2N · ITERS`; total bytes also scale
linearly. Container allocations are transient, freed before the next iteration, so they hit the
totals but not the peak.

```sh
cargo build --release -p lean-rs --features dhat-heap --example alloc_probe
mkdir -p /tmp/dhat-runs && cd /tmp/dhat-runs
for w in string_identity_16 string_identity_256 string_identity_4096 \
         array_string_1 array_string_16 array_string_256; do
  rm -f dhat-heap.json
  /Users/jcreinhold/Code/lean-rs/target/release/examples/alloc_probe "$w"
  # dhat-heap.json carries per-call-site backtraces; inspect with
  # https://nnethercote.github.io/dh_view/dh_view.html
done
```

## How to read these numbers

- **Re-capture before declaring a regression.** Numbers are not portable across machines, OSes, or Lean versions, and the build uses the default `release` profile (no `target-cpu=native`) so the floor stays reproducible from CI.
- **Cold-path numbers are wall-clock from a single in-process measurement per run.** Variance comes from re-invocation; no statistical model.
- **`elaborate_small` warm steady-state under-counts cold elaboration.** Each session accumulates state across calls; the first call in a fresh session is systematically slower.
- **`session_reuse_hit` measures hot-cache LIFO pop only.** Cold-cache acquires cost an additional `LeanSession` construction and full Lean import—not measured here.
- **dhat sees Rust allocations only.** Lean's internal mimalloc is statically linked into `libleanrt.a` (`crates/lean-rs-sys/build.rs`) and invisible to `#[global_allocator]`. Allocation numbers capture host-stack churn (ABI conversions, `Vec`/`String` buffers, error message bounding), not kernel-side heap.
- **No subprocess / IPC comparison.** This baseline is `lean-rs` in-process only.
- **Bytearray ABI is not benched at the public surface.** `bytearray::*` helpers in `crates/lean-rs/src/abi/bytearray.rs` are `pub(crate)`; the module-comment explains the disambiguation against `Array UInt8`. The boxed-array `Vec<String>` benches above cover the equivalent allocation patterns at the public surface.
