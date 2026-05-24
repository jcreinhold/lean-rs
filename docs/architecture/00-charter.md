# Architecture Charter

The design boundary between Lean and `lean-rs`, the smallest public interface that supports it, and the alternatives
that were considered and rejected. Any new API or behavior must clear three questions: does it hide what should be
hidden, preserve what should be preserved, discard what should be discarded?

The charter pins intent, not Rust items or symbol names; those live in the per-crate API review baselines under
[`docs/api-review/`](../api-review/).

## Purpose

Lean owns elaboration, kernel checking, proof objects, universes, `MetaM`, and dependent-type meaning. `lean-rs` owns
linking, runtime initialization, ABI conversion, module loading, error and panic boundaries, scheduling, diagnostics,
batching, and packaging. The two halves do not negotiate. Anything that asks Rust to recompute a Lean semantic fact is
out of scope; anything that asks Lean to know about Rust hosting (thread pools, panic conversion, FFI batching, module
loaders) is out of scope.

## Adopted Shape

Five published crates plus one workspace-internal helper:

- **`lean-rs-sys`** (published). Raw `extern "C"` view of `lean.h`, split by semantic category; pure-Rust mirrors of
  `lean.h`'s `static inline` refcount helpers via `AtomicI32::from_ptr`; the `REQUIRED_SYMBOLS` allowlist; build-time
  `LEAN_HEADER_DIGEST`; link directives. Public types are opaque (`lean_object` is
  `[u8; 0] + PhantomData<(*mut u8, PhantomPinned)>`); layout structs are `pub(crate)`. The one crate-wide
  `#[allow(unsafe_code)]` opt-out; every `unsafe { ... }` block carries a `// SAFETY:` comment naming the invariant.
- **`lean-toolchain`** (published). Toolchain discovery, typed `ToolchainFingerprint`, fixture digest, layered link
  diagnostics, build-script helpers. Re-exports `LEAN_VERSION`, `LEAN_HEADER_DIGEST`, and `REQUIRED_SYMBOLS` from
  `lean-rs-sys` so the allowlist lives in one place.
- **`lean-rs`** (published, **L1**). The safe FFI primitive: bring the runtime up, open a Lake-built dylib, initialise a
  module, call typed `@[export]` functions, and register Rust callbacks through a generic shim package. Five public
  modules (`error`, `module`, `handle`, `runtime`, `abi`) plus one `#[doc(hidden)] pub mod __host_internals` giving the
  sibling `lean-rs-host` the small set of `LeanError`-constructor wrappers it needs without exposing them to external
  callers. It has no theorem-prover host shim contract; every downstream that just needs to call Lean from Rust starts
  here.
- **`lean-rs-host`** (published, **L2**). The opinionated theorem-prover-host stack built on `lean-rs`: `LeanHost`,
  `LeanCapabilities`, `LeanSession`, elaboration / evidence / meta surfaces, `SessionPool`. Owns and bundles the 28 + 6
  `lean_rs_host_*` `@[export]` Lean shim contract it loads alongside consumer capability dylibs. Batch and session-pool
  operations are methods on `LeanSession` rather than a separate `batch` module. Downstreams that want this opinion add
  it on top of `lean-rs`; downstreams that don't aren't paying for it.
- **`lean-rs-worker`** (published process-boundary layer). The worker crate supervises a child process around the host
  stack. It owns process lifecycle, private framing, request timeouts, fatal-exit classification, memory cycling, live
  row streaming, diagnostics, terminal summaries, capability metadata, and typed command facades. It is not a remote
  `LeanSession` mirror and not a `lean-dup` API. Downstreams bring their own command names and serde row schemas.

`lean-rs-test-support` is workspace-internal (`publish = false`).

### Composition Rule

Compose at the highest layer that fits the workload:

- use `lean-rs` for custom same-process ABI calls, module loading, and advanced callbacks;
- use `lean-rs-host` for trusted in-process theorem-prover work such as imports, elaboration, kernel checks, declaration
  queries, progress, cancellation, and pooling;
- use `lean-rs-worker` when the application needs a process boundary for fatal Lean exits, request watchdogs, worker
  cycling, live row streams, diagnostics, or typed downstream commands.

Lower layers are still real APIs. They are escape hatches and implementation substrates, not steps every downstream
caller should hand-compose.

### Lifetime spine

