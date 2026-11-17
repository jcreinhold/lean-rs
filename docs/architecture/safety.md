# Safety Model

`lean-rs` owns the safe Rust boundary around Lean runtime initialization, object ownership, ABI conversion, dynamic
loading, and callbacks. A safe API must enforce the ownership, lifetime, layout, and signature invariants it needs. This
is a Rust memory-safety contract; Lean still owns elaboration, kernel checking, and the meaning of proof terms.
Termination and recovery from fatal Lean failures require separate resource and process boundaries.

`lean-rs-host` forbids unsafe Rust crate-wide. It consumes safe handles, conversion/view APIs, and manifest-checked
`HostShimBindings` from `lean-rs`; its session methods dispatch a closed set of host operations.

## Unsafe boundary

Raw `lean_*` symbols enter through `lean-rs-sys`. Its public FFI types are opaque: `lean_object` has no accessible
fields, and `LeanObjectRepr` and the other layout mirrors remain `pub(crate)`. Raw consumers depend on `lean-rs-sys`
directly and accept its unsafe preconditions. The safe `lean-rs` surface never re-exports raw symbols or requires
callers to inspect object headers, manage reference counts, or cast symbol addresses.

Safe export lookup requires trusted signature metadata, such as a build manifest for a known capability contract.
`LeanModule::exported_unchecked::<Args, R>(name)` is unsafe: a symbol name and caller-selected Rust types do not prove
that the compiled C ABI matches. Manifest and loader checks are described in
[`loading.md`](loading.md).

The current public hidden `LeanAbi` conversion hooks do not fully enforce the intended boundary: `Obj`'s safe `from_c`
implementation accepts a raw C representation without a caller-side unsafe requirement. The sibling-crate marker
convention also does not restrict implementations through Rust visibility. These are unresolved implementation gaps, not
exceptions to the required contract above. The [unsafe inventory](../safety/unsafe-inventory.md) records the source
locations and required follow-up; documentation cleanup does not repair that source boundary.

## Object ownership and lifetime

[`Obj<'lean>`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/src/runtime/obj.rs) owns one Lean reference; `Clone` increments it and `Drop`
releases it. `ObjRef<'lean, 'a>` borrows from its source without independently adjusting the reference count. Both carry
the runtime lifetime. The raw ABI ownership aliases (`lean_obj_arg`, `b_lean_obj_arg`, `lean_obj_res`) stay below the
safe boundary.

Semantic handles and host-defined evidence wrap those objects. Rust may decode Lean-authored summaries and transport
opaque handles; it must not reconstruct a kernel judgment or duplicate Lean's proof semantics. Constructor layout
knowledge belongs in `lean-rs` conversion/view APIs rather than in the host service layer.

## Concurrency

Lean-derived handles inherit `!Send + !Sync` from `LeanRuntime`. Opting a handle into either trait is a per-type
contract change requiring justification against Lean's thread and reference-count rules. Initialize and use runtime
handles on the calling thread; use `LeanThreadGuard` to scope explicit attach/detach. See
[`concurrency.md`](concurrency.md) for the precise contract and async embedding pattern.

## Unsafe-code review

The workspace sets `unsafe_code = "deny"`. `lean-rs-sys` is the single crate-wide opt-out. Elsewhere, new unsafe code
requires:

1. `#[allow(unsafe_code)]` at the smallest scope that compiles, with the reason in the PR description; no blanket
   crate-root allow.
2. A `// SAFETY:` comment on each unsafe block naming the invariant it relies on, and a `# Safety` section on each
   public unsafe function stating its preconditions. Merely naming the FFI crate is insufficient.
3. A check that would catch a plausible violation when practical: a focused boundary test, refcount stress test,
   sanitizer run, or Miri test for Rust-only behavior.
4. Reviewer sign-off from someone other than the author before merge.

The [sanitizer workflow](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/sanitizer.yml) runs Miri on the pure-Rust `lean-rs-sys` boundary with
`-Zmiri-tree-borrows`; FFI tests are ignored there. Its ASan job covers Rust-only `lean-toolchain` helpers. It does
**not** instrument in-process Lean tests: the uninstrumented Lean shared runtime currently crashes that configuration
before producing useful Rust diagnostics. Loader regressions, ordinary workspace tests, worker containment tests, and
manual ABI fuzzing cover the remaining paths; none substitutes for reviewing the unsafe invariant itself.

## Panic discipline

Rust unwinds must not cross C or Lean frames. Callback trampolines catch unwinding Rust panics and return a callback
status plus a bounded `LeanError`. Lean `IO` exceptions and ABI-shape failures also use typed Rust errors.

Lean internal panics, aborting Rust panics, generated unreachable paths, foreign unwinds, `std::exit`, and `abort` have
no in-process recovery guarantee. Use the worker process boundary when the application must survive them; see
[`worker-processes.md`](worker-processes.md).

Non-test code must not use `unwrap()`, `expect()`, or `panic!` unless a comment states the proof obligation making the
operation infallible. Lean-to-Rust callbacks use `LeanCallbackHandle`, with the lifetime and reentrancy rules in
[`callbacks.md`](callbacks.md).
