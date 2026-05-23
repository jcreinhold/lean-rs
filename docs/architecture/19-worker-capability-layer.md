# Worker Capability Layer

A subprocess worker does more than move rows. It defines when rows become committable, how diagnostics are separated
from data, how request timeouts are handled, how callers discover capability versions, how the worker is built and
started, and how performance is measured. Those concerns belong above raw row transport and below downstream business
schemas; this is the crate's home for them.

## Chosen Boundary

`lean-rs-worker` exposes a generic worker capability layer. That layer owns:

- process lifecycle and restart behavior;
- private worker framing and live bounded row forwarding;
- row ordering, row-boundary cancellation, and sink failure handling;
- terminal completion metadata and commit-after-success semantics;
- diagnostics as a channel distinct from data rows;
- per-request timeout and watchdog behavior;
- capability metadata and doctor checks;
- build/session setup ergonomics for downstream capabilities;
- typed command facades over downstream-owned serde request, row, and summary types;
- performance evidence for large streams.

Downstream crates own:

- command names and their semantic meaning;
- request, row, and terminal summary schemas;
- validation beyond the generic worker envelope;
- cache policy, persistence, indexing, and user-facing workflows.

This keeps the worker layer somewhat general-purpose. It hides process and IPC mechanics that every production consumer
would otherwise reimplement, but it does not encode `lean-dup` declarations, features, probes, or cache validity as
first-party `lean-rs-worker` concepts.

## Rejected Designs

**Raw `run_data_stream`.** Rejected as the final downstream interface. It is a useful primitive, but it asks every
caller to know export names, request JSON encoding, row sink wiring, session setup, completion policy, and failure
classification. That is too much worker machinery at the call site.

**`lean-dup`-specific worker APIs.** Rejected for `lean-rs-worker`. Methods such as `extract`, `features`, `index`, or
`probe` would make the worker crate a `lean-dup` adapter. Those commands are valid downstream examples, not generic
worker responsibilities.

**Generic worker capability layer.** Chosen. The worker owns lifecycle, framing, live streaming, timeout, cancellation,
diagnostics, metadata, completion, and restart behavior. Downstream crates own schemas and semantic commands.

## Capability Surface

The capability layer fills the gap between row transport and a production worker replacement:

1. **Live bounded forwarding.** Rows reach the parent while Lean produces them. The worker owns bounded buffering, pipe
   backpressure at row boundaries, cancellation checks, and sink-failure handling.
2. **Terminal completion and diagnostics.** Streams return per-stream row counts, elapsed time, optional downstream
   terminal JSON, and clear commit-after-success semantics. Diagnostics have a separate sink and are not smuggled
   through row payloads.
3. **Timeouts and watchdogs.** Startup timeout, request timeout, cancellation, child crash, and policy restart are
   distinct outcomes with distinct typed errors or restart reasons.
4. **Capability metadata and doctor checks.** Downstream tools can report protocol versions, capability versions,
   supported commands, supported features, Lean version, and health diagnostics without hard-coding one tool's command
   set into `lean-rs-worker`.
5. **Builder ergonomics.** `LeanWorkerCapabilityBuilder` composes Lake target build, shim resolution, worker child path,
   capability dylib path, imports, restart policy, metadata validation, and session opening without handwritten path
   mangling.
6. **Typed command facade.** Downstream callers use serde request, row, and summary types while `lean-rs-worker` owns
   transport, lifecycle, diagnostics, timeout, cancellation, and completion.
7. **High-throughput row payload path.** Large streams use a private raw-JSON representation so typed commands can
   deserialize directly into downstream row types without first building a `serde_json::Value` tree.

## Relationship To Callback Payloads

Worker capability streaming is IPC work. It does not replace the L1 callback payload track in `lean-rs`.

Inside the child, a streaming runner may use `LeanCallbackHandle<LeanStringEvent>` to receive row JSON from Lean. Across
the worker boundary, the parent receives worker rows and diagnostics. Callback handles, raw Lean object lifetimes, and
trampoline values remain in-process mechanisms.

Future byte or object callback work remains L1 `lean-rs` payload work. It is not a shortcut for worker capability
streaming, and worker capability streaming is not a reason to expose arbitrary downstream `LeanCallbackPayload`
implementations. Byte callbacks need a concrete same-process binary callback caller before they earn public surface.
Object callbacks are not exposed until a soundness proof produces a scoped API.

## Consumer Guidance

Use typed worker commands for production-style downstream tools that need live row delivery, request completion
metadata, diagnostics, timeouts, capability discovery, restart policy, and measured performance. Use `run_data_stream`
directly only for low-level experiments, fixtures, and schema-less tooling. The source-of-truth recipe for this path is
[`docs/recipes/worker-capability-runner.md`](../recipes/worker-capability-runner.md).

