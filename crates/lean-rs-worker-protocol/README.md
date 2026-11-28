# lean-rs-worker-protocol

The messages between [`lean-rs-worker-parent`](https://docs.rs/lean-rs-worker-parent) and
[`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child): their owned value types, the length-delimited frame
codec, and the environment variable names both sides read. It does not link `libleanshared`. `lean-rs-worker-parent`
re-exports the types that appear in its API.

## Cargo features

- `harness`: an in-process test harness that spawns a child binary and runs framed request and reply round trips. Off by
  default.

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
