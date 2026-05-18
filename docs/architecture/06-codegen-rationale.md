# Code generation rationale — why `lean-rs` ships no macro for typed exported-function handles

This document records the prompt-19 decision: `lean-rs` does **not** ship a declarative macro for
constructing typed [`LeanExported`](../../crates/lean-rs/src/module/exported.rs) handles. The manual
per-site dispatch shape in `crates/lean-rs/src/host/session.rs` stays. The recovery protocol
(`prompts/lean-rs/00-recovery-protocol.md` — "Code Generation Is Not Earned") explicitly anticipates
this outcome as a local adaptation rather than a contract change.

If a future change wants to revisit the decision, treat it as a re-evaluation of the criteria in §5
("When to revisit"), not as a drift fix.

## 1. Decision

`lean-rs` ships no macro for typed exported-function handles. Every dispatch site in
`host/session.rs` continues to spell out

```rust
let address = self.capabilities.symbols().<field>;
// SAFETY: per the SessionSymbols::resolve invariant; signature is (X) -> Y.
let handle: LeanExported<'lean, '_, (Args,), R> =
    unsafe { LeanExported::from_function_address(self.runtime(), address) };
```

at every call site. The arity-stamping macro that already lives inside
`module::exported` (per `RD-2026-05-17-007`, for the per-arity `.call` impls) is unrelated and stays.

## 2. Scope counted

Production call sites that match the prompt's "module, symbol, arity, conversion" boilerplate:

| Crate / module          | Sites | Notes                                                                 |
| ----------------------- | ----- | --------------------------------------------------------------------- |
| `lean_rs::module`       | 0     | `module` is the loader being wrapped elsewhere; no self-wrapping.     |
| `lean_rs::host::session`| 12    | All construct typed handles via `unsafe { LeanExported::from_function_address(...) }`. |

The twelve `host/session.rs` sites cover `import`, `query_declaration`, `list_declarations`,
`declaration_type`, `declaration_kind`, `declaration_name`, `elaborate`, `kernel_check`,
`check_evidence`, `summarize_evidence`, `run_meta`, and `make_name`.

Test sites (24 in `host/handle/tests.rs`, 43 in `abi/tests.rs`) use the public
`LeanModule::exported::<Args, R>("symbol")` API and are deliberately excluded from the pool:
each test exists to spell out a single `(symbol, Args, R)` triple that is under test. Collapsing
the spelling-out would hide the contract being exercised.

## 3. What the boilerplate is and isn't

Each of the twelve sites is roughly four source lines: an address read, a SAFETY comment, a typed
`LeanExported<...>` annotation, and an `unsafe { from_function_address(...) }` construction. None
of those four pieces is incidental:

- The `LeanExported<'lean, '_, (Args,), R>` type annotation is the safety contract. It is the
  single, greppable, type-checked statement of which Lake-emitted signature the address must
  match. Compressing it inside macro syntax saves no information and makes call sites less
  greppable for queries like "every dispatch returning `LeanIo<Option<...>>`".
- The SAFETY comment recites the signature on purpose. Every comment in `session.rs` of the form
  `// SAFETY: per the SessionSymbols::resolve invariant; signature is (X) -> Y` names the same
  X / Y as the type annotation above it. A macro that templated the comment would either lose the
  per-site signature or duplicate the type annotation as a string — neither helps a reviewer.
- The `unsafe { from_function_address(...) }` block is load-bearing.
  `LeanExported::from_function_address` is intrinsically `unsafe fn` because the cached
  `*mut c_void` carries no signature evidence at the type level. The prompt forbids generating
  unsafe code outside `lean_rs::runtime` and `lean_rs::module`; a macro that satisfies this
  constraint must require the caller to wrap the invocation in `unsafe { ... }`, leaving the
  safety surface unchanged.
- The symbol name appears once per dispatch path — in `SessionSymbols::resolve()` — and the field
  read at the call site uses Rust's existing name resolution. There is no string duplication a
  macro would remove here.

## 4. Macro shape considered and rejected

