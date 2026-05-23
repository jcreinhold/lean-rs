# Compilation and Build Performance

How to organize Lean 4 code and Lake builds for fast compilation.

## Lake Build Parallelism

Lake's unit of compilation is a single `.lean` file. Files that do not depend on each other build in parallel.

```bash
# Explicit parallelism (N = number of parallel tasks)
lake build -j8

# Use all available cores
lake build -j$(nproc)
```

The `LEAN_NUM_THREADS` environment variable controls the Lean runtime's internal thread pool (defaults to logical CPU
count). This affects within-file parallelism for things like async elaboration.

### Maximizing build parallelism

The build is as fast as its longest serial chain (critical path). To shorten the critical path:

1. **Prefer wide, shallow dependency graphs.** If module A imports B which imports C which imports D, that is a 4-deep
   serial chain. If instead A, B, C each import D directly, they build in parallel after D finishes.

1. **Split large files.** A 2000-line file is one serial compilation unit. If its theorems are independent, split into
   focused modules that build in parallel.

1. **Put expensive definitions early.** Place costly elaborations (large mutual inductives, complex typeclass
   hierarchies) in leaf modules. They build once, get cached, and downstream consumers compile quickly.

1. **Minimize import depth.** Every `import` adds a sequential dependency. Import the most specific module possible.

## Import Discipline

Each import pulls in the full transitive closure of dependencies. This means:

- All typeclass instances from all transitively imported modules enter the synthesis database.
- All `@[simp]` lemmas enter the discrimination tree.
- All notation and syntax extensions are active.

This directly affects elaboration speed in your file.

### Rules

1. **Import the leaf module, not the parent.** `import Mathlib.Topology.Basic` not `import Mathlib.Topology`.

1. **Do not import modules just for notation.** If you only need a single notation or type, consider `open ... in` or a
   local abbreviation.

1. **Audit imports periodically.** Remove imports that are no longer used. Use `importGraph` (available via mathlib's
   tooling) to visualize the transitive import DAG and spot unintended heavy pulls:

    ```bash
    lake exe graph MyProject
    ```

    Alternatively, comment out imports one at a time and see if the file builds.

1. **Scope `open` declarations narrowly.** `open CategoryTheory` at file scope forces Lean to resolve every name against
   the entire namespace during elaboration. Use `open ... in` blocks or qualified names to keep the cost local.

1. **For mathlib-dependent projects**, always run `lake exe cache get` after updating mathlib. This downloads
   precompiled oleans and avoids rebuilding the entire dependency.

## Incremental Compilation

Lake tracks dependencies via trace hashes (source content, toolchain version, import list). Only changed modules and
their transitive dependents rebuild.

Compiled outputs per module:

- `.olean` — declarations, proof terms, compiled definitions
- `.ilean` — incremental info for the language server
- `.c` / `.o` — generated C code and compiled objects (for `@[extern]` and executable definitions)

### Avoiding unnecessary rebuilds

- **Do not edit leaf modules casually.** A change to a widely-imported module forces rebuilding everything that imports
  it.
- **Use `lake build --old`** during development to skip recompilation of unedited files, assuming oleans are valid. This
  is useful when you know the oleans are fine and want to iterate on a single file.
- **Keep `lakefile.lean` stable.** Changes to the lakefile can invalidate the entire build cache.

## Mathlib Caching

Mathlib provides a cloud cache of precompiled oleans:

```bash
# Download precompiled oleans for all mathlib modules
lake exe cache get

# After git pull on mathlib, always do this before building
```

This reduces initial build from hours to minutes. If the cache is broken or stale:

```bash
lake clean          # or: rm -rf .lake
lake exe cache get  # re-download
lake build
```

## Lean Reservoir

Lean's package registry at `reservoir.lean-lang.org` hosts precompiled artifacts for published packages. Lake can
download build outputs for dependencies from Reservoir automatically, reducing build times for projects with many
dependencies.

## Module Organization Patterns

### Pattern: Interface + Implementation Split

```
MyProject/
  Basic.lean          -- types, structures, core API lemmas
  Basic/
    Lemmas.lean       -- heavier lemmas that only some consumers need
    Tactics.lean      -- custom tactics (expensive to elaborate)
    Instances.lean    -- typeclass instances (adds to synthesis DB)
```

Consumers that only need the types import `Basic`. Consumers that need the full API import `Basic.Lemmas`. This keeps
the critical path short for most files.

### Pattern: Avoiding Re-export Bloat

```lean
-- BAD: re-exports everything from a heavy module
export HeavyModule (Type1 Type2 Lemma1 Lemma2 ...)

-- BETTER: consumers import HeavyModule directly when they need it
-- Your module only imports what it uses
import HeavyModule
open HeavyModule in  -- scoped, not re-exported
```

`export` creates new declarations that downstream modules must process. `open` only affects name resolution in the
current file.

### Pattern: Parallel Test Modules

For a formalization with many independent theorems:

```
KanProofs/
  Syntax/
    Weakening.lean       -- independent
    Substitution.lean    -- independent
    WeakeningSubst.lean  -- imports both (builds after both finish)
```

The two independent modules build in parallel, and the combined module builds last.

## Compiler Options (for Executable Code)

These options affect code generation, not proof checking. Relevant only for modules that produce executable code:

```lean
-- Inlining thresholds
set_option compiler.small 1              -- inline functions ≤ this size
set_option compiler.maxRecInline 1       -- max recursive inlines for @[inline]

-- Closed-term caching
set_option compiler.extract_closed true  -- cache closed terms at init (default)

-- Reference counting optimization
set_option compiler.reuse true           -- insert reset/reuse pairs (default)
```

For most KanProofs work, these are irrelevant — mark definitions `noncomputable` to skip code generation entirely.

## Language Server Performance

The language server re-elaborates files as you edit. To keep it responsive:

1. **Split large files.** The server processes one file at a time. Smaller files mean faster feedback.

1. **Minimize imports.** The server loads all transitively imported oleans into memory. Heavy imports increase memory
   usage (1-2 GB is common for mathlib-heavy files).

1. **Use `lake build` before editing.** Pre-building ensures the server loads cached oleans rather than elaborating
   dependencies from scratch.

1. **Close files you are not editing.** Some editors keep multiple Lean files active, each consuming server resources.

## Build Time Diagnostics

```bash
# Time a full build
time lake build

# Time a single module
time lake env lean MyProject/SlowModule.lean

# Show Lake's dependency graph (useful for finding the critical path)
lake print-paths MyProject.SlowModule
```

For per-command profiling within a file, use `set_option profiler true` (see `profiling-and-diagnostics.md`).
