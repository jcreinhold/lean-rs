# Safety Model

This document states the safety thesis the `lean-rs` workspace is built to honour.
Every later prompt that adds an unsafe block, a wrapper type, an FFI call, or a
concurrency claim must be consistent with the rules here. If a planned API cannot
be made consistent with this thesis, the right move is a Replanning Delta under
`00-recovery-protocol.md`, not a weaker version of the thesis.

## Unsafe boundary thesis

Raw `lean_*` symbols enter the workspace only through the in-tree workspace
crate `lean-rs-sys` (`publish = false`). Inside `lean-rs`, every import of a
`lean_rs_sys` item lives in a `pub(crate)` module and is never re-exported
through the public API. Every public function of `lean-rs` and `lean-toolchain`
is safe Rust.

A consequence: a reader of `lean_rs::*` cannot acquire a raw Lean pointer
through the public API at all, and because `lean-rs-sys` is `publish = false`
they cannot reach raw symbols by adding a direct dependency either. An
application that genuinely needs raw FFI must contribute the missing
capability to `lean-rs` or fork the workspace.

## Reference counting

Safe APIs own all `lean_inc` / `lean_dec` calls. The public surface never
accepts or returns a raw `lean_obj_arg`, `b_lean_obj_arg`, or `lean_obj_res`;
owned and borrowed obligations are encoded with the safe wrapper types
`LeanObj` (owned) and `BorrowedLeanObj<'a>` (borrowed), which will be added by
prompt 07 under the `OBJECT-MEMORY` contract. `LeanObj` releases its
reference on `Drop`; `Clone` performs `lean_inc`. `BorrowedLeanObj<'a>` is a
view tied to its source's lifetime and performs no RC adjustments on its own.

A caller of `lean-rs` does not need to know what `lean_inc` and `lean_dec`
are. If a future API would force the caller to choose a refcount discipline,
that is a charter violation, not an option.

## Proof objects

Proof-related results cross into Rust as opaque handles or as Lean-authored
summaries. Rust does not reconstruct proof terms, inspect their structure, or
re-derive a kernel judgement; the kernel is in Lean.

A handle's only safe public operations are the ones Lean explicitly exposes
through a capability — for example, "ask Lean to print this proof's type" is
fine if Lean offers that, "walk the proof's expression tree in Rust" is not.

## Concurrency

`Send` and `Sync` are denied by default on Lean-derived handles. The Lean
runtime is per-thread (`lean_initialize_thread` / `lean_finalize_thread`), and
treating a Lean object as freely movable between OS threads is a soundness
hazard, not an ergonomic choice. Opting any handle type into `Send` or `Sync`
is a per-type contract change, recorded in `00-current-state.md` and justified
against the Lean runtime's per-thread model.

## Workspace lint policy

`unsafe-code = "deny"` at the workspace level. This is already set in
`Cargo.toml` `[workspace.lints.rust]` (see `CI-LINT-BASELINE` in
`00-current-state.md`).

Per-file opt-outs require, in this order:

1. `#[allow(unsafe_code)]` at the smallest scope that compiles — a module, not
   a crate — with the PR description naming the reason. A blanket allow at the
   crate root is rejected.
2. A `// SAFETY:` comment on every `unsafe { ... }` block naming the invariant
   the caller (or the surrounding context) is relying on. "Calls into
   `lean-rs-sys`" is not a safety comment; "the runtime is initialized on this
   thread and `obj` is the unique owner per `LeanObj`'s `Drop`" is.
3. A test that would fail under a plausible violation of that invariant when
   practical — Miri on the Rust side of the boundary (Miri cannot validate the
   Lean C runtime itself), a sanitizer build, a refcount stress test, or a
   focused unit test on the unsafe seam.
4. Reviewer sign-off from someone other than the author. Self-merging a new
   `unsafe` block is not allowed.

## Panic discipline

Rust panics must not unwind across a C or Lean frame. The `error` module of
`lean-rs` (to be filled by prompt 10 under `ERROR-BOUNDARY`) is the typed
conversion point: it catches panics at the FFI boundary, converts Lean
exceptions to typed Rust errors, and converts ABI-shape violations to typed
errors as well. No `unwrap()`, `expect()`, or `panic!` in non-test code unless
a comment names a proof obligation that makes the call infallible.
