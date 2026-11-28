# lean-rs-worker-child

The Lean-linked worker process that [`lean-rs-worker-parent`](https://docs.rs/lean-rs-worker-parent) starts. It hosts a
`lean-rs` runtime, serves typed requests over stdin and stdout one at a time, and exits when its parent dies. It is the
only crate in the worker stack that links `libleanshared`. A worker binary's `main` is one call:

```rust
fn main() -> std::process::ExitCode {
    lean_rs_worker_child::run_worker_child_stdio()
}
```

```text
lean-rs-worker-child         (this crate; links libleanshared)
├── lean-rs-worker-protocol  (wire types; no Lean link)
├── lean-rs                  (runtime and objects)
├── lean-rs-host             (sessions, elaboration, kernel checks)
└── lean-toolchain           (manifest checks, capability descriptors)
```

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
