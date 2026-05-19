# Distilled External Lessons

These are not cargo-cult recipes. They are decision rules distilled from Rust performance references and strong Rust
codebases.

## From The Rust Performance Book

- Profile before optimizing. Hot paths are frequently not where intuition points.
- Allocation work is often the dominant cost in compiler-style Rust.
- `clone`, `to_owned`, and `to_string` are fine until profiles show they are hot.
- `SmallVec` is only justified by measured short-length distributions.
- Faster hashers help only when hashing is actually hot and keys are trusted.

Applying this:

- treat clone pressure, transient vectors, and key choice as first-class suspects
- use DHAT or allocation-aware baselines when CPU profiles show allocator frames

## From Criterion.rs

- Keep baseline management explicit.
- Use `--save-baseline` and `--baseline` instead of eyeballing numbers.
- Use `--profile-time` when attaching profilers so the profiler sees the benchmarked code, not Criterion's adaptive
    measurement machinery.

Applying this:

- store baselines around perf-sensitive changes
- prefer repeatable Criterion workflows over ad hoc `Instant` timing

## From DHAT

- Heap profiling is easy to enable and gives both total and peak views.
- Allocation count, total bytes, and peak live bytes answer different questions.
- Testing-style heap assertions can guard regressions when a path is stable enough.

Applying this:

- use DHAT for typechecker and unifier allocation work, not just CPU hot spots
- consider regression-style allocation assertions only for stable, non-noisy paths

## From rustc, measureme, And rustc-perf

- Large compilers need more than one profiling surface.
- Query or stage-level summaries and function-level profilers are complementary, not competing.
- Curated benchmark suites matter because local wins often regress another workload.
- Scenario labels such as full, incremental, cached, and uncached change conclusions.

Applying this:

- use both the local crate bench and a broader pipeline or end-to-end workload
- distinguish cold-cache, warm-cache, single-file, stdlib, and full-build claims

## From rust-analyzer

- Built-in lightweight profiling and batch analysis tools make iterative optimization practical.
- Performance debugging gets much faster when the codebase has command-driven workloads instead of IDE-only
    reproduction.

Applying this:

- prefer command-line reproductions over GUI-only ones
- if a hot path only reproduces through a larger workflow, consider adding a dedicated workload rather than relying on
    manual reproduction

## From ripgrep

- Benchmark design must isolate the real task instead of accidentally measuring hidden fast paths.
- Representative workload shape matters more than generic "faster engine" claims.
- The right optimization may be choosing a strategy that better matches the input, not just making the current strategy
    cheaper.

Applying this:

- avoid toy inputs that miss the real traversal, normalization, or cache behavior
- compare realistic input shapes and cache states

## From regex-automata

- Expose explicit space/time tradeoffs instead of pretending one representation wins everywhere.
- Dense representations, sparse representations, state ID size, and alphabet factoring each move different cost axes.
- Compilation cost and runtime cost can trade hard against one another.

Applying this:

- be explicit when choosing between arena slices, persistent vectors, hash tables, dense indices, or cached normalized
    forms
- measure build-time and runtime effects separately when a representation change impacts both

## From hashbrown

- Table structure is only one part of map performance.
- Hasher choice, key shape, load factor, and empty-map behavior can matter as much as the table implementation.

Applying this:

- focus first on integer or interned keys and hot-path hasher choice
- do not expect switching map type alone to solve a lookup problem

## External Sources Used

- The Rust Performance Book
- Criterion.rs book
- `dhat` docs
- `samply` README
- `flamegraph-rs` README
- Rust Compiler Development Guide profiling chapters
- `measureme` README
- `rustc-perf` docs
- rust-analyzer contributing and profiling docs
- ripgrep FAQ
- regex-automata README
- hashbrown README
