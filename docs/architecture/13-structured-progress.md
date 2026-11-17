# Structured Progress

`LeanProgressSink` observes long-running host operations. `lean-rs` supplies the scoped callback and panic/status
boundary; `lean-rs-host` supplies phase names, totals, and reporting points. Cancellation remains a separate control
signal.

## Public contract

Methods that report progress accept `Option<&dyn LeanProgressSink>` after their cancellation argument. `None` avoids
callback registration. A Lean-side progress path registers `LeanProgressCallback` for one synchronous call and removes
it before returning; Rust-side item loops report directly through the host panic boundary.

`LeanProgressEvent` contains a method-local `phase: &'static str`, `current: u64`, `total: Option<u64>`, and elapsed
`Duration` since that phase began. Events run synchronously on the calling Lean-bound thread. A sink must not re-enter
the same session or call stack; queue expensive UI, logging, or IPC work elsewhere.

Unwinding sink panics become `LeanError::Host` at `HostStage::CallbackPanic`, code `lean_rs.internal`. Fatal Lean
failures still require [process containment](06-panic-containment.md).

## Reporting and batching

The current reporting paths are fresh session imports (including pool misses), declaration lookup/type/kind/name bulk
methods, bulk elaboration, filtered declaration listing, and kernel checking. Events are phase-granular; they do not
promise visibility into every Lean suboperation. Empty bulk input emits no events and performs no dispatch.

With progress and no cancellation token, bulk methods retain one Lean-side bulk dispatch. Declaration lookup also
reports its Rust-side `prepare_names` phase, whose individual name conversions remain separate dispatches. With a
cancellation token, bulk methods that have singular equivalents iterate in Rust and report after each item; cancellation
is observed at the next explicit check and partial output is discarded.

Fresh import, filtered listing, and kernel checking have no corresponding Rust item loop. Their callbacks provide
observations during Lean work, not an interrupt mechanism. A sink may call `token.cancel()`, but a single dispatched
Lean call need not check the flag again before returning. See
[cooperative cancellation](07-cooperative-cancellation.md).

Tracing describes call boundaries for logs and profiles. Progress is caller-supplied live feedback, such as
`phase=query_declarations_bulk current=4200 total=5000`; neither channel replaces the other.

## ABI and package boundary

Additive host progress shims return `IO (Except UInt8 α)`. `LeanProgressCallback::decode_result` decodes success and
maps a `Panic` status to the stored callback error. `StaleHandle`, `WrongPayload`, and `Stopped` are invariant failures:
a host progress sink reports observations and has no generic callback stop semantics. Existing no-progress exports
retain their separate signatures.

Capabilities prepare the bundled generic interop and host shim targets, opening the generic interop library globally
before the host library so imported `LeanRsInterop.Callback.Tick` initialization is available. Package ownership and
parity are specified in [generic shims](11-generic-interop-shims.md).

The [progress implementation](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/progress.rs),
[session dispatch](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/session.rs), and
[progress tests](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/tests/progress.rs) define and verify the reporting behavior.
