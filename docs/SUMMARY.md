# Summary

[Introduction](introduction.md)

# Guides

- [Ship a crate with Lean](recipes/ship-crate-with-lean.md)
- [Exports and callbacks](recipes/downstream-interop.md)
- [Worker commands and raw transport](recipes/worker-capability-runner.md)
- [Production hosting](production-hosting.md)
- [Diagnostics](diagnostics.md)

# Architecture

- [Architecture map](architecture/README.md)
- [Charter](architecture/charter.md)
- [Safety model](architecture/safety.md)
- [Compatibility](architecture/compatibility.md)

# Runtime and FFI

- [Raw FFI](architecture/raw-ffi.md)
- [Concurrency](architecture/concurrency.md)
- [Callbacks and interop shims](architecture/callbacks.md)
- [Build and link](architecture/build-and-link.md)
- [Loading](architecture/loading.md)

# Standard host services

- [Host services](architecture/host-services.md)
- [Host capability contract](lean-rs-host-capability-contract.md)
- [Module queries](architecture/module-queries.md)
- [Same-process controls](same-process-controls.md)

# Worker services

- [Worker processes](architecture/worker-processes.md)
- [Worker capabilities](architecture/worker-capabilities.md)
- [Worker sessions](architecture/worker-sessions.md)
- [Worker streaming](architecture/worker-streaming.md)
- [Worker pool](architecture/worker-pool.md)
- [Worker runtime](architecture/worker-runtime.md)

# Verification and maintenance

- [Testing and fixtures](testing.md)
- [Performance](performance.md)
- [Long-session memory](safety/long-session-memory.md)
- [Unsafe inventory](safety/unsafe-inventory.md)
- [Release](release.md)
- [Bump toolchain](bump-toolchain.md)
- [Version matrix](version-matrix.md)
- [Public-API review](api-review.md)