The universal currency inside `lean-rs` is a token-bound object handle: `runtime::Obj<'lean>` carries a phantom lifetime
tied to a `&'lean LeanRuntime` borrow. Public types built on top—the L1 semantic handles (`LeanExpr<'lean>`,
`LeanName<'lean>`, …) and the L2 surfaces in `lean-rs-host` (`LeanHost<'lean>`, `LeanCapabilities<'lean, 'h>`,
`LeanSession<'lean, 'c>`)—propagate the lifetime so the type system enforces *init-before-use* and *no value escapes the
runtime*. The pattern is borrowed from PyO3's `Bound<'py, T>`; `LeanRuntime` plays the role `Python<'py>` plays, except
creation is process-once rather than GIL-scoped. `'lean` is invisible at typical call sites (inferred from the runtime
borrow) and disappears from the `*_rs::*` re-exports when bound to `'static`.

The L1/L2 crate split gives each layer its own semver and refactor surface: `lean-rs`'s internal modules can reshape
without affecting `lean-rs-host` callers, and vice versa, as long as both crate roots stay stable.

Topic deep-dives:

- [`05-raw-sys-design.md`](05-raw-sys-design.md) — `lean-rs-sys` per-decision rationale.
- [`03-host-stack.md`](03-host-stack.md) — `lean-rs-host` curated surface.
- [`14-interop-release-contract.md`](14-interop-release-contract.md) — reusable interop release contract.
- [`16-production-boundary.md`](16-production-boundary.md),
  [`17-worker-session-adapter.md`](17-worker-session-adapter.md) — worker-process boundary and the host-session subset
  crossing it.
- [`18-worker-data-streaming.md`](18-worker-data-streaming.md) — downstream row streams over the worker boundary.
- [`20-worker-pool.md`](20-worker-pool.md) — local worker-pool boundary.
- [`22-worker-row-batching.md`](22-worker-row-batching.md),
  [`23-worker-data-plane-format.md`](23-worker-data-plane-format.md) — row batching and data-plane format decisions.
- [`24-lean-side-worker-streaming.md`](24-lean-side-worker-streaming.md) — Lean-side worker envelope helpers.
- [`28-production-scale-release.md`](28-production-scale-release.md) — production-scale worker contract.

## Hidden knowledge

No public Rust API requires a caller to know any of this. If an L1 item appears in `lean-rs`'s public surface, or an L2
item in `lean-rs-host`'s, the wrong layer has taken ownership.

**Hidden by `lean-rs` (L1):**

- Runtime init order: `lean_initialize_runtime_module`, `lean_initialize`, per-thread `lean_initialize_thread` /
  `lean_finalize_thread`, process-args setup, `LEAN_INIT_MUTEX`.
- `lean_object` layout: tag bits, packed scalar encoding, ctor field placement, scalar vs heap distinction
  (`lean_is_scalar`).
- Reference counting: `lean_inc` / `lean_dec`, owned vs borrowed args (`lean_obj_arg` vs `b_lean_obj_arg`), owned
  results (`lean_obj_res`), in-place reuse rules.
- Module initializer names and ordering: per-module `initialize_<Module>` symbols, dependency order, idempotent flag.
- Object conversion: boxed scalars (`lean_box` / `lean_unbox`), strings, bytearrays, arrays, ctor structures, closures.
- Exception and panic boundary: Lean exceptions to typed Rust errors; Rust panics caught before unwinding across a C
  frame.
- The seam between Lean semantic authority and Rust hosting: Rust never owns a semantic fact about a Lean term.

**Hidden by `lean-rs-host` (L2):**

- Session lifecycle: `LeanHost` → `LeanCapabilities` → `LeanSession` construction order, imports cache, capability
  refresh, the 28 + 6 `lean_rs_host_*` Lean shim contract.
- Capability dispatch: `SessionSymbols` cached address tables, `Args` / `R` propagation through `call_capability`,
  tracing-span shape and metrics.
- Batching: per-source result aggregation, `N + 1` vs `2N` FFI-cost analysis behind bulk methods, strict / skip-missing
  semantics.
- Pool capacity: FIFO-take / LIFO-push reuse, capability-agnostic storage (entries are bare `Obj<'lean>` environments
  rewrapped at acquire), over-capacity drop accounting.

## Decisions that must not leak

Implementation details. Changing them is allowed; surfacing them in the public API is a contract change.

- `lean_object` layout (tag bits, header, ctor field order).
- Borrowed vs owned RC tokens (`lean_obj_arg`, `b_lean_obj_arg`, `lean_obj_res`).
- Module initializer symbol names (`initialize_<Module>`), ordering, per-module idempotence flag.
- Lake search policy: how `lean-toolchain` finds Lake, search order, cache, fallback discovery for embedders without a
  Lake workspace.
