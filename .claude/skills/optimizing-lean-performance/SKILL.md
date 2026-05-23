---
name: optimizing-lean-performance
description: 'Use for Lean 4 performance: slow elaboration, heartbeat limits, typeclass loops, simp timeouts, large oleans, slow Lake builds, term bloat, module parallelism.'
---

# Optimizing Lean 4 Performance

Diagnose and fix Lean 4 performance problems: slow elaboration, tactic timeouts, large proof terms, expensive typeclass
synthesis, and slow builds.

**Core principle: measure before changing.** Profile the slow command, identify the bottleneck subsystem (elaboration,
simp, typeclass synthesis, kernel checking), then apply the targeted fix. Do not scatter `maxHeartbeats` increases or
random `simp` rewrites hoping something helps.

## Fast Start

For every performance problem:

1. **Profile** — add `set_option diagnostics true in` before the slow command. This shows heartbeat counts per
   subsystem, which simp lemmas fire, and which typeclass instances are tried.
1. **Classify** — is the bottleneck elaboration, simp, typeclass synthesis, kernel reduction, or build structure?
1. **Read only the reference that matches:**

| Bottleneck | Reference |
| --- | --- |
| Don't know yet / need profiling | `references/profiling-and-diagnostics.md` |
| Slow tactic, simp timeout, heartbeat limit | `references/tactic-performance.md` |
| Large oleans, term bloat, transparency control | `references/term-size-and-transparency.md` |
| Slow build, Lake parallelism, imports, module layout | `references/compilation-and-build.md` |

Do not read reference files speculatively. Profile first, then read the one that matches.

## Decision Tree

```
Slow Lean file
├── Which command is slow?
│   ├── Unknown → set_option diagnostics true, read profiling-and-diagnostics.md
│   ├── A specific theorem/def
│   │   ├── Heartbeat timeout → read tactic-performance.md
│   │   ├── Typeclass synthesis timeout → read tactic-performance.md §Typeclass
│   │   ├── simp timeout or looping → read tactic-performance.md §Simp
│   │   └── Kernel reduction / by rfl slow → read term-size-and-transparency.md
│   └── The whole file / import
│       ├── Large olean → read term-size-and-transparency.md
│       └── Build time → read compilation-and-build.md
```

## Quick Wins

These eight changes fix the majority of Lean 4 performance problems:

1. **Profile first.** `set_option diagnostics true in` on the slow command. Look at heartbeat breakdown before touching
   anything.

1. **Replace bare `simp` with `simp only [...]`.** Use `simp?` to extract the minimal lemma list. Bare `simp` searches
   the entire lemma database via discrimination trees — `simp only` skips that scan.

1. **Use `omega` for linear arithmetic.** It abstracts proofs into auxiliary definitions, keeping proof terms small.
   Faster and produces smaller oleans than `simp` or `decide` for `Nat`/`Int` inequalities.

1. **Mark definitions `noncomputable`** when you do not need executable code. This skips the entire compiler
   code-generation pass.

1. **Provide explicit typeclass instances** when synthesis is slow. A `have` binding or `@[local instance]`
   short-circuits the search.

1. **Use `dsimp` before `simp`** when the goal has definitional equalities. `dsimp` only fires definitional rewrite
   rules (proof is `rfl`), which is cheaper than full `simp`.

1. **Factor large proofs into named helper lemmas.** Each `theorem` gets its own heartbeat budget. The parent theorem
   references the helper by name (opaque), not by inlining the full proof term.

1. **Minimize imports.** Import the most specific module, not a parent barrel. Each unnecessary import adds transitive
   dependencies, typeclass instances, and simp lemmas to the environment.

## Tactic Speed Hierarchy

From fastest to slowest — prefer the cheapest tactic that closes the goal:

