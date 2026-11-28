# lean-rs-host

Lean's standard services in one Rust process, on top of [`lean-rs`](https://docs.rs/lean-rs): imports, sessions,
elaboration with typed diagnostics, kernel checks with evidence, declaration and module queries, and bounded `MetaM`
services under `lean_rs_host::meta`. The crate bundles its host and interop shim packages and has Lake build them when
first needed, so a Lake project declares only its own library.

```text
LeanHost                opened once per Lean runtime
  └─ LeanCapabilities   loaded once per capability library, or shims only
       └─ LeanSession   one per unit of work
```

[`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child) runs these services in a worker process.

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
