# lean-rs documentation

`lean-rs` hosts Lean 4 in Rust applications. Lean owns elaboration, kernel checking, proof objects, and dependent-type
meaning; Rust owns the runtime bridge, error boundaries, scheduling, and packaging.

For a runnable project, start with [Ship a crate with Lean](recipes/ship-crate-with-lean.md). Use
[Downstream interop](recipes/downstream-interop.md) for custom exports and callbacks, or
[Worker capability runner](recipes/worker-capability-runner.md) for supervised commands and live rows.
[Production hosting](production-hosting.md) covers long-running services with bounded workers and warm sessions.

For design work, use the [architecture map](architecture/) to find the owning contract. Start with the
[charter](architecture/charter.md) and [safety model](architecture/safety.md). Measurements and audit records retain
their dates and workload limits; they are evidence for those workloads rather than universal performance guarantees.

The [repository README](https://github.com/jcreinhold/lean-rs#readme) maps crates to tasks. The
[version matrix](version-matrix.md) defines supported Lean releases and platforms, and [Testing](testing.md) describes
verification. See [Diagnostics](diagnostics.md) when a run fails.