- `MetaM` execution details: elaborator state, meta-level monad stack, trace and option propagation.
- Raw proof-term interpretation: `Expr`, universe levels, declaration bodies, environment internals.

## Preserved capability

Rust applications using `lean-rs` can call Lean code, ask the elaborator and kernel semantic questions through bounded
host capabilities, and receive typed results. They can load compiled Lean modules, invoke exported functions, batch
calls, and reuse sessions. None of this requires reaching into `lean-rs-sys` or knowing any item in the hidden-knowledge
table. Applications that legitimately need raw FFI—for example, to call a Lean capability not yet wrapped in
`lean-rs`—can opt in by depending on `lean-rs-sys` directly, accepting full `unsafe` discipline. This is friendlier than
a workspace fork.

## Discarded behaviour

Not in scope; will not be added.

- **Raw `lean_*` calls through `lean-rs`.** Raw imports live in `pub(crate)` modules and are never re-exported through
  `lean-rs`'s safe surface. Applications needing raw FFI depend on `lean-rs-sys` directly (opting in to full `unsafe`
  discipline and opaque public types). The recommended path is to contribute the missing capability to `lean-rs`'s safe
  layer.
- **Rust-side reconstruction of Lean semantics.** No parallel representation of `Expr`, universes, environments, or
  proof terms.
- **Unmeasured FFI micro-optimizations.** Any performance claim is backed by a named workload, command, before number,
  and after number—the discipline in `PERFORMANCE-BASELINE`.

## Rejected alternatives

Each was considered before the adopted shape. Recorded so reviewers can recognize a regression toward one of them.

- **A safe wrapper over all of `lean.h`.** Surface is large and shallow. Every ABI decision (`lean_obj_arg` direction,
  RC obligation, initializer ordering, ctor field layout) ends up in the public type system, so the caller still has to
  know everything `lean.h` knows. The "safety" is nominal.
- **Mirror Lean internals in Rust.** Creating Rust types for `Expr`, `Level`, `Name`, environments, and elaborator state
  creates a second source of truth. Drift is guaranteed the moment Lean's internals evolve, and drift here means
  *quietly wrong proofs*. The charter's first rule—Lean owns Lean semantics—exists to make this impossible.
- **Thin façade re-exporting raw FFI directly.** Pushes the entire initialization, refcount, and error contract back
  onto callers. No error or panic boundary; degenerates into raw-FFI-with-extra-steps.
- **Six published crates, one per layer** (`lean4-sys` → `lean4-runtime` → `lean4-abi` → `lean4-module` → `lean4-host` +
  test-support). Fake public-API story: no real downstream picks up `lean4-abi` or `lean4-runtime` in isolation; they
  take `lean4-host`. Cuts against the dominant Rust binding shape (`git2`+`libgit2-sys`, `openssl`+`openssl-sys`,
  `z3`+`z3-sys`, `rusqlite`+`libsqlite3-sys`, the pyo3 family), which is consistently two or three crates: a raw `*-sys`
  plus one safe front door, plus a build helper in larger stacks. The internal *organization* the layer cake encodes is
  real and worth preserving; `pub(crate)` modules inside `lean-rs` police those boundaries at zero semver cost.
- **External `lean-sys` adoption.** Considered taking `digama0/lean-sys` as the raw FFI source. Rejected: the published
  `0.0.9` was pinned to a Lean below our target, and the surfaces we needed (`LEAN_VERSION` const,
  `cargo:rerun-if-changed=lean.h`, signature-checked allowlist, typed diagnostics) would have required ongoing upstream
  PRs the published crate did not provide.
- **Conflate L1 FFI primitive and L2 host stack in one crate.** Putting both layers behind one default entry point
  (`LeanHost`) made it impossible for an external L1-only consumer to depend on `lean-rs = "0.1"` without first
  satisfying the `lean_rs_host_*` shim contract. Generic callback helpers belong below the host stack; theorem-prover
  host shims do not.

The adopted shape is deeper than each rejected alternative: fewer caller-facing details, less temporal coupling, a small
unsafe surface, and a one-line in-process layering invariant (`lean-rs-sys → lean-toolchain → lean-rs → lean-rs-host`)
with `lean-rs-worker` wrapping the host stack when callers need a process boundary. It matches the dominant Rust binding
shape, so contributors arrive with correct expectations, and it contains no Rust-side dependent-type imitation.
