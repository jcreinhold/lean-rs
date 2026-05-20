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

Use typed worker commands for production-style downstream tools that need live
row delivery, request completion metadata, diagnostics, timeouts, capability
discovery, restart policy, and measured performance. Use `run_data_stream`
directly only for low-level experiments, fixtures, and schema-less tooling.
The source-of-truth recipe for this path is
[`docs/recipes/worker-capability-runner.md`](../recipes/worker-capability-runner.md).

As of the stream completion work, `run_data_stream` forwards rows live, keeps
diagnostics on a separate sink, and returns terminal summaries with total rows,
per-stream counts, elapsed time, and optional downstream metadata. It remains a
low-level escape hatch until the builder and typed command facade hide export
names, request JSON encoding, and setup sequencing.

As of the request-timeout work, startup and request deadlines are separate.
`LeanWorkerConfig::startup_timeout` covers only child handshake.
`LeanWorkerConfig::request_timeout`, `LeanWorker::set_request_timeout`, and
`LeanWorkerSession::set_request_timeout` configure the parent-enforced deadline
for one request. A timeout kills and replaces the child, records
`LeanWorkerRestartReason::RequestTimeout`, returns `LeanWorkerError::Timeout`,
and invalidates the open session. Partial rows delivered before the timeout
remain tentative because no terminal success summary was returned.

As of the metadata and doctor work, runtime metadata and capability metadata are
separate. `LeanWorker::runtime_metadata` reports worker protocol facts captured
during handshake. `LeanWorkerSession::capability_metadata` and
`LeanWorkerSession::capability_doctor` call downstream Lean exports with ABI
`String -> IO String` and decode returned JSON into generic command,
capability, version, and diagnostic envelopes. The worker transports and
validates those envelopes; downstream crates decide which command names,
capability names, semantic versions, and doctor diagnostics affect caches or
user-facing support workflows.

As of the builder work, `LeanWorkerCapabilityBuilder` is the normal setup path
for downstream capabilities. It builds the named Lake `lean_lib` target through
`lean-toolchain`, resolves and starts the worker child, health-checks the
worker, opens the configured import session, applies selected restart/timeout
policy, and optionally validates capability metadata. The caller still names
the Lake project root, package, target, and imports; those are capability
identity, not worker internals. The default child resolver checks
`LEAN_RS_WORKER_CHILD`, sibling Cargo profile paths, and the workspace
development build. Low-level `LeanWorker` remains available for tests, custom
supervision, and focused lifecycle examples.

Use downstream crates for domain schemas. A `lean-dup` integration should map
its own request and row types onto the generic worker capability layer; it
should not require `lean-rs-worker` to know what a declaration row, feature row,
index update, or probe result means.

As of the typed command work, `LeanWorkerJsonCommand<Req, Resp>` and
`LeanWorkerStreamingCommand<Req, Row, Summary>` are the preferred downstream
interfaces over capability exports. They name an export while keeping request,
row, and terminal-summary schemas in downstream serde types. The worker still
owns process lifecycle, private framing, live row forwarding, diagnostics,
timeouts, cancellation, and terminal completion. Typed row decode failures
carry the command export, stream, and sequence; raw `LeanWorkerDataRow` access
remains available through `run_data_stream` for callers that intentionally want
schema-less rows.

As of the row performance work, the private worker protocol uses an adjacent
tag shape so `DataRow` can carry a `serde_json::value::RawValue` payload.
Typed streaming commands deserialize that validated raw JSON directly into
downstream row types. The public `LeanWorkerDataRow { payload:
serde_json::Value }` surface remains for schema-less tooling, but it is no
longer on the typed command hot path.

As of the downstream-shaped fixture work, `lean-rs-worker` has an operational
proof that combines the verified pieces above without adding downstream
business methods. The fixture exports command-like names `version`, `doctor`,
`extract`, `features`, `index`, and `probe` only to stress the generic
capability layer: metadata discovery, doctor diagnostics, typed JSON commands,
typed streaming commands, terminal summaries, cancellation, request timeouts,
fatal child exits, explicit cycling, and RSS/throughput measurement. The row
schemas are intentionally small fixture schemas. They do not define
declaration rows, feature rows, probe results, cache policy, ranking, or any
other downstream semantic contract.

The scenario benchmark and `worker_capability_probe` example are the
performance envelope for this shape. They measure cold startup, first import,
import-once streaming, row throughput, cancellation latency, fatal-exit
recovery, worker cycling, and memory growth. A comparison against an existing
subprocess worker is optional and must name the exact downstream command and
revision; it is not part of the `lean-rs-worker` API.
