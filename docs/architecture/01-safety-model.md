# Safety Model

The safety thesis the `lean-rs` workspace is built to honour. Every change that adds an
unsafe block, wrapper type, FFI call, or concurrency claim must be consistent with the rules
here. If a planned API cannot be made consistent, stop and rethink—don't weaken the thesis.

## Unsafe boundary

Raw `lean_*` symbols enter the workspace only through `lean-rs-sys` (published). Its public types are opaque: `lean_object` is
`[u8; 0] + PhantomData<(*mut u8, PhantomPinned)>`, and downstream code reaches object state only
through `pub unsafe fn` helpers. Lean's header layout (`LeanObjectRepr`) is `pub(crate)`.

Inside `lean-rs`, every import of a `lean_rs_sys` item lives in a `pub(crate)` module of
`runtime` and is never re-exported. Every public function of `lean-rs` and `lean-toolchain` is
safe Rust. A reader of `lean_rs::*` cannot acquire a raw Lean pointer through the public API.

Applications that genuinely need raw FFI opt in by depending on `lean-rs-sys` directly,
accepting full `unsafe` discipline (per-block `// SAFETY:`, per-fn `# Safety` doc) and opaque
public types—friendlier than forking the workspace, and the same trade every peer `*-sys`
crate makes.

## Reference counting

Lean's per-thread runtime model means refcount adjustments cannot escape the safe layer.
`pub(crate) runtime::obj::Obj<'lean>` owns one refcount; `pub(crate) runtime::obj::ObjRef<'lean, 'a>`
is a borrowed view tied to its source's lifetime and the runtime borrow. `Obj<'lean>` releases
its reference on `Drop`; `Clone` performs `lean_inc` via the Rust mirror in `lean-rs-sys`;
`ObjRef` performs no RC adjustments on its own.

The public surface never accepts or returns raw `lean_obj_arg`, `b_lean_obj_arg`, or
`lean_obj_res`. A caller of `lean-rs` does not need to know what `lean_inc` and `lean_dec` are.
If a future API would force the caller to choose a refcount discipline, that is a charter
violation, not an option.

## Proof objects

Proof-related results cross into Rust as opaque handles or Lean-authored summaries. Rust does
not reconstruct proof terms, inspect their structure, or re-derive a kernel judgement; the
kernel is in Lean.

A handle's only safe public operations are the ones Lean explicitly exposes through a
capability—"ask Lean to print this proof's type" is fine if Lean offers it; "walk the proof's
expression tree in Rust" is not.

## Concurrency

The Lean runtime is per-thread (`lean_initialize_thread` / `lean_finalize_thread`), so
Lean-derived handles must not move between OS threads. `Send` and `Sync` are denied by default
on every such handle: treating a Lean object as freely portable is a soundness hazard, not an
ergonomic choice. Opting any handle type into `Send` or `Sync` is a per-type contract change that must be
justified against the per-thread model.

See [`04-concurrency.md`](04-concurrency.md) for the full thread-attach discipline and the
async-embedding pattern.

## Workspace lint policy

`unsafe-code = "deny"` at the workspace level (set in `Cargo.toml` `[workspace.lints.rust]`).

Per-file opt-outs require, in order:

1. **`#[allow(unsafe_code)]` at the smallest scope that compiles**—a module, not a crate. PR description names the reason. A blanket allow at the crate root is rejected.
2. **A `// SAFETY:` comment on every `unsafe { ... }` block** naming the invariant the caller or surrounding context relies on. "Calls into `lean-rs-sys`" is not a safety comment; "the runtime is initialized on this thread and `obj` is the unique owner per `Obj<'lean>`'s `Drop`" is.
3. **A test that would fail under a plausible violation** when practical—Miri on the Rust side of the boundary (Miri cannot validate the Lean C runtime itself), a sanitizer build, a refcount stress test, or a focused unit test on the unsafe seam.
4. **Reviewer sign-off from someone other than the author.** Self-merging a new `unsafe` block is not allowed.

## Panic discipline

Rust panics must not unwind across a C or Lean frame. The `error` module of `lean-rs` is the
typed conversion point for Rust-owned boundaries: it catches Rust panics before they cross into
Lean callbacks, converts Lean `IO` exceptions to typed Rust errors, and converts ABI-shape
violations to typed errors. It is not a Lean-runtime panic recovery layer. A Lean internal
panic, generated `unreachable`, `std::exit`, or `abort` during a `LeanSession` call may
terminate the process; see [`06-panic-containment.md`](06-panic-containment.md). No `unwrap()`,
`expect()`, or `panic!` in non-test code unless a comment names a proof obligation that makes
the call infallible.
