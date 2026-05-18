# Performance Baseline

Recorded for prompt 22 to use as a regression floor — and only that. **These
numbers are not targets, not service-level objectives, and not a comparison
against any other transport.** They are a single capture on one machine, used
to verify that future interventions move the right paths in the right
direction.

Capture date: 2026-05-17.

## Machine and toolchain

- Hardware: Apple M4 Pro, 12 cores, 24 GiB RAM, macOS 26.4.1 (arm64).
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`.
- Lean: `4.29.1` (commit `f72c35b3f637c8c6571d353742168ab66cc22c00`, arm64-apple-darwin24.6.0, Release).
- Lake fixture digest (`lean_toolchain::LAKE_FIXTURE_DIGEST`):
  `d1a2ee71e75c7709daedf618d64418423cca8f29e1e30b44f54eac030758d77e`.
- Commit at capture: `0816098` (working tree clean before bench runs; the
  baseline-harness commit follows this one).

All numbers below come from the `release` profile under `cargo bench` /
`cargo run --release`. No `RUSTFLAGS` overrides, no `target-cpu=native`, no
custom allocator on the Rust side except where dhat is explicitly enabled.

## Workload index

| Surface | Workload | Where it lives | What it measures |
| --- | --- | --- | --- |
| `lean_rs::runtime` | `runtime_init_cold` | `examples/cold_probe.rs` | First call to `LeanRuntime::init` |
| `lean_rs::module` | `library_open_cold` | `examples/cold_probe.rs` | First `LeanLibrary::open` on the fixture dylib |
| `lean_rs::module` | `module_initialize_cold` | `examples/cold_probe.rs` | First `LeanLibrary::initialize_module` for `LeanRsFixture` |
| `lean_rs::module` | `module::scalar_dispatch_u32_add` | `benches/hot_paths.rs` | `LeanExported<(u32,u32),u32>::call` on `lean_rs_fixture_u32_add` |
| `lean_rs::abi` (string) | `abi::string_roundtrip/{16,256,4096}` | `benches/hot_paths.rs` | `LeanExported<(String,),String>::call` on `lean_rs_fixture_string_identity` |
| `lean_rs::abi` (array) | `abi::array_string_roundtrip/{1,16,256}` | `benches/hot_paths.rs` | `LeanExported<(Vec<String>,),Vec<String>>::call` on `lean_rs_fixture_array_string_identity` |
| `lean_rs_host::host::session` | `host::session::query_declarations_bulk/{1,4,8,16}` | `crates/lean-rs-host/benches/session.rs` | `LeanSession::query_declarations_bulk` |
| `lean_rs_host::host::session` | `host::session::elaborate_small` | `crates/lean-rs-host/benches/session.rs` | `LeanSession::elaborate("(1+1 : Nat)", None, &opts)` |
| `lean_rs_host::meta` | `host::meta::run_meta_whnf` | `crates/lean-rs-host/benches/session.rs` | `LeanSession::run_meta(&whnf(), <Nat.zero type>, &opts)` |
| `lean_rs_host::host::pool` | `host::pool::session_reuse_hit` | `crates/lean-rs-host/benches/session.rs` | `SessionPool::acquire` on a warm 1-slot pool |

## Cold paths

Cold paths run once per process. Each invocation of `cold_probe` prints one
line per stage; the numbers below are over 25 invocations.

| Workload | n | Mean (µs) | Min (µs) | Max (µs) |
| --- | ---: | ---: | ---: | ---: |
| `runtime_init_cold` | 25 | 16,387 | 15,866 | 16,927 |
| `library_open_cold` | 25 | 1,253 | 1,174 | 1,439 |
| `module_initialize_cold` | 25 | 8 | 6 | 12 |

**Command:**

```sh
cargo build --release -p lean-rs --example cold_probe
for i in $(seq 1 25); do ./target/release/examples/cold_probe; done
# or, if `hyperfine` is on PATH:
hyperfine --runs 25 './target/release/examples/cold_probe'
```

(`hyperfine` adds its own process-spawn timing; for cold-stage attribution
prefer the `for` loop and aggregate the `name=… elapsed_us=…` lines.)

## Hot paths — `lean_rs::module` + `lean_rs::abi`

Criterion estimates (median of 100 samples, default 5 s collection window).

| Workload | Parameter | Mean | 95% CI |
| --- | --- | ---: | --- |
| `module::scalar_dispatch_u32_add` | — | 768 ps | 762 ps – 777 ps |
| `abi::string_roundtrip` | 16 B | 43.5 ns | 43.3 ns – 43.8 ns |
| `abi::string_roundtrip` | 256 B | 130 ns | 129 ns – 130 ns |
| `abi::string_roundtrip` | 4096 B | 1.30 µs | 1.30 µs – 1.30 µs |
| `abi::array_string_roundtrip` | 1 elt | 83.0 ns | 82.6 ns – 83.8 ns |
| `abi::array_string_roundtrip` | 16 elts | 708 ns | 706 ns – 710 ns |
| `abi::array_string_roundtrip` | 256 elts | 10.2 µs | 10.2 µs – 10.2 µs |

Reported throughput from Criterion (informational; the per-iter mean is the
load-bearing number):

- `abi::string_roundtrip` at 4096 B: ≈ 2.93 GiB/s.
- `abi::array_string_roundtrip` at 256 elts: ≈ 25 Melem/s.

**Command:** `cargo bench -p lean-rs --bench hot_paths`

## Session paths — `lean_rs_host::*`

| Workload | Parameter | Mean | 95% CI |
| --- | --- | ---: | --- |
| `host::session::query_declarations_bulk` | N=1 | 660 ns | 658 ns – 662 ns |
| `host::session::query_declarations_bulk` | N=4 | 2.40 µs | 2.39 µs – 2.42 µs |
| `host::session::query_declarations_bulk` | N=8 | 4.74 µs | 4.72 µs – 4.75 µs |
| `host::session::query_declarations_bulk` | N=16 | 10.2 µs | 9.84 µs – 10.6 µs |
| `host::session::elaborate_small` | `(1 + 1 : Nat)` | 464 µs | 422 µs – 506 µs |
| `host::meta::run_meta_whnf` | type-of-`Nat.zero` | 1.93 µs | 1.92 µs – 1.96 µs |
| `host::pool::session_reuse_hit` | warm cache | 81 ns | 77 ns – 86 ns |

**Command:** `cargo bench -p lean-rs-host --bench session`

Notes:

- `query_declarations_bulk` scales close to linear in N (`660 ns + ≈600 ns/N`)
  — consistent with the design: one FFI call regardless of N, plus N
  in-band `name_from_string` resolutions and N `Obj` decodes. The 16-case CI
  is wider (18% high-severe outliers) because that workload allocates the
  largest per-call `Vec<LeanDeclaration>`.
- `elaborate_small` has high variance (~±20% of the mean) even at the
  steady-state limit. Elaboration touches global Lean state that drifts
  between calls; the median is the load-bearing number.
- `run_meta_whnf` calls into the bounded `MetaM` shim; the cost dominantly
  reflects MetaM bring-up plus the `LeanExpr::clone` per iteration, not
  whnf itself (`Nat.zero`'s type is already a `Sort 1` head).
- `session_reuse_hit` is the pool's hot-cache contract: 81 ns covers
  `Mutex::lock` + pop + session re-binding + drop + push, no Lean import.

## Allocation snapshots

dhat output for the conversion-heavy workloads. Each invocation runs the
matching `hot_paths` loop body for `ITERS = 1000` iterations with
`#[global_allocator] static A: dhat::Alloc = dhat::Alloc;` installed.
Numbers cover **the entire process**, including Lean runtime bring-up,
library open, module initialiser, and the 1000 iterations of the workload.

