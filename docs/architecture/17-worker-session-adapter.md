# Worker Session Adapter

The worker crates expose a narrow host-session adapter over the process boundary. It is not a remote `LeanSession`. The
in-process session remains the richer API for trusted callers; the worker adapter serves production callers that need
crash isolation, memory cycling, and typed IPC.

## Public Shape

Callers open a session with `LeanWorker::open_session` and a `LeanWorkerSessionConfig` naming the Lake project, package,
library, and import list. The returned `LeanWorkerSession` borrows the supervisor and exposes the supported
cross-process subset:

- `elaborate` returns `LeanWorkerElabResult`;
- `kernel_check` returns `LeanWorkerKernelResult`;
- `declaration_kinds` returns `Vec<String>`;
- `declaration_names` returns `Vec<String>`.

These outputs are deliberately serializable. The worker does not send `LeanExpr`, `LeanEvidence`, `LeanDeclaration`, or
`LeanName` handles to the parent because those handles are tied to the child Lean runtime. Diagnostics are copied into
`LeanWorkerDiagnostic` values with severity, message, file label, and optional source positions.

## Hidden Boundary

The private protocol carries frames for opening a host session and for the supported operations. That protocol is
versioned and tested, but it is not the caller-facing API. Callers do not manage `Command`, pipe reads, frame encoding,
child stderr parsing, or restart bookkeeping.

Inside the child, the adapter calls `lean-rs-host`. This keeps theorem-prover policy in `lean-rs-host` and process
policy in the worker crates.

## Progress And Cancellation

Worker progress is a parent-side IPC event, not a cross-process callback handle. `LeanWorkerProgressSink` receives
`LeanWorkerProgressEvent` values with a string phase, current count, optional total, and elapsed duration measured in
the parent.

`LeanWorkerCancellationToken` is also parent-side. The supervisor checks it before sending a request and after progress
frames while a request is in flight. If cancellation is observed during a request, the supervisor cycles the child
process, returns `LeanWorkerError::Cancelled`, and invalidates the open `LeanWorkerSession`. Open a new session before
issuing more host requests.

This is a worker-safe boundary. It does not share an in-process `LeanCancellationToken` or `LeanCallbackHandle` with the
child.

## Current Limits

The adapter intentionally does not mirror every `LeanSession` method. It does not support arbitrary capability
protocols, cross-process callback handles, `MetaM` services, evidence handles, source-range handles, or expression
transport. Add operations only when a production worker use case needs a process-safe value shape.
