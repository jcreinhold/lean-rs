# Codegen Rationale—Why `lean-rs` Ships No Macro for Typed Exported-Function Handles

`lean-rs` ships **no** declarative macro for constructing typed
[`LeanExported`](../../crates/lean-rs/src/module/exported.rs) handles. The manual per-site
dispatch shape in `crates/lean-rs-host/src/host/session.rs` stays.

Revisiting the decision is a re-evaluation of the criteria in §5 ("When to revisit").

## 1. Decision

Every dispatch site in `host/session.rs` continues to spell out:

```rust
let address = self.capabilities.symbols().<field>;
// SAFETY: per the SessionSymbols::resolve invariant; signature is (X) -> Y.
let handle: LeanExported<'lean, '_, (Args,), R> =
    unsafe { LeanExported::from_function_address(self.runtime(), address) };
```

The arity-stamping macro inside `module::exported` (which generates the per-arity `.call`
impls) is unrelated and stays.

## 2. Scope counted

Twelve production sites match the "module, symbol, arity, conversion" boilerplate shape, all
in `crates/lean-rs-host/src/host/session.rs`: `import`, `query_declaration`,
`list_declarations`, `declaration_type`, `declaration_kind`, `declaration_name`, `elaborate`,
`kernel_check`, `check_evidence`, `summarize_evidence`, `run_meta`, `make_name`. `lean_rs::module`
has none—the loader being wrapped doesn't self-wrap.

Test sites (24 in `crates/lean-rs/src/handle/tests.rs`, 43 in `crates/lean-rs/src/abi/tests.rs`)
use the public `LeanModule::exported::<Args, R>("symbol")` and are excluded: each test exists to
spell out a single `(symbol, Args, R)` triple under test. Collapsing the spelling-out would
hide the contract being exercised.

## 3. What the boilerplate is and isn't

Each site is roughly four source lines: an address read, a SAFETY comment, a typed
`LeanExported<...>` annotation, and an `unsafe { from_function_address(...) }` construction.
None of the four is incidental:

- The `LeanExported<'lean, '_, (Args,), R>` annotation is the safety contract—the single, greppable, type-checked statement of which Lake-emitted signature the address must match. Compressing it inside macro syntax saves no information and makes call sites less greppable for queries like "every dispatch returning `LeanIo<Option<...>>`".
- The SAFETY comment recites the signature on purpose. Every comment in `session.rs` of the form `// SAFETY: per the SessionSymbols::resolve invariant; signature is (X) -> Y` names the same X / Y as the type annotation above it. A macro that templated the comment would either lose the per-site signature or duplicate the annotation as a string.
- The `unsafe { from_function_address(...) }` block is load-bearing. `LeanExported::from_function_address` is intrinsically `unsafe fn` because the cached `*mut c_void` carries no signature evidence at the type level. The prompt forbids generating unsafe code outside `lean_rs::runtime` and `lean_rs::module`; a macro that satisfies this constraint must require the caller to wrap the invocation in `unsafe { ... }`, leaving the safety surface unchanged.
- The symbol name appears once per dispatch path—in `SessionSymbols::resolve()`—and the field read uses Rust's existing name resolution. There is no string duplication a macro would remove.

## 4. Macro shape considered and rejected

The smallest declarative macro that would satisfy the prompt's four constraints—generate
typed handles, preserve explicit module and symbol names, produce clear compile errors for
unsupported types, generate no unsafe code outside `lean_rs::runtime` / `lean_rs::module`
internals—looks roughly like:

```rust
// crates/lean-rs/src/module/exported.rs
macro_rules! lean_dispatch {
    ($runtime:expr, $address:expr, ($($arg:ty),* $(,)?) -> $ret:ty $(,)?) => {{
        // Expansion contains no `unsafe` keyword; the caller wraps the
        // whole invocation and writes its own SAFETY comment.
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

Compared with the manual form: one line saved per site, ~12 lines across `session.rs`. In
exchange the project carries a declarative macro that has one in-tree consumer, breaks grep
over `LeanExported<'lean, '_, (Obj<'lean>, ...), LeanIo<...>>`, forces every new dispatch-site
reader to first learn `lean_dispatch!`, and adds no compile-time safety beyond what the
existing `LeanArgs` / `DecodeCallResult` / `LeanAbi` sealed bounds already enforce.

This is the abstraction-without-payoff case `CLAUDE.md` warns against ("no speculative traits
with one implementor"; the same logic generalises to macros) and Ousterhout ch. 8 frames as
"pulling complexity down should reduce total complexity, not just relocate it." Here total
complexity rises (one new macro to learn) for a sub-one-line-per-site cosmetic gain.

## 5. Real duplication this decision does not address

The genuinely repeated boilerplate lives in `crates/lean-rs-host/src/host/capabilities.rs` and
`SessionSymbols` in `crates/lean-rs-host/src/host/session.rs`: field name, literal symbol
string in `SessionSymbols::resolve()`, and the docstring table at the top of `session.rs` are
three hand-synchronised views of the same fourteen-entry contract. Changing the contract means
edits in all three places.

That is registry boilerplate, not "typed exported-function handles", and out of scope for
this document. If a future audit attacks it, the natural shape is a single declarative macro
that emits the struct, the resolver, and the `meta_address_by_name` dispatch from one
token-tree—and at that point the dispatch macro sketched in §4 could ride along.

## 6. When to revisit

Re-open when any of these becomes true: ≥5 new dispatch sites in `host/session.rs` (or a
sibling module) share the same arity and return shape, crossing the line where the macro pays
for itself; if bulk methods or a `SessionPool` helper later introduce an arity-uniform batch
pattern the manual shape obscures; a larger registry-and-dispatch macro becomes viable that
subsumes both the `SessionSymbols::resolve()` mapping (see §5) and per-site typed-handle
construction in one declaration; or the `from_function_address` safety story changes—for
example, a typed `SessionSymbols<'lean>` carrying per-field phantom signatures making the call
safe at the type level—at which point the per-site SAFETY comments lose their per-site
content and a macro becomes information-preserving.

Until then, manual typed handles remain the clearer shape.
