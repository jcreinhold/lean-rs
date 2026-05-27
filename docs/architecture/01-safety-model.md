# Safety Model

`lean-rs` is the crate that owns raw Lean ABI operations for the workspace. It may use `unsafe` internally to initialise
the Lean runtime, manage Lean object ownership, decode Lean object layouts, load dynamic symbols, and bridge callbacks.
Safe public APIs in `lean-rs` are allowed only when the crate enforces the relevant safety invariants itself.

This is a Rust memory-safety claim for safe Rust APIs. It is not a claim that Lean code is semantically correct, that a
proof term proves what the user intended, that user-authored Lean code terminates, or that Lean's kernel/elaborator
never rejects the input.

`lean-rs-host` is outside that trusted implementation boundary. It forbids unsafe Rust code crate-wide and consumes
`lean-rs` through safe handles, safe ABI conversion/view APIs, and manifest-checked host-shim bindings. Bundled host
shims are resolved once through trusted export signature metadata and cached as typed `HostShimBindings`; public
`LeanSession` methods dispatch only those closed host operations.

Every change that adds an unsafe block, wrapper type, FFI call, or concurrency claim must be consistent with the rules
below. An API that cannot be made consistent does not ship as safe.

## Unsafe boundary

Raw `lean_*` symbols enter the workspace only through `lean-rs-sys` (published). Its public types are opaque:
`lean_object` is `[u8; 0] + PhantomData<(*mut u8, PhantomPinned)>`, and downstream code reaches object state only
through `pub unsafe fn` helpers. Lean's header layout (`LeanObjectRepr`) is `pub(crate)`.

Inside `lean-rs`, imports of `lean_rs_sys` live in the runtime, ABI conversion, module loading/dispatch, callback, and
error-decoding internals and are never re-exported. Every public safe function of `lean-rs` must either avoid raw Lean
state or fully enforce the needed ownership, lifetime, layout, and ABI-signature invariants before returning. A reader
of `lean_rs::*` should not have to choose a refcount discipline, inspect a Lean object header, or cast a symbol address
to a C function pointer to use the safe surface.

Checked export lookup is safe only when backed by trusted signature metadata, such as a build manifest generated for a
known capability contract. `LeanModule::exported_unchecked::<Args, R>(name)` is unsafe because arbitrary dynamic export
lookup cannot be validated from a raw symbol name plus caller-chosen `Args`/`R`. The call is memory-safe only if the
symbol's compiled C ABI is known to match those Rust types. Raw dynamic-loader symbol addresses and `lean_rs_sys`
remain implementation details of `lean-rs`.

Applications that genuinely need raw FFI opt in by depending on `lean-rs-sys` directly, accepting full `unsafe`
discipline (per-block `// SAFETY:`, per-fn `# Safety` doc) and opaque public types—friendlier than forking the
workspace, and the same trade every peer `*-sys` crate makes.

## Reference counting

Lean's per-thread runtime model means refcount adjustments cannot escape the safe layer.
`pub(crate) runtime::obj::Obj<'lean>` owns one refcount; `pub(crate) runtime::obj::ObjRef<'lean, 'a>` is a borrowed view
tied to its source's lifetime and the runtime borrow. `Obj<'lean>` releases its reference on `Drop`; `Clone` performs
`lean_inc` via the Rust mirror in `lean-rs-sys`; `ObjRef` performs no RC adjustments on its own.

The public surface never accepts or returns raw `lean_obj_arg`, `b_lean_obj_arg`, or `lean_obj_res`. A caller of
`lean-rs` does not need to know what `lean_inc` and `lean_dec` are. If a future API would force the caller to choose a
refcount discipline, that is a charter violation, not an option.

The former host ownership/drop leakage is closed for argument-only `LeanAbi` impls: host types name the boxed C-ABI
representation through `Obj`'s associated `LeanAbi` type and consume impossible owned returns through `Obj::from_c`.

## Proof objects

Proof-related results cross into Rust as opaque handles or Lean-authored summaries. Rust does not reconstruct proof
terms, inspect their structure, or re-derive a kernel judgement; the kernel is in Lean.

A handle's only safe public operations are the ones Lean explicitly exposes through a capability—"ask Lean to print this
proof's type" is fine if Lean offers it; "walk the proof's expression tree in Rust" is not.

The same rule applies to host diagnostic and query payloads: Rust may decode a Lean-authored summary shape, but raw
constructor layout knowledge should be hidden in `lean-rs` conversion/view APIs rather than duplicated in
`lean-rs-host`.

## Concurrency

The Lean runtime is per-thread (`lean_initialize_thread` / `lean_finalize_thread`), so every Lean-derived handle is
`!Send + !Sync` by default. Treating a Lean object as freely portable is a soundness hazard. Opting any handle type into
`Send` or `Sync` is a per-type contract change that must be justified against the per-thread model.

See [`04-concurrency.md`](04-concurrency.md) for the full thread-attach discipline and the async-embedding pattern.

## Workspace lint policy

`unsafe-code = "deny"` at the workspace level (set in `Cargo.toml` `[workspace.lints.rust]`).

Per-file opt-outs require, in order:

1. **`#[allow(unsafe_code)]` at the smallest scope that compiles**—a module, not a crate. PR description names the
   reason. A blanket allow at the crate root is rejected.
2. **A `// SAFETY:` comment on every `unsafe { ... }` block** naming the invariant the caller or surrounding context
   relies on. "Calls into `lean-rs-sys`" is not a safety comment; "the runtime is initialized on this thread and `obj`
   is the unique owner per `Obj<'lean>`'s `Drop`" is.
3. **A test that would fail under a plausible violation** when practical—Miri on the Rust side of the boundary (Miri
   cannot validate the Lean C runtime itself), a sanitizer build, a refcount stress test, or a focused unit test on the
   unsafe seam.
4. **Reviewer sign-off from someone other than the author.** Self-merging a new `unsafe` block is not allowed.

## Panic discipline

Rust panics must not unwind across a C or Lean frame. The `error` module of `lean-rs` is the typed conversion point for
Rust-owned boundaries: it catches Rust panics before they cross into Lean callbacks, converts Lean `IO` exceptions to
typed Rust errors, and converts ABI-shape violations to typed errors. It is not a Lean-runtime panic recovery layer. A
Lean internal panic, generated `unreachable`, `std::exit`, or `abort` during a `LeanSession` call may terminate the
process; see [`06-panic-containment.md`](06-panic-containment.md). No `unwrap()`, `expect()`, or `panic!` in non-test
code unless a comment names a proof obligation that makes the call infallible.

Lean-to-Rust callbacks go through `LeanCallbackHandle`, not raw user-provided function pointers. The registry catches
unwinding Rust panics before the C boundary and reports them as callback status plus a bounded `LeanError`; see
[`10-callback-registry.md`](10-callback-registry.md).
