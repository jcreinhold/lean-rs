# Import Set Planning

The planner sits above the memory-aware worker pool and groups module work by the same facts the pool uses for session
reuse, so downstream tools can avoid repeating Lean imports without learning worker process mechanics.

## Crate Boundary

Two concerns are better apart:

**Planner in the worker crates.** Rejected as the only layer. Worker sessions and pool keys matter for batching, but
Lake module discovery is useful without a worker child process.

**Discovery in `lean-toolchain`, batching in the worker crates.** Chosen. `lean-toolchain` owns Lake root detection,
module-root discovery, module-name validation, source-set fingerprints, and capability-target declaration checks. The
worker crates own the worker-facing batch plan because the batch key is a `LeanWorkerSessionKey` and feeds
`LeanWorkerPool` leases.

This split keeps Lake layout policy in one general-purpose crate and keeps worker session policy in the worker crate.
Neither crate learns downstream commands, row schemas, cache validity, ranking, or reporting.

## Discovery

`lean-toolchain::discover_lake_modules` accepts a requested root and optional selected module roots. It resolves
conventional Lake project locations, discovers `lean_lib` roots from `lakefile.lean` or `lakefile.toml`, enumerates Lean
source modules deterministically, and returns:

- `LeanModuleDescriptor` values with module name, source path, and source root;
- `LeanModuleSetFingerprint` with the build-baked toolchain fingerprint, lakefile digest, optional manifest digest,
  source count, and maximum source mtime;
- typed `LeanModuleDiscoveryDiagnostic` errors for missing Lake roots, missing selected module roots, invalid module
  names, unsupported toolchains, and I/O failures.

The fingerprint carries cache-key-relevant facts. It does not decide whether a downstream cache entry is fresh.

## Worker Batches

`LeanWorkerImportPlanner` consumes discovery output or explicit `LeanWorkerModuleWork` values. It groups work by:

- Lake project root;
- capability package and target;
- source root;
- import set;
- optional capability metadata expectation;
- Lean toolchain and `lean-rs` protocol fingerprint;
- restart-policy class.

Each `LeanWorkerPlannedBatch` contains a `LeanWorkerSessionKey`, module work items, a batch fingerprint, and enough
capability material to create a `LeanWorkerCapabilityBuilder`. The plan contains no worker ids, child pids, pipe
handles, protocol frames, queue positions, or scheduling decisions. The pool still decides which local child hosts a
batch.

## Downstream Use

A downstream tool should treat planned batches as execution groups, not cache records. The group says: "these modules
can run against the same worker session requirements." It does not say whether a declaration row, feature row, probe
result, or report is valid for reuse. That remains downstream policy.

The import planner is therefore deliberately not a `lean-dup` API. A `lean-dup` integration can map its source modules
into planned batches, then run typed worker commands through pool leases. The worker crates still does not define
`extract`, `features`, `index`, or `probe` methods.

## Performance Claim

The fixture test records the shape rather than claiming universal speedup: naive per-module execution opened one worker
capability per module, while the planned path used one batch and one pool worker. On a local run, two interop fixture
modules took 1049 ms in the naive path and 573 ms through a single planned pool lease. Larger projects should use their
own workload numbers before turning a batch plan into a cache or scheduling policy.
