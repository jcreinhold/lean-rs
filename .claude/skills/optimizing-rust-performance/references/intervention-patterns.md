# Intervention Patterns

Choose an intervention because the measurements point to it, not because it is a common Rust tip.

## 1. Remove Work

Use when profiles show whole passes, traversals, or conversions dominating.

Good moves:

- cache a checked or normalized fact at the layer that already knows it
- narrow invalidation so fewer modules, defs, or constraints re-run
- batch lookups or reductions instead of repeating them per node
- replace repeated full traversal with one pass that records the needed fact

Validate with:

- same hot-path bench
- one broader throughput workload

## 2. Representation And Ownership

Use when cloning, pointer chasing, or expensive equality/keying dominates.

Good moves:

- borrow instead of own in hot steady-state code
- replace path or string keys with interned or numeric IDs where authority already exists
- split cold metadata out of hot structs
- move from generic containers to dense index storage when key space is compiler-generated

Decision rules:

- Borrow when lifetime naturally stays inside a phase or call tree.
- Arena-allocate when data dies with the phase and does not need `Drop`.
- Intern when equality and hashing on repeated strings dominate.
- Use indexed side storage when keys are dense and compiler-owned.

## 3. Allocation Strategy

Use when DHAT or CPU profiles point at allocation sites.

Good moves:

- `Vec::with_capacity` or `HashMap::with_capacity` when size is known or bounded
- scratch-buffer reuse with `clear()`
- arena slices for phase-local immutable output
- `SmallVec` only when measured length distribution is mostly within the inline size
- `clone_from` when overwriting an existing owned buffer

Decision rules:

- Prefer `SmallVec` for genuinely short collections with a measured distribution, not folklore.
- Prefer `ArrayVec` only when maximum size is hard-bounded and stack size is acceptable.
- Do not introduce `Rc` or `Arc` just to dodge borrowing unless sharing is the real data model.

## 4. Layout And Locality

Use when the hot path touches many nodes or table entries and does little work per touch.

Good moves:

- shrink hot structs or enums
- replace nested pointer-heavy structures with contiguous vectors or slices
- move rarely-read diagnostics or spans out of line
- keep hot fields together and cheap to copy

Validation:

- inspect type sizes if layout is suspected
- re-profile for fewer cache-miss-shaped stacks or lower traversal time

## 5. Hashing, Keying, And Lookup

Use when profiles show hash table work or lookups dominate.

Facts that matter:

- The standard table structure is already strong; the main lever is often key choice and hasher choice.
- For trusted compiler-internal keys, faster hashers can help.
- Integer or interned keys usually beat string or path keys.

Good moves:

- `FxHashMap` or `FxHashSet` for hot, trusted compiler-internal keys
- dense ID indexing instead of hash maps when IDs are contiguous
- precompute fingerprints only when lookups are repeated enough to justify it

Do not:

- switch table type without measuring
- use a faster hasher on potentially adversarial external input by default

## 6. Branch Structure And Cold Paths

Use when the fast path is cluttered with rare checks or expensive error construction.

Good moves:

- move rare diagnostics or formatting behind cold helpers
- separate validation-only work from steady-state work
- check cheap discriminants before expensive normalization or reconstruction

## 7. Backend And Build-Profile Effects

Use only after source-level bottlenecks are understood.

Good moves:

- use the repo's `profiling` profile for profiling binaries
- keep frame pointers when collecting stacks
- consider `cargo llvm-lines` when codegen or monomorphization cost dominates

Do not:

- claim a portable win from `target-cpu=native`
- use build-flag changes as the only evidence for a code change

## 8. Micro-Optimization

This is last.

Examples:

- branchless rewrites
- tighter iterator or loop forms
- selective inlining
- avoiding repeated bounds checks

Only do this when:

- the profile is already tight on a specific line or function
- the higher-level design is already sound
- the measured win survives re-run and broader workloads