| Workload | Total bytes | Total blocks | Peak live bytes | Peak live blocks |
| --- | ---: | ---: | ---: | ---: |
| `string_identity_16` | 184,535 | 2,050 | 150,313 | 20 |
| `string_identity_256` | 664,776 | 2,050 | 150,314 | 20 |
| `string_identity_4096` | 8,348,617 | 2,050 | 150,315 | 20 |
| `array_string_1` | 214,563 | 4,052 | 150,309 | 20 |
| `array_string_16` | 1,145,194 | 34,082 | 150,310 | 20 |
| `array_string_256` | 16,035,275 | 514,562 | 150,311 | 20 |

Observations:

- `string_identity` block count is constant in payload size (≈ 2 blocks
  per iteration: one Rust-side `String` round-trip allocation +
  bookkeeping). Total bytes scale linearly with payload size.
- `array_string` block count scales with **N × ITERS × 2** — roughly two
  Rust-side allocations per element (the per-element `String` decode and
  its container slot). Container size is recorded against the peak only
  because all transient allocations are freed before the next iteration.
- Peak live bytes are ≈ 150 KiB in every workload, dominated by
  bring-up state (host, capabilities, session). Prompt 22 should treat
  bring-up peak as the constant floor and target **total** bytes for
  reduction.

**Command:**

```sh
cargo build --release -p lean-rs --features dhat-heap --example alloc_probe
mkdir -p /tmp/dhat-runs && cd /tmp/dhat-runs
for w in string_identity_16 string_identity_256 string_identity_4096 \
         array_string_1 array_string_16 array_string_256; do
  rm -f dhat-heap.json
  /Users/jcreinhold/Code/lean-rs/target/release/examples/alloc_probe "$w"
  # `dhat-heap.json` carries per-call-site backtraces; inspect with
  # https://nnethercote.github.io/dh_view/dh_view.html
done
```

## Limitations

- **One machine, one OS, one Lean.** Numbers are not portable. Re-capture
  before declaring a regression in CI or comparing intervention candidates.
- **No `target-cpu=native`.** The build uses the default `release` profile
  to keep the baseline reproducible from CI without machine-specific flags.
  Prompt 22 may experiment with `RUSTFLAGS`, but the regression-floor
  comparison must stay against this same profile.
- **dhat sees only Rust allocations.** Lean's internal mimalloc is
  statically linked into `libleanrt.a` (see
  `crates/lean-rs-sys/build.rs`) and is invisible to
  `#[global_allocator]`. The numbers above capture host-stack churn (ABI
  conversions, `Vec`/`String` buffers, error message bounding), not
  kernel-side heap.
- **Cold paths are wall-clock from a single in-process measurement per run.**
  Variance comes from re-invocation; there is no statistical model.
- **`elaborate_small` warm steady-state under-counts cold elaboration.**
  Each session accumulates state across calls; the first call in a fresh
  session is systematically slower. The bench captures warm cost only.
- **`session_reuse_hit` measures hot-cache LIFO pop only.** Cold-cache
  acquires (no warm-up) cost an additional `LeanSession` construction +
  full Lean import — not measured here.
- **No subprocess / IPC comparison.** The prompt explicitly disclaims this
  axis. This baseline covers `lean-rs` in-process only.
- **Bytearray ABI is not benched at the public surface.** `bytearray::*`
  helpers in `crates/lean-rs/src/abi/bytearray.rs` are `pub(crate)` — the
  module-comment explains the disambiguation rationale against
  `Array UInt8`. The boxed-array `Vec<String>` benches above cover the
  equivalent allocation patterns at the public surface.
