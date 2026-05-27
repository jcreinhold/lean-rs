# Summary

[Introduction](introduction.md)

# Foundations

- [Charter](architecture/00-charter.md)
- [Safety model](architecture/01-safety-model.md)
- [Versioning and compatibility](architecture/02-versioning-and-compatibility.md)
- [Raw FFI design](architecture/05-raw-sys-design.md)

# Same-process FFI (`lean-rs`)

- [Concurrency](architecture/04-concurrency.md)
- [Panic containment](architecture/06-panic-containment.md)
- [Cooperative cancellation](architecture/07-cooperative-cancellation.md)
- [Reusable interop](architecture/08-reusable-interop.md)
- [Callback ABI spike](architecture/09-callback-abi-spike.md)
- [Callback registry](architecture/10-callback-registry.md)
- [Generic interop shims](architecture/11-generic-interop-shims.md)
- [Interop build and link](architecture/12-interop-build-and-link.md)
- [Structured progress](architecture/13-structured-progress.md)
- [Interop release contract](architecture/14-interop-release-contract.md)
- [Callback payloads](architecture/15-callback-payloads.md)
- [Loader and artifact boundary](architecture/29-loader-and-artifact-boundary.md)
- [Info-tree projection](architecture/info-tree-projection.md)

# Standard Lean services (`lean-rs-host`)

- [Service-layer surface](architecture/03-host-stack.md)
- [Capability contract](lean-rs-host-capability-contract.md)

# Worker (`lean-rs-worker-protocol` / `-parent` / `-child`)

- [Production boundary](architecture/16-production-boundary.md)
- [Worker session adapter](architecture/17-worker-session-adapter.md)
- [Worker data streaming](architecture/18-worker-data-streaming.md)
- [Worker capability layer](architecture/19-worker-capability-layer.md)
- [Worker pool](architecture/20-worker-pool.md)
- [Import-set planning](architecture/21-import-set-planning.md)
- [Worker row batching](architecture/22-worker-row-batching.md)
- [Worker data-plane format](architecture/23-worker-data-plane-format.md)
- [Lean-side worker streaming](architecture/24-lean-side-worker-streaming.md)
- [Mathlib-scale worker fixture](architecture/25-mathlib-scale-worker-fixture.md)
- [Worker pool observability](architecture/26-worker-pool-observability.md)
- [Lean-dup readiness](architecture/27-lean-dup-readiness.md)
- [Production-scale release](architecture/28-production-scale-release.md)

# Recipes

- [Ship a crate with Lean](recipes/ship-crate-with-lean.md)
- [Downstream interop](recipes/downstream-interop.md)
- [String-callback streaming](recipes/string-callback-streaming.md)
- [Worker capability runner](recipes/worker-capability-runner.md)
- [Worker process boundary](recipes/worker-process-boundary.md)

# Safety audits

- [Long-session memory](safety/long-session-memory.md)
- [Unsafe inventory](safety/unsafe-inventory.md)

# Operations

- [Testing](testing.md)
- [Performance](performance.md)
- [Diagnostics](diagnostics.md)
- [Release](release.md)
- [Bump toolchain](bump-toolchain.md)
- [Version matrix](version-matrix.md)
- [Public-API review](api-review.md)
