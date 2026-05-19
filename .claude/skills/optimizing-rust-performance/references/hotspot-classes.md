# Rust Hotspot Classes

These are the bottleneck classes that repeatedly matter in compiler-style Rust codebases. Each section names the
class, lists the patterns to grep for, and calls out the typical failures inside that class.

## Arena And Phase-Local Allocation

Start here when you see many short-lived vectors, slices, or strings.

Search:

```bash
rg -n "arena|Bump|scratch|fold_term|walk|visitor" crates
```

What to look for:

- building temporary `Vec`s only to copy into arena slices
- re-allocating scratch buffers inside loops instead of clearing and reusing
- storing data past a phase boundary when it should die with the arena
- using store/load or owned clones as a borrow workaround instead of fixing lifetime structure

## Normalization, Evaluation, And Read-Back

These are core compiler hot paths.

Look for files named `normalize.rs`, `eval.rs`, `quote.rs`, `whnf.rs`, `trampoline.rs`, and benches that exercise them.

Typical bottlenecks:

- repeated closure forcing
- unnecessary quoting or normalization when weak-head work would suffice
- environment growth or copying under binders
- recursive or pointer-heavy traversals in hot steady-state loops

## Traversal Cost

Traversal shows up both directly and as overhead inside other passes.

Search:

```bash
rg -n "fold_term|walk|visitor|travers" crates
```

Typical bottlenecks:

- repeated full traversals where a cached or incremental fact would do
- collecting transient child vectors instead of using an explicit stack or iterator
- recomputing structure facts on every pass

## Typechecker, Unifier, Constraint Queue, Metas

One of the most important hotspot families in compiler workloads.

Look for files named `unify.rs`, `unification/dispatch.rs`, `constraints/queue.rs`, `constraints/solving.rs`,
`meta/state.rs`, and benches that exercise them.

Typical bottlenecks:

- repeated normalization during dispatch
- constraint deduplication or wake-up overhead
- meta-solution lookup churn
- cloning definitions, environments, or metadata at retry boundaries
- persistent structure costs in deep or write-heavy paths

## Registry, Metadata, Side Tables, Cache Lookups

These costs often hide behind "small" operations executed everywhere.

Look for `registry.rs`, `metadata/table.rs`, cache modules under the pipeline driver, and stdlib build orchestration.

Typical bottlenecks:

- `HashMap<Vec<PathSegment>, ...>` style keys on hot paths
- repeated registry cloning or cache-key cloning across phase boundaries
- converting cheap IDs into expensive path or string keys too early
- cold metadata bloating hot structs instead of living in a side table

## Closure Capture And Environment Representation

Compilers and interpreters use closures and environments in multiple layers.

Search:

```bash
rg -n "CapturedEnv|closure|captured|imbl::Vector|push_back" crates
```

Typical bottlenecks:

- persistent vectors helping lookup but hurting repeated writes
- copying captured environments at instantiation or quoting boundaries
- storing more in every closure than the hot path actually needs

## Pipeline Throughput And Pass Boundaries

Micro wins can lose here.

Look for the pipeline driver, the module cache, and any shared profiling crate's full-build / frontend binaries.

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
rg -n "imbl::|Vector<|HashMap<|HashSet<|SmallVec<" crates
```

Questions to ask:

- Is the hot operation mostly reads, appends, random lookups, or full clones?
- Is structural sharing paying for itself?
- Would an arena slice, dense index storage, or reusable `Vec` be cheaper for this phase?
