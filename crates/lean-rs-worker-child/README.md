# lean-rs-worker-child

Child-process runtime for the `lean-rs` worker boundary.

This crate ships the `lean-rs-worker-child` binary that hosts a `lean-rs` runtime and serves typed requests over
stdin/stdout from a parent supervisor. It is the only crate in the worker stack that links `libleanshared`.

Parent supervisors should depend on [`lean-rs-worker-parent`](https://docs.rs/lean-rs-worker-parent). The wire types
both peers exchange live in [`lean-rs-worker-protocol`](https://docs.rs/lean-rs-worker-protocol).

## Layering

```text
lean-rs-worker-child        (this crate; links libleanshared)
├── lean-rs-worker-protocol   (wire types, no Lean link)
├── lean-rs                   (typed FFI layer)
├── lean-rs-host              (standard Lean services: capabilities, sessions, kernel)
└── lean-toolchain            (manifest validation, capability descriptor)
```

## Custom child binary

Application binaries can wrap [`run_worker_child_stdio`] in a one-line `main` to ship a per-toolchain worker child whose
identity matches the host application:

```rust,ignore
fn main() -> std::process::ExitCode {
    lean_rs_worker_child::run_worker_child_stdio()
}
```

## Parent Loss

The child treats stdin/stdout as the control channel. If stdin reaches EOF or a response write fails because stdout is
closed, the child exits instead of waiting for another request. On Linux, startup also installs a best-effort
`PR_SET_PDEATHSIG(SIGTERM)` guard so abrupt parent death can terminate the child even when inherited stdio descriptors
remain open. Other platforms rely on the portable control-channel behavior and external process-manager policy.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
