# Performance Interventions (Prompt 22)

This document records the measurement-driven interventions applied after
the prompt-21 baseline (`docs/performance/baseline.md`), the workloads
each one was kept or discarded against, and the candidates that were
considered and not pursued.

Capture machine: Apple M4 Pro, macOS 26.4.1 (arm64), Lean 4.29.1, Rust
1.95.0 — identical to the baseline. Criterion baseline label: `pre-22`,
saved from commit `909e1be` (the prompt-21 baseline-harness commit)
before any prompt-22 edit. Post-22 numbers come from criterion's
`--baseline pre-22` comparison after the kept interventions landed.

## Intervention A — borrowed-string `LeanAbi`

**Status:** kept.

**Change:** added `impl<'lean> LeanAbi<'lean> for &str` in
`crates/lean-rs/src/abi/string.rs`. The `into_c` reuses the existing
`from_str` helper (one Lean-side allocation via
`lean_mk_string_from_bytes_unchecked`), exactly matching `String`'s
encode cost. `from_c` returns a `HostStage::Conversion` error: a
borrowed-return shape would have no source lifetime to borrow from, so
the impl is encode-only by design (sealed, never reached through normal
flow — `LeanExported<Args, R>` puts `&str` only in `Args`).

Five session shims previously took `&str` from callers but allocated an
owned `String` solely to satisfy `LeanAbi<'lean> for String`:

| Site | `to_owned()` calls eliminated |
| --- | ---: |
| `LeanSession::elaborate` (`host/session.rs:471`) | 3 |
| `LeanSession::kernel_check` (`host/session.rs:520`) | 3 |
| `LeanSession::elaborate_bulk` (`host/session.rs:761`) | 2 |
| `LeanSession::make_name` (`host/session.rs:853`) | 1 per name |
| (Total per `query_declarations_bulk` of N names) | `N + … ` |

**Measured impact** (`cargo bench -p lean-rs --bench session -- --baseline pre-22`,
`change` column is the mean delta with 95% CI):

| Workload | pre-22 mean | post-A mean | change (mean, p) |
| --- | ---: | ---: | --- |
| `host::session::query_declarations_bulk/1` | 674 ns | 651 ns | −2.1% (p=0.00) |
| `host::session::query_declarations_bulk/4` | 2.35 µs | 2.34 µs | −0.8% (within noise) |
| `host::session::query_declarations_bulk/8` | 4.79 µs | 4.53 µs | **−5.7% (p=0.00)** |
| `host::session::query_declarations_bulk/16` | 9.46 µs | 8.97 µs | **−5.7% (p=0.00)** |
| `host::session::elaborate_small` | 372 µs | 374 µs | +0.4% (no change) |
| `host::meta::run_meta_whnf` | 1.91 µs | 1.92 µs | +0.4% (no change) |
| `host::pool::session_reuse_hit` | 65.9 ns | 63.8 ns | −3.6% (unrelated, CPU state) |

The 5.7% win at N=8/16 of `query_declarations_bulk` matches the
structural prediction: each batched name pays one fewer `String`
malloc through `make_name`, and the marginal alloc cost dominates the
per-element work in the loop.

