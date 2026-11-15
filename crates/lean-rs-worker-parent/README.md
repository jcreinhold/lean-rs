# lean-rs-worker-parent

Parent-side supervisor for the `lean-rs` worker process boundary.

This crate spawns and supervises `lean-rs-worker-child` processes, frames typed requests across stdin/stdout, drives
sessions and pools, and surfaces structured diagnostics—without itself linking `libleanshared`. That makes it the
recommended dependency for parent binaries, such as servers, dispatchers, or host applications, that need to talk to one
or more worker children at runtime without pinning the parent's link graph to a specific Lean toolchain.

Wire types are re-exported from [`lean-rs-worker-protocol`](https://docs.rs/lean-rs-worker-protocol). Application
binaries that host a Lean runtime in the worker child depend on
[`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child).

## Layering

```text
lean-rs-worker-parent      (this crate; libleanshared-free)
├── lean-rs-worker-protocol  (wire types, no Lean link)
└── lean-toolchain           (manifest validation, capability descriptor; link-free ABI metadata)
```

The child runtime (`lean-rs-worker-child`) is published separately and is the only crate in the stack that links
`libleanshared`.

## Shutdown

Call `shutdown()` on `LeanWorker`, `LeanWorkerCapability`, or `LeanWorkerHostHandle` when shutdown status matters. It
stops request admission, asks the child to terminate, escalates to kill after a bounded timeout, waits for the child to
be reaped, and returns a structured `LeanWorkerShutdownReport`.

The older `terminate()` methods are deprecated compatibility wrappers that return only the final exit status. Dropping a
worker handle runs best-effort bounded cleanup, but Rust `Drop` cannot report kill or wait failures.

To distinguish a failed operation whose direct worker has stopped from unconfirmed execution, take
`request_checkpoint()` before the synchronous operation and then query `confirmed_request_stop_since(&checkpoint)`. The
receipt binds the latest attempted request to its generation, PID and observed exit, and survives replacement. It does
not establish a successful result, absence of earlier effects, descendant cleanup, replacement cleanup, or permission to
replay the request. Capacity remains governed by the process tracker and stopped-owner contract.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
