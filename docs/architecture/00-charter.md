# Architecture Charter

This document is the first thing every later prompt in the `lean-rs` sequence reads.
It states the design boundary between Lean and `lean-rs`, the smallest public
interface that supports that boundary, and the alternative designs we considered and
rejected. When a later prompt proposes an API or a behavior, the charter is the bar
it has to clear: does the API hide what it should hide, does it preserve what it
should preserve, does it discard what it should discard.

## Purpose

Lean owns elaboration, kernel checking, proof objects, universes, `MetaM`,
and dependent-type meaning. `lean-rs` owns linking, runtime initialization, ABI
conversion, module loading, error and panic boundaries, scheduling, diagnostics,
batching, and packaging. These two halves do not negotiate. Anything that asks Rust
to recompute or second-guess a Lean semantic fact is out of scope; anything that
asks Lean to know about Rust hosting (thread pools, panic conversion, FFI
batching, module loaders) is out of scope.

The charter is prose-first. It does not pin Rust items, crate versions, or symbol
names — those live in `00-current-state.md` and the per-prompt contracts. The
charter pins *intent*.

## Hidden knowledge owned by the binding stack

The binding stack encapsulates the following so that no public Rust API requires a
caller to know any of it:

- Lean runtime initialization order and idempotence: `lean_initialize_runtime_module`,
  `lean_initialize`, per-thread `lean_initialize_thread` / `lean_finalize_thread`,
  process-args setup, and the `LEAN_INIT_MUTEX` discipline.
- `lean_object` layout: tag bits, packed scalar encoding, ctor field placement, and
  the distinction between scalar (`lean_is_scalar`) and heap objects.
- Reference-counting conventions: `lean_inc` / `lean_dec`, owned vs. borrowed
  arguments (`lean_obj_arg` vs. `b_lean_obj_arg`), owned results (`lean_obj_res`),
  and the in-place reuse rules that depend on whether the runtime observes a
  unique reference.
- Module initializer symbol names and ordering: the per-module
  `initialize_<Module>` symbols, their dependency order, and the idempotent
  flag they each carry.
- Object conversion: boxed scalars (`lean_box` / `lean_unbox`), strings,
  bytearrays, arrays, ctor structures, and closures.
- The exception and panic boundary: how Lean exceptions become typed Rust
  errors, and how Rust panics are caught before they unwind across a C frame.
- The seam between Lean semantic authority and Rust hosting: Rust never owns a
  semantic fact about a Lean term.

If any of the above appears in a function signature, doc comment, or example in
the public API of `lean-rs`, that is a charter violation.

## Smallest public interface

Three published crates carry the public surface; one workspace-internal
helper stays out of it:

- `lean-rs-sys` (published, per `RD-2026-05-17-005`). Holds the curated
  `extern "C"` declarations for the subset of `lean.h` the stack uses, the
  pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, the
  `REQUIRED_SYMBOLS` allowlist, the build-time `LEAN_HEADER_DIGEST`, and
  the link directives. Public types (`lean_object`, etc.) are
  extern-type-equivalent opaque: callers hold pointers only and can only
  read/modify object state through this crate's `pub unsafe fn` helpers.
  Layout structs (`LeanObjectRepr`, the subclass headers) are `pub(crate)`
  only — Lean's header layout is a crate-private invariant pinned by the
  digest check. Publishing this crate is *opt-in unsafe raw FFI*; the safe
  layers in `lean-rs` are the recommended path.
- `lean-toolchain` (published). Owns Lean toolchain discovery, version
  metadata, typed `ToolchainFingerprint`, fixture digest, layered link
  diagnostics, and the build-script helpers downstream embedders can reuse.
  Re-exports `LEAN_VERSION`, `LEAN_HEADER_DIGEST`, and `REQUIRED_SYMBOLS`
  from `lean-rs-sys` directly — the allowlist lives in one place.
- `lean-rs` (published). The single safe front door for hosting Lean
  capabilities from Rust.

`lean-rs-test-support` is workspace-internal (`publish = false`) and
carries fixtures and helpers; it is not a public surface.

Inside `lean-rs`, the module layout mirrors the original layer story but
compresses it after a holistic review: **three publicly-visible modules**
(`module`, `host`, `error`) and **two `pub(crate)` infrastructure modules**
(`runtime`, `abi`). Bulk and pooling operations live as methods on
`LeanSession`, not in a sibling `batch` module — a separate `batch` module
would be a shallow wrapper that always borrows a session. The compression is
recorded in `RD-2026-05-17-004`. Module boundaries are policed by `pub(crate)`,
not by Cargo crate splits. Re-exports at the crate root are a curated public
API, not a path-shortening facade. A reader of `lean_rs::*` should see the
smallest set of items that lets them call Lean code, ask semantic questions,
and receive typed results.

## Decisions that must not leak

