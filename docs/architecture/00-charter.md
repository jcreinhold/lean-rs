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

Two published crates plus one external dependency carry the entire public
surface:

- External: [`lean-sys`](https://crates.io/crates/lean-sys), maintained by
  digama0 / Mario Carneiro. Provides the raw C ABI. We do not re-implement it.
- `lean-toolchain` (published). Owns Lean toolchain discovery, version metadata,
  header digest, typed `ToolchainFingerprint`, the curated `lean_*` symbol
  allowlist with link-time verification, layered link diagnostics, and the
  build-script helpers downstream embedders can reuse.
- `lean-rs` (published). The single safe front door for hosting Lean
  capabilities from Rust.

`lean-rs-test-support` is workspace-internal (`publish = false`) and carries
fixtures and helpers; it is not a public surface.

Inside `lean-rs`, the module layout mirrors the original layer story —
`runtime`, `abi`, `module`, `host`, `batch`, `error` — but those boundaries are
policed by `pub(crate)`, not by Cargo crate splits. Re-exports at the crate root
are a curated public API, not a path-shortening facade. A reader of
`lean_rs::*` should see the smallest set of items that lets them call Lean code,
ask semantic questions, and receive typed results.

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
batch calls, and reuse sessions. None of this requires the caller to import
`lean-sys` directly or to know any item in the *hidden knowledge* list above.

## Intentionally discarded behavior

The following are *not* in scope and will not be added:

- Direct application use of raw `lean_*` calls through `lean-rs`. Applications
  that genuinely need the raw C ABI depend on `lean-sys` directly and accept the
  unsafe contract that implies; they do not get there through a `lean-rs`
  re-export.
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

### Rejected: thin façade re-exporting `lean-sys` directly

A crate that re-exports `lean-sys` under a friendlier name and adds nothing
else. Rejected: it pushes the entire initialization, refcount, and error
contract back onto callers. There is no error or panic boundary. The
`pub(crate)` discipline that keeps raw symbols out of the public surface is
defeated by construction. The crate degenerates into `lean-sys`-with-extra-steps.

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

### Adopted: external `lean-sys`, `lean-toolchain`, `lean-rs`

The shape implemented in `RD-2026-05-17`:

- External `lean-sys` (digama0) for the raw C ABI. The workspace does not
  re-implement it.
- `lean-toolchain` (published) for discovery, fingerprint, symbol allowlist,
  layered link diagnostics, and build-script helpers reusable by downstream
  embedders.
- `lean-rs` (published) as the single safe front door, with internal modules
  `runtime`, `abi`, `module`, `host`, `batch`, `error`, all `pub(crate)`.
- `lean-rs-test-support` (`publish = false`) for fixtures and helpers.

This design is deeper than each rejected alternative: fewer caller-facing
details, less temporal coupling (no "call this first" exposed as a safe API),
smaller unsafe surface (raw symbols enter only via `lean-sys` and live behind
`pub(crate)` walls), and a layering invariant a reviewer can check in one line
— `lean-sys (external) → lean-toolchain → lean-rs`. It matches the dominant
Rust binding shape, so contributors arrive with correct expectations, and it
contains no Rust-side dependent-type imitation.

The internal modules give the organizational benefit the layer cake encoded
without the semver and ergonomics tax of intermediate published crates: a
later refactor that moves `lean_rs::module` into `lean_rs::host::module` or
collapses `batch` into `host` requires no consumer change.
