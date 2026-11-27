# Architecture

Start with the [charter](charter.md) for crate ownership and the [safety model](safety.md) for Rust boundary obligations.
Then choose the contract for the change you are making. The filenames name topics; this page and the book sidebar
provide the reading order.

## Runtime and FFI

| Change | Read |
| --- | --- |
| Add a raw symbol, layout mirror, or reference-count operation | [Raw FFI](raw-ffi.md) |
| Change supported Lean versions, ABI checks, or protocol compatibility | [Compatibility](compatibility.md) |
| Initialize Lean, attach a thread, or embed in async code | [Concurrency](concurrency.md) |
| Add a callback payload or package a generic Lean/C helper | [Callbacks and interop shims](callbacks.md) |
| Build a Lake target, reuse artifacts, or acquire writer custody | [Build and link](build-and-link.md) |
| Load manifests, dependency libraries, or packaged capabilities | [Loading](loading.md) |

## Standard host services

| Change | Read |
| --- | --- |
| Add a Lean service, evidence type, or same-process session API | [Host services](host-services.md) |
| Change shim exports or capability metadata | [Host capability contract](../lean-rs-host-capability-contract.md) |
| Process source, selectors, proof positions, or degraded query results | [Module queries](module-queries.md) |
| Bound work, report progress, cancel, or use bracketed imports | [Same-process controls](../same-process-controls.md) |

## Worker services

| Change | Read |
| --- | --- |
| Choose isolation, classify fatal failures, or reset Lean memory | [Worker processes](worker-processes.md) |
| Expose host operations, loaded metadata, or artifact read custody over IPC | [Worker sessions](worker-sessions.md) |
| Configure a downstream command and its import workspace | [Worker capabilities](worker-capabilities.md) |
| Deliver live rows, apply backpressure, or change framing | [Worker streaming](worker-streaming.md) |
| Reuse workers, admit leases, or report pool statistics | [Worker pool](worker-pool.md) |
| Change generations, cleanup ownership, capacity, or restart semantics | [Worker runtime](worker-runtime.md) |

The worker runtime is the lifecycle contract. The session, build, and loading pages own their boundary-specific details;
link to those contracts when extending a feature instead of copying them into a second design note.

## Implement and verify

Use the [recipes](../recipes/ship-crate-with-lean.md) for a runnable starting point,
[production hosting](../production-hosting.md) for deployment budgets, [testing](../testing.md) for checks and fixtures,
and [performance](../performance.md) for measurements. The [unsafe inventory](../safety/unsafe-inventory.md) and
[memory investigation](../safety/long-session-memory.md) record specific findings and their limits.
