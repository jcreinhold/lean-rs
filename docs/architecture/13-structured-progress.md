# Structured Progress

`lean-rs-host` progress is a host-level observation channel for long-running `LeanSession` calls. It is built on the L1
callback registry from `lean-rs` and the generic `lean-rs-interop-shims` callback helper. The host crate owns the
policy: phase names, totals, cancellation checkpoints, and which session methods report events.

## Public Contract

Long-running host methods accept a final `progress: Option<&dyn LeanProgressSink>` after their existing cancellation
parameter. `None` is the fast path: no callback handle is allocated, no progress shim is called, and bulk methods keep
their single-dispatch shape. `Some(sink)` registers a temporary callback handle for the one call and drops it before the
method returns.

The temporary handle is `LeanCallbackHandle<LeanProgressTick>`. The bridge converts that L1 tick payload into a host
`LeanProgressEvent`; it does not expose `LeanStringEvent` or any downstream streaming policy through the host progress
surface.

`LeanProgressEvent` contains:

- `phase: &'static str`: a stable method-local phase label;
- `current: u64`: a phase-local counter;
- `total: Option<u64>`: a known phase-local bound when cheap;
- `elapsed: Duration`: time since that phase began.

Events are delivered synchronously on the Lean-bound worker thread. A sink must not call back into the same
`LeanSession` or re-enter the same Lean call stack. Expensive UI, logging, or IPC work should be queued by the sink and
handled on a different thread.

Rust panics from progress sinks are contained by the callback boundary. The session method returns `LeanError::Host`
with stage `CallbackPanic` and code `lean_rs.internal`. Lean internal panics remain process-scoped; see
[`06-panic-containment.md`](06-panic-containment.md).

## Reporting Points

Progress is phase-granular, not a promise that every Lean suboperation reports. The current host surface reports
progress for:

- `LeanCapabilities::session` and `SessionPool::acquire` on fresh imports;
- `LeanSession::query_declarations_bulk`;
- `LeanSession::{declaration_type_bulk,declaration_kind_bulk,declaration_name_bulk}`;
- `LeanSession::elaborate_bulk`;
- `LeanSession::list_declarations_filtered`;
- `LeanSession::kernel_check`.

Bulk methods with `progress = Some` and `cancellation = None` use Lean-side progress shims so they still perform one
bulk dispatch. Bulk methods with both progress and cancellation use Rust per-item loops where a per-item singular path
exists, so cancellation can be observed between items and partial output can be discarded. `list_declarations_filtered`,
fresh import, and `kernel_check` do not have a Rust per-item equivalent; cancellation is checked before dispatch and
then again only after Lean returns.

Empty bulk input returns an empty vector, records no FFI dispatch, and emits no progress event.

## Relationship To Tracing And Cancellation

`tracing` spans describe call boundaries and are useful for logs, profiles, and post-hoc diagnostics. Progress events
are a caller-supplied in-process callback for live user feedback such as
`phase=query_declarations_bulk current=4200 total=5000`. Neither replaces the other.

Cancellation is a control signal. Progress is an observation signal. Where a method uses a Rust per-item loop, the
cancellation check and progress report happen at the same item boundary. A progress sink may call `token.cancel()` on a
shared `LeanCancellationToken`; the method observes that token at the next host-controlled cancellation check.

## ABI Boundary

The host shims expose additive progress variants returning `IO (Except UInt8 α)`. The `UInt8` is the L1
`LeanCallbackStatus` byte. Rust decodes `Except.ok` as the method result, maps `Panic` through the callback handle's
stored error, and maps `StaleHandle`, `WrongPayload`, and `Stopped` to internal host invariant failures. Host progress
is one-way: unlike a generic L1 callback loop, a progress sink does not define stop semantics. Existing no-progress shim
symbols and signatures are unchanged.

`LeanCapabilities` builds the crate-bundled generic interop and host shim Lake targets on demand, then opens the generic
interop shim dylib globally before opening the host shim dylib. This anchors the generated `LeanRsInterop.Callback.Tick`
initializer that host progress shims import.
