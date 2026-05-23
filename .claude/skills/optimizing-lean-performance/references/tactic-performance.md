# Tactic Performance

How to choose fast tactics and avoid common performance traps.

## Tactic Selection Order

When multiple tactics can close a goal, prefer the cheapest one. This ordering reflects actual cost, not generality:

### Tier 1: Direct Construction (microseconds)

- **`exact e`** — supplies a term directly. No search.
- **`apply f`** — unifies the goal with `f`'s conclusion. One unification.
- **`constructor`** — applies the unique constructor. One lookup + unification.
- **`assumption`** — linear scan of the local context.
- **`rfl`** — kernel definitional equality check. Fast unless the terms are large and require deep reduction (see
  "Expensive `rfl`" below).

### Tier 2: Decision Procedures (milliseconds)

- **`omega`** — linear integer/natural arithmetic. Fast and produces small proof terms (abstracts into auxiliary
  definitions). Preferred over `simp` or `decide` for `Nat`/`Int` inequality goals.
- **`norm_num`** — extensible numeric normalization. Detects non-applicability early and exits fast.
- **`decide`** — evaluates a `Decidable` instance in the kernel. Fine for small computations (e.g., `2 + 2 = 4`, small
  list membership). Becomes expensive for large computations because the kernel must evaluate every step.
- **`native_decide`** — compiles the `Decidable` instance to native code. Fast for large computations but trusts the
  compiler (bypasses kernel verification). Do not use for trust-critical foundations.

### Tier 3: Rewriting (milliseconds to seconds)

- **`dsimp`** / **`dsimp only [...]`** — applies only definitional rewrite rules (proof is `rfl`). Cheaper than `simp`
  because it skips the discrimination tree lookup for non-definitional lemmas.
- **`simp only [lemma₁, lemma₂, ...]`** — applies a specific lemma set. Skip discrimination tree scan of the full
  database.
- **`rw [lemma]`** / **`rewrite [lemma]`** — single directed rewrite. Cheaper than `simp` when you know exactly which
  lemma to apply.

### Tier 4: Search (seconds)

- **`simp`** — searches the entire simp lemma database. Cost scales with database size and term complexity. Always
  convert to `simp only [...]` via `simp?` before committing.
- **`aesop`** — best-first proof search over registered rules. Use `aesop?` to extract the explicit proof.
- **`convert e`** — creates unification subgoals. Prefer `refine`/`exact` after manual `rw` steps.

## Simp Discipline

Bare `simp` is the most common cause of slow proofs. Follow these rules:

1. **Use `simp?` to extract `simp only [...]`.** Always do this before committing. The explicit lemma list is faster (no
   database scan) and stable across mathlib versions.

1. **Use `dsimp` before `simp`** when the goal has definitional equalities. This handles the cheap rewrites first,
   leaving `simp` with a simpler goal.

1. **Keep simp lemma lists minimal.** Every lemma in `simp only [...]` is tried against every subexpression. Ten
   unnecessary lemmas can double the cost.

1. **Avoid `simp` in a loop.** `simp` inside `repeat`, `iterate`, or a recursive tactic compounds the cost. Use `simp`
   once, then switch to targeted rewrites.

1. **Use `simp (config := { singlePass := true })`** when you suspect looping. Single-pass mode applies each rule at
   most once per subexpression.

1. **Do not add `@[simp]` attributes casually.** Each new simp lemma slows every bare `simp` call in every downstream
   file. A simp lemma should belong to a coherent normal form — it should always simplify, never just rewrite to an
   equivalent form.

## Typeclass Synthesis

Typeclass inference can be the dominant cost in files with deep algebraic hierarchies (common with mathlib).

### Diagnosing slow synthesis

```lean
set_option trace.Meta.synthInstance true in
set_option synthInstance.maxHeartbeats 20000 in  -- default
```

Look for:

- The same goal attempted many times (indicates backtracking).
- Long instance chains (10+ steps to resolve).
- Instances that match the head but fail deep in unification.

### Fixing slow synthesis

1. **Provide the instance explicitly.** A `have` or `let` binding short-circuits the search:

    ```lean
    have : Monoid α := inferInstance  -- resolved once, reused below
    ```

1. **Use `@[local instance]`** to add a fast-path instance for a specific file without polluting the global database.

1. **Cap synthesis budgets locally** when a proof touches a pathological hierarchy:

    ```lean
    set_option synthInstance.maxHeartbeats 10000 in
    set_option synthInstance.maxSize 400 in
    ```

1. **Reduce instance search depth.** If the profiler shows synthesis trying instances that clearly cannot apply, the
   hierarchy may need restructuring (this is a larger fix).

## Expensive `rfl`

`by rfl` (or `rfl` as a term) asks the kernel to check definitional equality by normalizing both sides. If the terms
involve:

- Well-founded recursion (WF termination proofs are unfolded)
- Large nested structures
- Deep computation chains

then `rfl` can be very expensive. Alternatives:

- **`native_decide`** for concrete decidable equalities.
- **`omega`** for arithmetic equalities.
- **`simp only [...]`** to rewrite to a common form, then close with `rfl` on simpler terms.
- **Mark the definition `@[irreducible]`** so the kernel cannot unfold it during `rfl`. Provide API lemmas instead.

## Mutual-Inductive Proofs

Mutual inductives create performance traps that do not arise with ordinary inductive types.

### `cases` vs `induction`

For commutation lemmas over mutual inductives (e.g., weakening commutes with substitution), prefer `cases` at the top
level with explicit recursive calls to the theorem itself (which Lean's `mutual` block handles). Using `induction`
forces Lean to synthesize a motive over the full mutual inductive, which can be very expensive — sometimes the motive
synthesis alone exceeds the heartbeat budget.

### Mutual block sizing

Each `mutual` block elaborates all its members together in a shared context. If a mutual block contains 20 lemmas but
only 5 genuinely need mutual recursion, the other 15 pay the elaboration cost for nothing.

Split mutual blocks to contain only the lemmas that need each other. If `weakenAt_substAt` for ValueTerm, CompTerm,
ValueType, and CompType form a genuine mutual family, put them in their own block — not alongside unrelated lemmas.

### Per-constructor simp

Never pass a recursive function name directly to `simp` (e.g., `simp [substAt]`). This unfolds the function one step,
produces a large goal that `simp` cannot close, and the failed rewrite still costs heartbeats for the whnf check.
Instead, prove per-constructor `@[simp]` lemmas (`substAt_boundVar`, `substAt_pair`, etc.) and use `simp only` with
those.

## `nontriviality` and Other Slow Tactics

Some tactics are convenience wrappers with high overhead:

- **`nontriviality`** — replace with `rcases subsingleton_or_nontrivial α`.
- **`field_simp`** — can be expensive on complex fraction goals. Consider manual `rw` with specific lemmas.
- **`ring`** — generally fast, but can slow down on very large expressions. Factor the expression into named subterms.

## Avoiding Exponential Blowup

The most common causes of exponential behavior:

1. **Typeclass backtracking.** An instance matches the head, fails deep in unification, backtracks, and tries the next
   instance — which also fails. Fix: provide the instance explicitly.

1. **Simp looping.** A pair of lemmas rewrites back and forth: `a = f(a)` and `f(a) = a`. Fix: orient lemmas
   consistently, or use `simp only` with one of them.

1. **Unification with large terms.** `isDefEq` on terms with shared subexpressions can explore an exponential number of
   paths if sharing is lost. Fix: factor into `let` bindings to preserve sharing.

1. **Recursive tactic combinators.** `repeat (simp; ring)` can run indefinitely. Use `iterate N` with a bound, or
   restructure.