| Tactic | Speed | When to use |
| --- | --- | --- |
| `exact`, `apply`, `constructor` | Fastest | Direct term construction |
| `assumption` | Fast | Goal matches a hypothesis |
| `omega` | Fast | Linear `Nat`/`Int` arithmetic |
| `norm_num` | Fast | Numeric normalization |
| `decide` | Medium | Small decidable computations |
| `native_decide` | Fast (large) | Large decidable computations (trusts compiler) |
| `dsimp` | Medium | Definitional simplification only |
| `simp only [...]` | Medium | Constrained rewrite set |
| `simp` | Slow | Full database search |
| `aesop` | Slow | Best-first proof search |

When a proof needs `simp` or `aesop` during exploration, always extract the explicit result with `simp?` or `aesop?`
before committing.

## Key Options Reference

Options you will use most often, with defaults:

```lean
-- Heartbeat budgets
set_option maxHeartbeats 200000        -- per command (0 = unlimited)
set_option synthInstance.maxHeartbeats 20000  -- per typeclass resolution

-- Profiling
set_option diagnostics true            -- subsystem-level counters
set_option profiler true               -- wall-clock per phase
set_option trace.profiler true         -- tactic-level flame graph
set_option trace.profiler.output "profile.json"  -- Firefox Profiler export

-- Typeclass synthesis
set_option synthInstance.maxSize 128   -- max instances in solution chain

-- Simp diagnostics
set_option trace.Meta.Tactic.simp.rewrite true  -- which lemmas fire

-- Reduction control
set_option smartUnfolding true         -- use auxiliary match defs (default)
```

For the complete option catalog and usage patterns, see the reference files.

## KanProofs-Specific Guidance

These patterns apply specifically to the `KanProofs/` formalization:

- **Mutual-inductive judgments** generate complex recursors. Typeclass synthesis over these types can be expensive.
  Provide instances explicitly when the profiler shows synthesis dominating.

- **Mutual-inductive elaboration cost.** Each `mutual` block elaborates all members together. Large mutual blocks (many
  lemmas sharing a mutual recursion) create expensive shared elaboration contexts. Split mutual blocks to contain only
  the lemmas that genuinely need mutual recursion. Use `cases` rather than `induction` at the top level — induction
  motive synthesis on large mutual inductives can blow the heartbeat budget by itself.

- **Binder-depth structural lemmas** often involve nested `Nat` arithmetic. Prefer `omega` over
  `simp [Nat.add_comm, ...]` chains for these goals.

- **Mathlib imports** are the largest contributor to build time. After `git pull` on mathlib, always run
  `lake exe cache get` before building. Import the most specific mathlib module possible.

- **`count_heartbeats in`** (from mathlib) measures actual heartbeat usage. Use it as a regression watermark after
  optimizing a proof.

## Hard Rules

1. **Do not raise `maxHeartbeats` as a first response.** Profile, find the bottleneck, fix it. Only raise the limit if
   the proof is genuinely complex and the heartbeat budget is the actual constraint after optimization.

1. **Do not scatter `@[simp]` attributes to make proofs faster.** Each new simp lemma slows every `simp` call in every
   downstream file. Add simp lemmas only when they belong to a coherent normal form.

1. **Do not use `native_decide` for trust-critical foundations.** It bypasses kernel verification. Use it for large
   concrete computations where the mathematical content is clear.

1. **Do not `set_option maxHeartbeats 0` in committed code.** This disables the timeout entirely. Use a specific
   increased value if genuinely needed.

1. **Do not optimize without measuring the before and after.** Use `count_heartbeats in` or `set_option profiler true`
   to confirm the improvement is real.

## References

- `references/profiling-and-diagnostics.md` — profiler options, trace flags, Firefox Profiler integration, `diagnostics`
  mode, heartbeat counting
- `references/tactic-performance.md` — tactic selection, simp discipline, typeclass synthesis tuning, avoiding
  exponential blowup
- `references/term-size-and-transparency.md` — olean size, proof term bloat, transparency hierarchy, reduction control,
  `noncomputable`
- `references/compilation-and-build.md` — Lake parallelism, caching, import structure, module organization, incremental
  compilation