The following are implementation details. Changing them is allowed; surfacing
them in the public API is a contract change.

- `lean_object` layout (tag bits, header, ctor field order).
- Borrowed vs. owned RC tokens (`lean_obj_arg`, `b_lean_obj_arg`, `lean_obj_res`).
- Module initializer symbol names (`initialize_<Module>`), their ordering, and
  the per-module idempotence flag.
- Lake search policy: how `lean-toolchain` finds Lake, the search order, the
  cache, and the fallback discovery for embedders without a Lake workspace.
- `MetaM` execution details: the elaborator state, the meta-level monad stack,
  trace and option propagation.
- Raw proof-term interpretation: structure of `Expr`, universe levels,
  declaration bodies, environment internals.

## Preserved capability

Rust applications using `lean-rs` can call Lean code, ask the elaborator and
kernel semantic questions through bounded host capabilities, and receive typed
results. They can also load compiled Lean modules, invoke exported functions,
batch calls, and reuse sessions. None of this requires the caller to reach
into `lean-rs-sys` or to know any item in the *hidden knowledge* list above.
Applications that legitimately need raw FFI — for example, to call a Lean
capability not yet wrapped in `lean-rs` — can opt in by depending on
`lean-rs-sys` directly, accepting full `unsafe` discipline; per
`RD-2026-05-17-005`, this is friendlier than forcing a workspace fork.

## Intentionally discarded behavior

The following are *not* in scope and will not be added:

- Direct application use of raw `lean_*` calls *through `lean-rs`*. The
  imports live in `pub(crate)` modules and are never re-exported through
  `lean-rs`'s safe surface. Applications that need raw FFI can depend on
  `lean-rs-sys` directly (it is published per `RD-2026-05-17-005`), but doing
  so means opting in to the full `unsafe` discipline and the public types
  there are opaque pointer-targets: `lean_object` has size 0 and Lean header
  layout is a `lean-rs-sys`-internal invariant. The recommended path remains
  to contribute the missing capability to `lean-rs`'s safe layer.
- Rust-side reconstruction of Lean semantics. Rust does not maintain a parallel
  representation of `Expr`, universes, environments, or proof terms.
- Unmeasured FFI micro-optimizations. Any performance claim is backed by a named
  workload, command, before number, and after number — the discipline in
  `PERFORMANCE-BASELINE`.

## Design it twice

Each design below was considered before the adopted shape. They are recorded so
later prompts can recognize a regression toward one of them.

### Rejected: a safe wrapper over all of `lean.h`

A crate that adds a thin safe layer over every symbol in `lean.h`. Rejected:
the surface is large and shallow. Every ABI decision (`lean_obj_arg`
direction, RC obligation, initializer ordering, ctor field layout) ends up in
the public type system, so the caller still has to know everything `lean.h`
knows. The "safety" is nominal: the caller carries the same invariants the raw
crate would have demanded, but now spread across more types.

### Rejected: mirror Lean internals in Rust

A crate that mirrors `Expr`, `Level`, `Name`, environments, and the elaborator
state as Rust types and operates on them. Rejected: this creates a second
source of truth for Lean's semantic objects. Drift is guaranteed the moment
Lean's internals evolve, and "drift" here means *quietly wrong proofs*. The
charter's first rule — Lean owns Lean semantics — exists to make this
impossible.

### Rejected: thin façade re-exporting raw FFI directly

A crate that re-exports a raw `-sys` crate under a friendlier name and adds
nothing else. Rejected: it pushes the entire initialization, refcount, and
error contract back onto callers. There is no error or panic boundary. The
`pub(crate)` discipline that keeps raw symbols out of the public surface is
defeated by construction. The crate degenerates into raw-FFI-with-extra-steps.

### Rejected: six published crates, one per layer

The original plan — `lean4-sys` → `lean4-runtime` → `lean4-abi` →
`lean4-module` → `lean4-host` plus `lean4-test-support` — published as six
separate Cargo crates. Rejected: this is a fake public-API story. No real
downstream user picks up `lean4-abi` or `lean4-runtime` in isolation; they
take `lean4-host` and the rest comes along. Splitting them across published
crates introduces N `Cargo.toml` entries and N semver surfaces for no caller
benefit, and it cuts against the dominant Rust binding shape
(git2+libgit2-sys, openssl+openssl-sys, z3+z3-sys, rusqlite+libsqlite3-sys,
the pyo3 family), which is consistently two or three crates: a raw `*-sys`
plus one safe front door, plus, in larger stacks, a build helper crate. The
internal *organization* the layer cake encodes — runtime, abi, module, host,
batch, error — is real and worth preserving, but `pub(crate)` modules inside
`lean-rs` police those boundaries at zero semver cost.

### Rejected: external `lean-sys` adoption

