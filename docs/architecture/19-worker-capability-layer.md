# Worker Capability Layer

Prompt 65 proved the transport mechanics for worker data rows: a child process
can run a downstream Lean export and send JSON-shaped rows to the parent as
`LeanWorkerDataRow` values. That proof is necessary, but it is not yet a
complete replacement for a production subprocess worker such as
`lean-dup-worker`.

A subprocess worker does more than move rows. It defines when rows become
committable, how diagnostics are separated from data, how request timeouts are
handled, how callers discover capability versions, how the worker is built and
started, and how performance is measured. Those concerns belong above raw row
transport and below downstream business schemas.

## Chosen Boundary

`lean-rs-worker` should grow a generic worker capability layer. That layer owns:

- process lifecycle and restart behavior;
- private worker framing and live bounded row forwarding;
- row ordering, row-boundary cancellation, and sink failure handling;
- terminal completion metadata and commit-after-success semantics;
- diagnostics as a channel distinct from data rows;
- per-request timeout and watchdog behavior;
- capability metadata and doctor checks;
- build/session setup ergonomics for downstream capabilities;
- typed command facades over downstream-owned serde request, row, and summary
  types;
- performance evidence for large streams.

Downstream crates own:

- command names and their semantic meaning;
- request, row, and terminal summary schemas;
- validation beyond the generic worker envelope;
- cache policy, persistence, indexing, and user-facing workflows.

This keeps the worker layer somewhat general-purpose. It hides process and IPC
mechanics that every production consumer would otherwise reimplement, but it
does not encode `lean-dup` declarations, features, probes, or cache validity as
first-party `lean-rs-worker` concepts.

## Rejected Designs

**Raw `run_data_stream`.** Rejected as the final downstream interface. It is a
useful primitive, but it asks every caller to know export names, request JSON
encoding, row sink wiring, session setup, completion policy, and failure
classification. That is too much worker machinery at the call site.

**`lean-dup`-specific worker APIs.** Rejected for `lean-rs-worker`. Methods such
as `extract`, `features`, `index`, or `probe` would make the worker crate a
`lean-dup` adapter. Those commands are valid downstream examples, not generic
worker responsibilities.

**Generic worker capability layer.** Chosen. The worker owns lifecycle,
framing, live streaming, timeout, cancellation, diagnostics, metadata,
completion, and restart behavior. Downstream crates own schemas and semantic
commands.

## Missing Capabilities

The next prompts fill the gap between row transport and a production worker
replacement:

1. **Live bounded forwarding.** Rows must reach the parent while Lean produces
   them. The worker needs bounded buffering, backpressure rules, cancellation at
   row boundaries, and sink-failure handling.
2. **Terminal completion and diagnostics.** A stream needs per-stream row
   counts, elapsed time, optional downstream terminal JSON, and clear
   commit-after-success semantics. Diagnostics must not be smuggled through row
   payloads.
3. **Timeouts and watchdogs.** Startup timeout, request timeout, cancellation,
   child crash, and policy restart are different outcomes and need different
   typed errors.
4. **Capability metadata and doctor checks.** Downstream tools need a generic
   way to report protocol versions, capability versions, supported commands,
   supported features, Lean version, and health diagnostics without hard-coding
   one tool's command set into `lean-rs-worker`.
5. **Builder ergonomics.** A normal downstream path should compose Lake target
   build, shim resolution, worker child path, capability dylib path, imports,
   restart policy, and session opening without handwritten path mangling.
6. **Typed command facade.** Downstream callers should be able to use serde
   request, row, and summary types while `lean-rs-worker` owns transport,
   lifecycle, diagnostics, timeout, cancellation, and completion.
7. **High-throughput row payload path.** Large streams need measured choices
   around `serde_json::Value`, raw JSON values, owned bytes, allocation count,
   row throughput, and parent/child RSS.

## Relationship To Callback Payloads

Worker capability streaming is IPC work. It does not replace the L1 callback
payload track in `lean-rs`.

Inside the child, a streaming runner may use
`LeanCallbackHandle<LeanStringEvent>` to receive row JSON from Lean. Across the
worker boundary, the parent receives worker rows and diagnostics. Callback
handles, raw Lean object lifetimes, and trampoline values remain in-process
mechanisms.

Byte and object callback prompts remain L1 `lean-rs` payload work. They are not
shortcuts for worker capability streaming, and worker capability streaming is
not a reason to expose arbitrary downstream `LeanCallbackPayload`
implementations.

## Consumer Guidance

Use `run_data_stream` directly for low-level experiments and fixtures.

Use the worker capability layer for production-style downstream tools that need
live row delivery, request completion metadata, diagnostics, timeouts,
capability discovery, restart policy, and measured performance.

As of the stream completion work, `run_data_stream` forwards rows live, keeps
diagnostics on a separate sink, and returns terminal summaries with total rows,
per-stream counts, elapsed time, and optional downstream metadata. It remains a
low-level escape hatch until the builder and typed command facade hide export
names, request JSON encoding, and setup sequencing.

Use downstream crates for domain schemas. A `lean-dup` integration should map
its own request and row types onto the generic worker capability layer; it
should not require `lean-rs-worker` to know what a declaration row, feature row,
index update, or probe result means.
