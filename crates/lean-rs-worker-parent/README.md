# lean-rs-worker-parent

Starts and supervises [`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child) processes without linking
`libleanshared`. It frames typed requests over each child's stdin and stdout, runs worker sessions, enforces deadlines,
restarts and cleanup, and reports structured diagnostics. It re-exports the wire types from
[`lean-rs-worker-protocol`](https://docs.rs/lean-rs-worker-protocol).

```text
lean-rs-worker-parent        (this crate; no Lean link)
├── lean-rs-worker-protocol  (wire types; no Lean link)
└── lean-toolchain           (manifest checks, capability descriptors)
```

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
