# lean-rs-worker-protocol

Wire-stable shared types and length-delimited frame codec for the `lean-rs` worker process boundary.

This crate is the single source of truth for the parent/child IPC surface used by
[`lean-rs-worker-parent`](https://docs.rs/lean-rs-worker-parent) and
[`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child). It does not link `libleanshared`, so peers that drive
the wire format (alternative transports, fuzz harnesses, recorders) can depend on it without pulling the Lean runtime.

Most applications should depend on `lean-rs-worker-parent` instead, which re-exports the wire types that appear on its
public API.

## Cargo features

- `harness` — exposes the in-process integration-test harness that spawns a child binary and exercises framed
  request/response round-trips. Off by default.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
