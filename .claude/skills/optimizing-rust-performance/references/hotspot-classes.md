# Kan Hotspot Classes

These are the bottleneck classes that repeatedly matter in this codebase.

## Arena And Phase-Local Allocation

Start here when you see many short-lived vectors, slices, or strings.

- `crates/util/arena/src/lib.rs`
- `crates/kernel/core/src/arena.rs`
- `crates/kernel/core/benches/util.rs`
- `crates/frontend/typecheck-infer/benches/util.rs`

What to look for:

- building temporary `Vec`s only to copy into arena slices
- re-allocating scratch buffers inside loops instead of clearing and reusing
- storing data past a phase boundary when it should die with the arena
- using store/load or owned clones as a borrow workaround instead of fixing lifetime structure

## Normalization, Evaluation, And Read-Back

These are core compiler hot paths.

- `crates/kernel/core/benches/normalization_bench.rs`
- `crates/kernel/core-eval/benches/eval_bench.rs`
- `crates/kernel/core-eval/src/engine/normalize.rs`
- `crates/kernel/core-eval/src/engine/trampoline.rs`
- `crates/kernel/core-eval/src/engine/quote_split.rs`

Typical bottlenecks:

- repeated closure forcing
- unnecessary quoting or normalization when weak-head work would suffice
- environment growth or copying under binders
- recursive or pointer-heavy traversals in hot steady-state loops

## Traversal Cost

Traversal shows up both directly and as overhead inside other passes.

- `crates/kernel/core/benches/traversal_bench.rs`
- search with `rg -n "fold_term|walk|visitor|travers" crates/kernel crates/frontend crates/pipeline`

Typical bottlenecks:

- repeated full traversals where a cached or incremental fact would do
- collecting transient child vectors instead of using an explicit stack or iterator
- recomputing structure facts on every pass

## Typechecker, Unifier, Constraint Queue, Metas

This is one of the most important hotspot families for Kan.

- `crates/frontend/typecheck-infer/benches/typecheck_bench.rs`
- `crates/frontend/typecheck-infer/benches/unification_bench.rs`
- `crates/frontend/typecheck-infer/benches/regression_bench.rs`
- `crates/frontend/typecheck-infer/benches/dhat_profile.rs`
- `crates/frontend/typecheck-infer/src/constraints/queue.rs`
- `crates/frontend/typecheck-infer/src/constraints/solving.rs`
- `crates/frontend/typecheck-infer/src/meta/state.rs`
- `crates/frontend/typecheck-infer/src/unification/dispatch.rs`

Typical bottlenecks:

- repeated normalization during dispatch
- constraint deduplication or wake-up overhead
- meta-solution lookup churn
- cloning definitions, environments, or metadata at retry boundaries
- persistent structure costs in deep or write-heavy paths

## Registry, Metadata, Side Tables, Cache Lookups

These costs often hide behind "small" operations executed everywhere.

- `crates/kernel/core/benches/registry_cache_bench.rs`
- `crates/kernel/core-typecheck/src/metadata/table.rs`
- `crates/kernel/core-typecheck/src/metadata/recording.rs`
- `crates/pipeline/build/src/compilation_pipeline.rs`
- `crates/pipeline/build/src/stdlib_build.rs`

Typical bottlenecks:

- `HashMap<Vec<PathSegment>, ...>` style keys on hot paths
- repeated registry cloning or cache-key cloning across phase boundaries
- converting cheap IDs into expensive path or string keys too early
- cold metadata bloating hot structs instead of living in a side table

## Closure Capture And Environment Representation

Kan uses closures and environments in multiple layers.

- `crates/execution/interpreter/benches/interpreter.rs`
- `crates/frontend/typecheck-infer/benches/regression_bench.rs`
- search with `rg -n "CapturedEnv|closure|captured|imbl::Vector|push_back" crates`

Typical bottlenecks:

- persistent vectors helping lookup but hurting repeated writes
- copying captured environments at instantiation or quoting boundaries
- storing more in every closure than the hot path actually needs

## Pipeline Throughput And Pass Boundaries

Micro wins can lose here.

- `crates/pipeline/build/benches/pipeline_bench.rs`
- `profiling/src/bin/profile_frontend.rs`
- `profiling/src/bin/profile_full_build.rs`
- `crates/pipeline/build/src/compilation_pipeline.rs`
- `crates/pipeline/build/src/pipeline/module_cache.rs`

Typical bottlenecks:

- pass-local clones that scale with module count
- repeated arena setup or source-map allocation
- invalidation that forces broad recomputation
- data converted to a new representation at each pass when a stable handle would do

## Persistent Structures And Immutable Data

Persistent collections are not free. They help when sharing dominates copying, but they hurt when mutation depth
dominates.

Search:

```bash
rg -n "imbl::|Vector<|HashMap<|HashSet<|SmallVec<" crates/frontend crates/kernel crates/pipeline
```

Questions to ask:

- Is the hot operation mostly reads, appends, random lookups, or full clones?
- Is structural sharing paying for itself?
- Would an arena slice, dense index storage, or reusable `Vec` be cheaper for this phase?
