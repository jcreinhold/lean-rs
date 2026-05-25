# lean-rs-worker-parent

Parent-side supervisor for the `lean-rs` worker process boundary.

This crate spawns and supervises `lean-rs-worker-child` processes, frames typed requests across stdin/stdout, drives
sessions and pools, and surfaces structured diagnostics — without itself linking `libleanshared`. That makes it the
recommended dependency for parent binaries (e.g. servers, dispatchers, host applications) that need to talk to one or
more worker children at runtime without pinning the parent's link graph to a specific Lean toolchain.

Wire types are re-exported from [`lean-rs-worker-protocol`](https://docs.rs/lean-rs-worker-protocol). Application binaries
that host a Lean runtime in the worker child depend on
[`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child).

## Layering

```text
lean-rs-worker-parent      (this crate; libleanshared-free)
├── lean-rs-worker-protocol  (wire types, no Lean link)
└── lean-toolchain           (manifest validation, capability descriptor; lean-rs-sys metadata-only)
```

The child runtime (`lean-rs-worker-child`) is published separately and is the only crate in the stack that links
`libleanshared`.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
