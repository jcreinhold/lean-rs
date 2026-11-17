# Import Set Planning

`lean-toolchain` discovers Lake modules without a worker dependency. `LeanWorkerImportPlanner` groups that discovery
into compatible worker-session batches. The pool still chooses the child; the planner neither schedules requests nor
decides downstream cache validity.

## Discovery

`lean-toolchain::discover_lake_modules` resolves a Lake project, reads `lean_lib` roots from `lakefile.lean` or
`lakefile.toml`, validates module names, and enumerates sources deterministically. Callers may restrict selected roots.
It returns:

- `LeanModuleDescriptor` values with module name, path, and source root;
- `LeanModuleSetFingerprint` with the configured toolchain fingerprint (build-baked by default), lakefile digest, optional manifest digest,
  source count, and maximum source mtime;
- typed errors for missing projects/selected roots, invalid names, unsupported toolchains, and I/O failures.

The fingerprint records source-set facts; it is not a content digest of every source file. The planner's
`plan_lake_project` also requires the capability target to be explicitly declared as a `lean_lib`.

## Batches

The planner accepts discovered modules or explicit `LeanWorkerModuleWork` values. It groups by project root, capability
package/target, source root, ordered import set, import profile, and restart-policy class. Configured capability metadata
expectations are carried into each batch's session key. Each `LeanWorkerPlannedBatch` includes sorted modules, a batch
fingerprint, and material for `capability_builder()`.

The [pool reuse key](20-worker-pool.md#leases-and-reuse) also carries build-baked toolchain identity. There is no separate
worker protocol fingerprint in the planner or session-key structure. Work planned from one capability project defaults
to that project's import workspace; a caller configuring a different workspace must set that on the derived builder.

Treat a planned batch as an execution group: its modules share session requirements. It makes no claim about reuse of
cached declaration rows, feature rows, proof results, or reports.

## Recorded fixture comparison

The fixture comparison opened one capability per module in the naive path and one batch/worker in the planned path.
A recorded local run of two interop modules took 1049 ms naively and 573 ms with one planned lease. That capture lacks
platform/revision metadata and is historical shape evidence, not a portable baseline. Larger projects need their own
measurements before adopting a cache or scheduling policy.
