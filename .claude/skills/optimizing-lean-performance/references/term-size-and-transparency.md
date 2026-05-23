# Term Size and Transparency

How to control proof term size, olean bloat, and unwanted kernel reduction.

## Why Term Size Matters

Lean stores proof terms in `.olean` files. Large proof terms cause:

- Slower `lake build` (more data to serialize/deserialize).
- Slower language server (more data to load per import).
- Higher memory usage in the elaborator.

Lean's proof irrelevance means the kernel never _unfolds_ theorem proofs (any two proofs of the same `Prop` are
definitionally equal). But the terms still exist in the olean and must be loaded and stored.

## Tactics That Produce Large Terms

| Tactic | Term size | Why |
| --- | --- | --- |
| `decide` on large inputs | Very large | Kernel evaluation trace of every step |
| `simp` with many rewrites | Large | Chain of `Eq.mpr` / `congrArg` steps |
| `aesop` | Large | Search tree encoded in proof term |
| `omega` | Small | Abstracts proof into auxiliary definition |
| `exact` / `apply` | Minimal | Direct term reference |

## Reducing Term Size

### Use `omega` for linear arithmetic

`omega` abstracts its proof into an auxiliary definition. The parent theorem references this definition by name, not by
inlining the proof. This reduced stdlib's `Vector.Extract` olean from 20 MB to 5 MB in one PR.

### Use `native_decide` for large decidable computations

Instead of a kernel evaluation trace, the proof term is a single opaque call. Tradeoff: trusts the compiler. Appropriate
for concrete computations where the mathematical content is clear.

### Factor proofs into named lemmas

Each `theorem` gets its own proof term. The parent theorem references the helper by name (opaque). Instead of:

```lean
theorem big : P ∧ Q ∧ R := by
  constructor
  · <100 lines of proof for P>
  constructor
  · <100 lines of proof for Q>
  · <100 lines of proof for R>
```

Factor into:

```lean
private theorem big_p : P := by ...
private theorem big_q : Q := by ...
private theorem big_r : R := by ...
theorem big : P ∧ Q ∧ R := ⟨big_p, big_q, big_r⟩
```

Each helper has its own heartbeat budget, and `big`'s proof term is three name references.

### Use `simp?` and `aesop?` to extract explicit proofs

The `?` variants print the proof term or tactic script. Committing the explicit result avoids re-running search and
often produces a smaller term.

## Transparency Hierarchy

Lean has a hierarchy that controls when definitions are unfolded. Choosing the right level prevents unnecessary kernel
work in downstream files.

| Keyword / Attribute | Transparency | Unfolded by | Use for |
| --- | --- | --- | --- |
| `abbrev` | Reducible | Everything (simp, typeclass, kernel) | True synonyms, notation |
| `def` | Semireducible | Explicit `unfold`/`delta` | General definitions |
| `@[irreducible] def` | Irreducible | Nothing (must use API lemmas) | Expensive internals |
| `opaque` | Opaque | Nothing, ever (even kernel) | FFI stubs, axiom-like |
| `theorem` | Proof-irrelevant | Nothing (proof irrelevance) | All proofs |

### When to use `@[irreducible]`

Mark a definition `@[irreducible]` when:

- Downstream files should reason about it via lemmas, not by unfolding.
- The definition involves well-founded recursion (unfolding WF defs forces the kernel to reduce termination proofs).
- The definition is an implementation detail that should not leak into typeclass synthesis or simp.

Since Lean 4.9, functions defined by well-founded recursion are `@[irreducible]` by default. This is intentional — do
not remove it.

### When to use `noncomputable`

Mark a definition `noncomputable` when you do not need to generate executable code. This skips the entire LCNF compiler
pipeline (code generation, optimization, C emission). For mathematical definitions that exist only for proof purposes,
this is free performance.

## Reduction Control Options

For fine-grained control over what the kernel and elaborator reduce:

```lean
-- Smart unfolding: use auxiliary match definitions for structural recursion
-- (default: true). Disabling this forces full reduction of recursive defs.
set_option smartUnfolding true

-- Lazy delta reduction in isDefEq (default: true).
-- Only unfold reducible defs/instances first, then escalate.
set_option backward.isDefEq.lazyWhnfCore true
set_option backward.isDefEq.lazyProjDelta true
```

These defaults are almost always correct. Override them only when profiling identifies a specific unification
bottleneck.

## MetaM Reduction Flags

When writing custom tactics or meta-programs, the `Meta.Config` record controls reduction:

- `iota` (default true) — reduce recursor/matcher applications
- `beta` (default true) — reduce `(fun x => t) a` to `t[a/x]`
- `zeta` (default true) — eliminate let-bindings
- `proj` (default `.yesWithDelta`) — reduce structure projections

Setting `iota := false` or `beta := false` in a custom tactic can prevent expensive reduction during pattern matching,
but breaks normal elaboration. Only use in targeted meta-programs.

## Olean Size Diagnostics

To check olean sizes:

```bash
# Find the largest oleans in the build
find .lake/build -name "*.olean" -exec ls -lS {} + | head -20

# Compare before/after an optimization
ls -la .lake/build/lib/KanProofs/MyModule.olean
```

If an olean is unexpectedly large (>1 MB for a module with few definitions), suspect `decide` on large inputs or
unabstracted `simp` chains.