The smallest declarative macro that would satisfy the prompt's four constraints
("generate typed exported-function handles", "preserve explicit module and symbol names",
"produce clear compile errors for unsupported types", "not generate unsafe code outside
`lean_rs::runtime` and `lean_rs::module` internals") looks roughly like:

```rust
// crates/lean-rs/src/module/exported.rs
macro_rules! lean_dispatch {
    ($runtime:expr, $address:expr, ($($arg:ty),* $(,)?) -> $ret:ty $(,)?) => {{
        // Expansion contains no `unsafe` keyword; the caller wraps the
        // whole invocation in `unsafe { ... }` and writes its own SAFETY
        // comment, preserving the safety obligation at the call site.
        let __handle: $crate::module::LeanExported<'_, '_, ($($arg,)*), $ret> =
            $crate::module::LeanExported::from_function_address($runtime, $address);
        __handle
    }};
}
pub(crate) use lean_dispatch;
```

At a call site:

```rust
let address = self.capabilities.symbols().env_query_declaration;
// SAFETY: per the SessionSymbols::resolve invariant.
let query = unsafe {
    lean_dispatch!(
        self.runtime(),
        address,
        (Obj<'lean>, LeanName<'lean>) -> LeanIo<Option<LeanDeclaration<'lean>>>,
    )
};
```

Compared with the manual form: one line saved per site, ~12 lines saved across `session.rs`. In
exchange the project carries a new declarative macro that:

- has a single expansion shape with one in-tree consumer (`host/session.rs`);
- moves the type signature inside a macro invocation, breaking grep patterns over
  `LeanExported<'lean, '_, (Obj<'lean>, ...), LeanIo<...>>`;
- forces every new dispatch-site reader to first learn what `lean_dispatch!` does before reading
  the call;
- adds no compile-time safety beyond what the existing `LeanArgs` / `DecodeCallResult` / `LeanAbi`
  sealed bounds on `LeanExported::from_function_address` already enforce.

This is exactly the abstraction-without-payoff case CLAUDE.md warns against
("no speculative traits with one implementor"; the same logic generalises to macros) and that
*A Philosophy of Software Design* ch 8 frames as "pulling complexity down should reduce total
complexity, not just relocate it." Here, total complexity rises (one new macro to learn) for a
sub-one-line-per-site cosmetic gain.

## 5. Real duplication this decision does *not* address

The boilerplate that *is* genuinely repeated lives in
`crates/lean-rs/src/host/capabilities.rs` and `crates/lean-rs/src/host/session.rs`'s
`SessionSymbols` struct: the field name, the literal symbol string in
`SessionSymbols::resolve()`, and the docstring table at the top of `session.rs` are three
hand-synchronised views of the same fourteen-entry contract. A change to the contract requires
edits in all three places.

That is registry boilerplate, not "typed exported-function handles", and is out of scope for
prompt 19 by the prompt's own framing. If a future audit decides to attack it, the natural shape
is a single declarative macro that emits the struct, the resolver, and the
`meta_address_by_name` dispatch from a single token-tree — and at that point the dispatch macro
sketched in §4 could ride along.

## 6. When to revisit

This decision should be re-opened when any of the following becomes true:

1. A future prompt adds ≥5 new dispatch sites in `host/session.rs` (or a sibling module) that
   share the same arity and return shape — at which point the per-site savings cross the line
   where the macro pays for itself.
2. Prompt 20's bulk methods or `SessionPool` helper introduce an arity-uniform batch dispatch
   pattern that the manual shape obscures.
3. A larger registry-and-dispatch macro becomes viable that subsumes both the
   `SessionSymbols::resolve()` mapping (see §5) and the per-site typed-handle construction in
   one declaration. Earning the registry macro changes the cost-benefit picture for the
   dispatch macro.
4. The `from_function_address` safety story changes (for example, if a typed
   `SessionSymbols<'lean>` ever carried per-field phantom signatures making the call safe at the
   type level), at which point the per-site SAFETY comments lose their per-site content and a
   macro becomes information-preserving.

Until then, manual typed handles remain the clearer shape.