`RD-2026-05-17` originally adopted `digama0/lean-sys` for the raw C ABI to
avoid duplicating its ~196 hand-written `extern "C"` declarations.
`RD-2026-05-17-003` reverted that decision: it required ongoing upstream-PR
management for surfaces the published crate did not provide (`LEAN_VERSION`
const, `cargo:rerun-if-changed=lean.h`, signature-checked allowlist, typed
diagnostics), the published `0.0.9` is pinned to a Lean version below our
target, and parallel-copies-plus-upstream was the only path to deliver our
contracts on our timeline. See `RD-2026-05-17-003` in
`prompts/lean-rs/00-current-state.md` for the full reasoning.

### Adopted: in-tree `lean-rs-sys`, `lean-toolchain`, `lean-rs` (all published)

The shape after `RD-2026-05-17-005`:

- `lean-rs-sys` (**published**, per `RD-2026-05-17-005`) for the raw C ABI:
  curated `extern "C"` declarations split by semantic category (`types`,
  `consts`, `refcount`, `object`, `scalar`, `string`, `array`, `nat_int`,
  `closure`, `io`, `init`, `external`), pure-Rust mirrors of `lean.h`'s
  `static inline` refcount helpers via `AtomicI32::from_ptr`, the
  `REQUIRED_SYMBOLS` allowlist, the build-time `LEAN_HEADER_DIGEST`, and
  the link directives. Public types are opaque (`lean_object` is `[u8; 0]
  + PhantomData<(*mut u8, PhantomPinned)>`); the actual `LeanObjectRepr`
  layout is `pub(crate)`. The one crate-wide `#[allow(unsafe_code)]`
  boundary in the workspace; every `unsafe { ... }` block carries a
  `// SAFETY:` comment naming the invariant.

  `RD-2026-05-17-003` originally chose `publish = false` to encode a
  "no raw escape hatch" policy. `RD-2026-05-17-005` reversed that:
  publication matches every peer `*-sys` crate (pyo3-ffi, libgit2-sys,
  openssl-sys, mlua-sys, libz-sys, …) and is friendlier to users who hit
  gaps in `lean-rs`'s safe layer (a stable `*-sys` dependency beats a
  workspace fork). The "no raw escape hatch through `lean-rs`" policy is
  enforced inside `lean-rs` by `pub(crate)` modules around the raw imports,
  independent of `lean-rs-sys`'s publication status.
- `lean-toolchain` (published) for discovery, typed fingerprint, fixture
  digest, layered link diagnostics, and build-script helpers reusable by
  downstream embedders. Composes on top of `lean-rs-sys`'s raw metadata.
- `lean-rs` (published) as the single safe front door, with three publicly
  visible modules (`module`, `host`, `error`) and two `pub(crate)`-only
  infrastructure modules (`runtime`, `abi`). Per `RD-2026-05-17-004`, batch
  and session-pool operations are methods on `LeanSession` rather than a
  separate `batch` module.
- `lean-rs-test-support` (`publish = false`) for fixtures and helpers.

The universal currency inside `lean-rs` is a token-bound object handle:
`pub(crate) runtime::Obj<'lean>` carries a phantom lifetime tied to a
`&'lean LeanRuntime` borrow. Public types built on top — `LeanHost<'lean>`,
`LeanCapabilities<'lean, 'h>`, `LeanSession<'lean, 'c>`, and the semantic
handles (`LeanExpr<'lean>`, `LeanName<'lean>`, …) — propagate the lifetime
so that the type system enforces *init-before-use* and *no value escapes the
runtime*. The pattern is borrowed from PyO3's `Bound<'py, T>`; `LeanRuntime`
plays the role `Python<'py>` plays, except creation is process-once rather
than GIL-scoped. The `'lean` parameter is invisible at typical call sites
(inferred from the runtime borrow) and disappears from `lean_rs::*`
re-exports when bound to `'static`.

This design is deeper than each rejected alternative: fewer caller-facing
details, less temporal coupling (no "call this first" exposed as a safe API),
a small unsafe surface (raw symbols enter only via `lean-rs-sys`, are
exposed only through `pub unsafe fn` helpers over opaque public types, and
live behind `pub(crate)` walls inside `lean-rs`), and a layering invariant
a reviewer can check in one line — `lean-rs-sys → lean-toolchain → lean-rs`.
It matches the dominant Rust binding shape (a raw published `*-sys` plus a
safe front door, plus a build-helper crate where one earns its place), so
contributors arrive with correct expectations, and it contains no Rust-side
dependent-type imitation.

The internal modules give the organizational benefit the layer cake encoded
without the semver and ergonomics tax of intermediate published crates: a
later refactor that moves `lean_rs::module` into `lean_rs::host::module` or
collapses `batch` into `host` requires no consumer change.

See [`04-raw-sys-design.md`](04-raw-sys-design.md) for the per-decision
rationale behind `lean-rs-sys`'s shape (publication status, opaque types,
refcount-mirror strategy, module layout, naming).