`run_data_stream` forwards rows live, keeps diagnostics on a separate sink, and returns terminal summaries with total
rows, per-stream counts, elapsed time, and optional downstream metadata. Treat it as a low-level escape hatch once the
builder and typed command facade hide export names, request JSON encoding, and setup sequencing.

Startup and request deadlines are separate. `LeanWorkerConfig::startup_timeout` covers only child handshake.
`LeanWorkerConfig::request_timeout`, `LeanWorker::set_request_timeout`, and `LeanWorkerSession::set_request_timeout`
configure the parent-enforced deadline for one request. A timeout kills and replaces the child, records
`LeanWorkerRestartReason::RequestTimeout`, returns `LeanWorkerError::Timeout`, and invalidates the open session. Partial
rows delivered before the timeout remain tentative because no terminal success summary was returned.

Runtime metadata and capability metadata are separate. `LeanWorker::runtime_metadata` reports worker protocol facts
captured during handshake. `LeanWorkerSession::capability_metadata` and `LeanWorkerSession::capability_doctor` call
downstream Lean exports with ABI `String -> IO String` and decode returned JSON into generic command, capability,
version, and diagnostic envelopes. The worker transports and validates those envelopes; downstream crates decide which
command names, capability names, semantic versions, and doctor diagnostics affect caches or user-facing support
workflows.

`LeanWorkerCapabilityBuilder` is the normal setup path for downstream capabilities. It builds the named Lake `lean_lib`
target through `lean-toolchain`, resolves and starts the worker child, health-checks the worker, opens the configured
import session, applies the selected restart/timeout policy, and optionally validates capability metadata. The caller
still names the Lake project root, package, target, and imports; those are capability identity, not worker internals.
The default child resolver checks `LEAN_RS_WORKER_CHILD`, sibling Cargo profile paths, and the workspace development
build. Low-level `LeanWorker` remains available for tests, custom supervision, and focused lifecycle examples.

Use downstream crates for domain schemas. A `lean-dup` integration maps its own request and row types onto the generic
worker capability layer; it does not require `lean-rs-worker` to know what a declaration row, feature row, index update,
or probe result means.

`LeanWorkerJsonCommand<Req, Resp>` and `LeanWorkerStreamingCommand<Req, Row, Summary>` are the preferred downstream
interfaces over capability exports. They name an export while keeping request, row, and terminal-summary schemas in
downstream serde types. The worker still owns process lifecycle, private framing, live row forwarding, diagnostics,
timeouts, cancellation, and terminal completion. Typed row decode failures carry the command export, stream, and
sequence; raw `LeanWorkerDataRow` access remains available through `run_data_stream` for callers that want schema-less
rows.

The private worker protocol uses an adjacent tag shape so `DataRow` can carry a `serde_json::value::RawValue` payload.
Typed streaming commands deserialize that validated raw JSON directly into downstream row types. The public
`LeanWorkerDataRow { payload: serde_json::Value }` surface remains for schema-less tooling but is not on the typed
command hot path.

Private row batching is not implemented. The microbenchmark and the broader 512-row worker stream did not justify adding
`DataRowBatch` frames or a public batch sink; row delivery stays live and per-row until a workload proves batching
improves the full worker path, not just a synthetic frame loop.

`lean-rs-worker` ships a downstream-shaped fixture that combines the pieces above without adding business methods. It
exports command-like names `version`, `doctor`, `extract`, `features`, `index`, and `probe` only to stress the generic
capability layer: metadata discovery, doctor diagnostics, typed JSON commands, typed streaming commands, terminal
summaries, cancellation, request timeouts, fatal child exits, explicit cycling, and RSS/throughput measurement. The row
schemas are deliberately small. They do not define declaration rows, feature rows, probe results, cache policy, ranking,
or any other downstream semantic contract.

Lean capability authors can use [`24-lean-side-worker-streaming.md`](24-lean-side-worker-streaming.md)'s
`LeanRsInterop.Worker.Stream` helpers to build worker row, diagnostic, progress, and terminal-metadata envelopes. Those
helpers live below the worker capability facade: they reduce Lean-side envelope boilerplate, but they do not define
command semantics, row schemas, pool scheduling, or session keys.

The scenario benchmark and `worker_capability_probe` example are the performance envelope for this shape. They measure
cold startup, first import, import-once streaming, row throughput, cancellation latency, fatal-exit recovery, worker
cycling, and memory growth. A comparison against an existing subprocess worker is optional and must name the exact
downstream command and revision; it is not part of the `lean-rs-worker` API.
