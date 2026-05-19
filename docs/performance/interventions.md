# Performance Interventions

Measurement-driven interventions applied after the recorded baseline
([`baseline.md`](baseline.md)). Each one is recorded with the workloads it was kept or
discarded against, and the candidates that were considered and not pursued.

Capture machine identical to the baseline: Apple M4 Pro, macOS 26.4.1 (arm64), Lean 4.29.1,
Rust 1.95.0. Criterion baseline label `pre-22`, saved from commit `909e1be` before any
intervention. Post-intervention numbers come from criterion's `--baseline pre-22` comparison
after the kept interventions landed.

## Intervention A—borrowed-string `LeanAbi`

**Status: kept.** Added `impl<'lean> LeanAbi<'lean> for &str` in `crates/lean-rs/src/abi/string.rs`.
`into_c` reuses the existing `from_str` helper (one Lean-side allocation via
`lean_mk_string_from_bytes_unchecked`), matching `String`'s encode cost. `from_c` returns
`HostStage::Conversion`: a borrowed-return shape has no source lifetime to borrow from, so the
impl is encode-only by design (sealed, never reached through normal flow—`LeanExported<Args, R>`
puts `&str` only in `Args`).

Five session shims previously took `&str` from callers but allocated an owned `String` solely to
satisfy `LeanAbi<'lean> for String`:

| Site (`crates/lean-rs-host/src/host/session.rs`) | `to_owned()` calls eliminated |
| --- | ---: |
| `LeanSession::elaborate` (line 503) | 3 |
| `LeanSession::kernel_check` (line 560) | 3 |
| `LeanSession::elaborate_bulk` (line 837) | 2 |
| `LeanSession::make_name` (line 941) | 1 per name |
| `query_declarations_bulk` of N names (aggregate) | N + 8 |

### Measured impact

`cargo bench -p lean-rs --bench session -- --baseline pre-22`. Sorted by Δ magnitude.

| Workload | pre-22 | post-A | Δ | p | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| `query_declarations_bulk/8` | 4.79 µs | 4.53 µs | −5.7% | 0.00 | kept |
| `query_declarations_bulk/16` | 9.46 µs | 8.97 µs | −5.7% | 0.00 | kept |
| `query_declarations_bulk/1` | 674 ns | 651 ns | −2.1% | 0.00 | kept |
| `query_declarations_bulk/4` | 2.35 µs | 2.34 µs | −0.8% |—| within noise |
| `elaborate_small` | 372 µs | 374 µs | +0.4% |—| within noise |
| `meta::run_meta_whnf` | 1.91 µs | 1.92 µs | +0.4% |—| within noise |
| `pool::session_reuse_hit` | 65.9 ns | 63.8 ns | −3.6% |—| CPU-state noise, unrelated |

The 5.7% win at N=8/16 matches the structural prediction: each batched name pays one fewer
`String` malloc through `make_name`, and the marginal alloc cost dominates the per-element work
in the loop.