`elaborate_small` saw no measurable delta — the 3 saved `String`
mallocs (~300 ns at this machine's malloc rate) are lost in the
elaborator's 372 µs dominant cost. dhat would confirm the allocations
were eliminated; we did not capture a separate dhat workload because the
bench-level conclusion (kept on `query_declarations_bulk` evidence) is
already decisive and adding the workload would have meant a probe
infrastructure change for confirmation rather than decision.

`hot_paths` workloads showed uniform −2 to −3% improvement across
workloads that do not exercise any of the changed call sites
(`module::scalar_dispatch_u32_add`, `abi::string_roundtrip/*`,
`abi::array_string_roundtrip/*`). That is CPU-state noise between
captures, not attributable to Intervention A; the takeaway is **no
regression**.

**Correctness:** `cargo test -p lean-rs -- --test-threads=1` passes (134
lib tests + integration suites). The new
`abi::tests::borrowed_str_arg_round_trips_through_string_identity` test
covers the empty / ASCII / non-ASCII UTF-8 / embedded-NUL cases by
calling the fixture's `string_identity` export with `&str` arguments
through `module.exported::<(&str,), String>(...)`.

**Command:**

```sh
cargo bench -p lean-rs --bench session   -- --save-baseline pre-22   # pre
# (apply Intervention A)
cargo bench -p lean-rs --bench session   -- --baseline pre-22         # post
cargo bench -p lean-rs --bench hot_paths -- --baseline pre-22         # regression check
```

## Intervention B — fused Lean-array builder for `query_declarations_bulk`

**Status:** considered, not pursued.

The plan reserved a second intervention to eliminate the intermediate
`Vec<LeanName>` allocation at `host/session.rs:708`. Reasoning after
Intervention A measured out:

- The residual saving is one ~16–128-byte Rust `Vec` allocation per call
  (one per query, regardless of element type — the per-element cost is
  the `make_name` work Intervention A already fixed).
- For `query_declarations_bulk/16` at 8.97 µs post-A, a single malloc
  saved is ~50–100 ns, i.e. 0.6–1%. Within the criterion CI for that
  workload.
- A fused builder requires handling mid-iteration errors safely. Lean's
  `lean_alloc_array(len, len)` returns uninitialised slots; if iteration
  errors mid-way, `lean_dec` of the partial array would walk uninitialised
  pointers (UB). The two safe paths — allocating with `m_size = 0` and
  bumping `m_size` as slots fill (requires `lean-rs-sys`'s `LeanArrayObjectRepr`
  internal field access), or pre-filling all slots with `lean_box(0)` so
  uninit reads are scalar-tagged no-ops in `lean_dec` — each add
  N pointer writes per call, negating the saving at moderate N.

Per the prompt-22 plan's "bounded — only the highest-leverage
interventions" scope, this candidate is not implemented. Re-evaluate
when (a) a profile of a real workload shows `query_declarations_bulk`
contributing a meaningful fraction of end-to-end time, or (b) the same
fused-iterator helper would also benefit a second call site (`elaborate_bulk`'s
`sources_owned` Vec is one such candidate).

## Considered, not pursued

The prompt-22 plan listed five "likely targets" for inspection. After
exploration each was ruled out:

- **Repeated symbol lookup in `lean_rs::module`.** Exploration confirmed
  `dlsym` runs once per `LeanModule::exported(name)` call
  (`module/library.rs:179–193`), not per `.call(...)`. The `LeanExported`
  handle then caches a function-pointer address (`module/exported.rs:288–296`),
  and `.call` dispatches through one indirect call — no name lookup, no
  lock, no hashmap read. The baseline `module::scalar_dispatch_u32_add`
  at 768 ps is at the floor for an indirect FFI call on this CPU.
- **Avoidable `String` allocation in `lean_rs::error`.** The `Ok` branch
  of `decode_io` (`error/io.rs:60–85`) pays zero string cost; only the
  exception branch materialises a bounded `String`. `bound_message`
  (`error/mod.rs:255–270`) is a zero-cost guard for short inputs.
  Errors are structurally required to own their diagnostic text per
  `LeanError`'s contract (`RD-2026-05-17-006`), so no allocation is
  redundant on the hot path.
- **Array conversion temporary vectors in `pub(crate) lean_rs::abi`.**
  `Vec<T>::try_from_lean` already pre-sizes the output Vec with
  `Vec::with_capacity(size)` (`abi/array.rs:88`), and `from_iter_exact`
  already constructs the Lean array in one `lean_alloc_array(len, len)`
  pass with `.into_raw()` ownership transfers per slot
  (`abi/array.rs:43–63`). The per-element `String` allocation is the
  structural floor for `Vec<String>` decoding — every element must own
  its bytes.
- **Needless `Obj<'lean>` clone/inc/dec pairs in `pub(crate) lean_rs::runtime`.**
  Audit of `abi/array.rs:97–99`, `module/exported.rs:457–522`, and the
  bench harnesses surfaced no gratuitous clone→inc→drop patterns.
  Every transfer boundary uses `Obj::into_raw()` (`runtime/obj.rs:105–109`)
  to move ownership without refcount traffic. The `expr.clone()` inside
  `benches/session.rs:133` is intentional — the bench measures the cost
  users pay when reusing an expression across `run_meta` calls.
- **Repeated import/session setup in `lean_rs::host`.** `SessionPool`
  (`host/pool.rs`) already amortises imports across acquires. The
  baseline `host::pool::session_reuse_hit` at 65–81 ns covers
  `RefCell::borrow_mut` + LIFO pop + shallow session re-wrap; the only
  remaining surface is the linear-scan `take_matching` on the free
  list, which is `O(capacity)` rather than `O(1)`. No workload exercises
  a multi-key pool with hostile scan behaviour, so the optimisation
  isn't earned by use.

## End-to-end workload — `session_workflow`

A new example, `crates/lean-rs/examples/session_workflow.rs`, runs the
representative cross-call sequence once per process and prints one
`name=<stage> elapsed_us=<u64>` line per stage. The eight stages cover
host open, capability load, session import (Lean prelude + fixture
modules), two `elaborate` calls (cold then warm in one session), one
`kernel_check`, one `query_declarations_bulk` of three fixture
declarations, and one `Meta.whnf` invocation.

Means over 10 invocations (post-Intervention A, post-prompt-22 commit):

| Stage | Mean (µs) | Min (µs) | Max (µs) |
| --- | ---: | ---: | ---: |
| `host_open` | 16 | 10 | 23 |
| `load_caps` | 1,357 | 1,212 | 1,529 |
| `session_import` | 642,704 | 589,067 | 797,140 |
| `elaborate_1` (cold) | 1,260 | 1,123 | 1,476 |
| `elaborate_2` (warm) | 515 | 451 | 633 |
| `kernel_check_1` | 72,931 | 68,204 | 79,109 |
| `query_bulk_3` | 16 | 12 | 18 |
| `meta_whnf` | 14 | 12 | 17 |

No baseline comparison: this workload is new in prompt 22. Future
regression checks compare against these numbers as the post-22 floor.

**Command:**

```sh
cargo build --release -p lean-rs --example session_workflow
for i in $(seq 1 10); do ./target/release/examples/session_workflow; done
```

Notes:

- `session_import` is dominated by Lean's prelude import — independent
  of any prompt-22 change. The 200 ms spread (589 → 797 ms) reflects
  Lean's elaborator state drift across processes, consistent with the
  ~±20% elaborator variance baseline.md already records.
- `elaborate_2 < elaborate_1` because the second call reuses elaborator
  caches set up by the first within the same session.
- `query_bulk_3` at 16 µs ≈ the per-call `make_name × 3 + bulk dispatch`
  cost predicted by the bench. The post-A `query_declarations_bulk/4`
  bench number (2.34 µs amortised per iteration) does not subsume this:
  bench iterations run inside a warm-cache loop, the e2e probe runs once
  with the cache cold for these specific names.

## Reverted interventions

None this session. Both pursued candidates (A applied, B deferred) made
their disposition before code lands; no edits are reverted.