`elaborate_small` shows no measurable delta—the 3 saved `String` mallocs (~300 ns at this
machine's malloc rate) are lost in the elaborator's 372 µs cost. dhat would confirm the
allocations were eliminated, but the bench-level conclusion is already decisive on
`query_declarations_bulk`; adding a dhat workload here would mean changing the probe
infrastructure for confirmation rather than decision.

`hot_paths` workloads (`module::scalar_dispatch_u32_add`, `abi::string_roundtrip/*`,
`abi::array_string_roundtrip/*`) show uniform −2 to −3% across workloads that do not exercise
any changed call site. That is CPU-state noise between captures, not attributable to A; the
takeaway is **no regression**.

### Correctness

`cargo test -p lean-rs -- --test-threads=1` passes (134 lib tests plus integration suites).
The new `abi::tests::borrowed_str_arg_round_trips_through_string_identity` covers empty / ASCII
/ non-ASCII UTF-8 / embedded-NUL cases by calling the fixture's `string_identity` export with
`&str` arguments through `module.exported::<(&str,), String>(...)`.

### Commands

```sh
cargo bench -p lean-rs --bench session   -- --save-baseline pre-22   # pre
# apply Intervention A
cargo bench -p lean-rs --bench session   -- --baseline pre-22         # post
cargo bench -p lean-rs --bench hot_paths -- --baseline pre-22         # regression check
```

## Intervention B—fused Lean-array builder for `query_declarations_bulk`

**Status: considered, not pursued.** Would eliminate the intermediate `Vec<LeanName>` allocation
at `crates/lean-rs-host/src/host/session.rs:780`.

**Cost saved.** One ~16–128-byte Rust `Vec` allocation per call (regardless of element
type—the per-element cost is the `make_name` work A already fixed). For `query_declarations_bulk/16`
at 8.97 µs post-A, one saved malloc is ~50–100 ns, i.e. 0.6–1%. Within the criterion CI.

**Risk.** A fused builder has to handle mid-iteration errors safely. `lean_alloc_array(len, len)`
returns uninitialised slots; if iteration errors mid-way, `lean_dec` of the partial array would
walk uninitialised pointers (UB). The two safe paths each add overhead that negates the saving
at moderate N:

- Allocate with `m_size = 0` and bump `m_size` as slots fill. Requires `lean-rs-sys`'s `LeanArrayObjectRepr` internal field access.
- Pre-fill all slots with `lean_box(0)` so uninit reads become scalar-tagged no-ops in `lean_dec`. Adds N pointer writes per call.

**Decision.** Within the bounded-intervention scope (only the highest-impact changes earn
their seat), not implemented. Re-evaluate when either (a) a profile of a real workload shows
`query_declarations_bulk` contributing a meaningful fraction of end-to-end time, or (b) the
same fused-iterator helper would also benefit a second call site—`elaborate_bulk`'s
`sources_owned` Vec is one such candidate.

## Considered, not pursued

Five candidates were considered and ruled out:

- **Repeated symbol lookup in `lean_rs::module`.** `dlsym` runs once per `LeanModule::exported(name)` (`module/library.rs:179–193`), not per `.call(...)`. The `LeanExported` handle caches a function-pointer address (`module/exported.rs:288–296`); `.call` dispatches through one indirect call. The baseline `module::scalar_dispatch_u32_add` at 768 ps is at the floor for an indirect FFI call on this CPU.
- **Avoidable `String` allocation in `lean_rs::error`.** The `Ok` branch of `decode_io` (`error/io.rs:60–85`) pays zero string cost; only the exception branch materialises a bounded `String`. `bound_message` (`error/mod.rs:255–270`) is a zero-cost guard for short inputs. Errors are required to own their diagnostic text per `LeanError`'s contract; no allocation is redundant on the hot path.
- **Array conversion temporary vectors in `pub(crate) lean_rs::abi`.** `Vec<T>::try_from_lean` already pre-sizes with `Vec::with_capacity(size)` (`abi/array.rs:88`); `from_iter_exact` builds the Lean array in one `lean_alloc_array(len, len)` pass with `.into_raw()` ownership transfer per slot (`abi/array.rs:43–63`). The per-element `String` allocation is the structural floor for `Vec<String>` decoding—every element must own its bytes.
- **Needless `Obj<'lean>` clone/inc/dec pairs in `pub(crate) lean_rs::runtime`.** Audit of `abi/array.rs:97–99`, `module/exported.rs:457–522`, and the bench harnesses found no gratuitous clone→inc→drop patterns. Every transfer boundary uses `Obj::into_raw()` (`runtime/obj.rs:105–109`) to move ownership without refcount traffic. The `expr.clone()` inside `crates/lean-rs-host/benches/session.rs:133` is intentional—the bench measures the cost users pay when reusing an expression across `run_meta` calls.
- **Repeated import/session setup in `lean_rs_host`.** `SessionPool` (`crates/lean-rs-host/src/host/pool.rs`) already amortises imports across acquires. Baseline `host::pool::session_reuse_hit` at 65–81 ns covers `RefCell::borrow_mut` + LIFO pop + shallow session re-wrap. The one remaining surface—linear-scan `take_matching` on the free list (`O(capacity)`, not `O(1)`)—is not exercised by any workload with hostile scan behaviour, so the optimisation isn't earned.

## End-to-end workload—`session_workflow`

`crates/lean-rs/examples/session_workflow.rs` runs the representative cross-call sequence once
per process and prints one `name=<stage> elapsed_us=<u64>` line per stage. The eight stages
cover host open, capability load, session import (Lean prelude + fixture modules), two
`elaborate` calls (cold then warm in one session), one `kernel_check`, one
`query_declarations_bulk` of three fixture declarations, and one `Meta.whnf` invocation.

Means over 10 invocations (post-Intervention A), grouped by cost tier:

**Sub-millisecond stages**

| Stage | Mean | Min | Max |
| --- | ---: | ---: | ---: |
| `host_open` | 16 µs | 10 µs | 23 µs |
| `meta_whnf` | 14 µs | 12 µs | 17 µs |
| `query_bulk_3` | 16 µs | 12 µs | 18 µs |
| `elaborate_2` (warm) | 515 µs | 451 µs | 633 µs |
| `elaborate_1` (cold) | 1.26 ms | 1.12 ms | 1.48 ms |
| `load_caps` | 1.36 ms | 1.21 ms | 1.53 ms |

**Tens-of-milliseconds**

| Stage | Mean | Min | Max |
| --- | ---: | ---: | ---: |
| `kernel_check_1` | 73 ms | 68 ms | 79 ms |

**Hundreds-of-milliseconds**

| Stage | Mean | Min | Max |
| --- | ---: | ---: | ---: |
| `session_import` | 643 ms | 589 ms | 797 ms |

No baseline comparison: this workload was added alongside the interventions. Future
regression checks compare against these numbers as the post-intervention floor.

```sh
cargo build --release -p lean-rs --example session_workflow
for i in $(seq 1 10); do ./target/release/examples/session_workflow; done
```

**Reading the table.** `session_import` dominates total wall-clock by an order of magnitude over
everything else; it is Lean's prelude import and is independent of any intervention here. The
200 ms spread (589 → 797 ms) is Lean's elaborator state drift across processes, consistent with
the ±20% elaborator variance recorded in `baseline.md`. `elaborate_2 < elaborate_1` because the
second call reuses elaborator caches from the first within the same session. `query_bulk_3` at
16 µs matches the per-call `make_name × 3 + bulk dispatch` cost predicted by the bench; the
post-A `query_declarations_bulk/4` bench number (2.34 µs amortised) does not subsume
it—bench iterations run in a warm-cache loop while the e2e probe runs once with the cache cold
for these specific names.

## Reverted interventions

None this session. Both candidates made their disposition before code lands (A applied,
B deferred); no edits are reverted.
