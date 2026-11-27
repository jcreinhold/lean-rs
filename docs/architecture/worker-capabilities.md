# Worker Capability Layer

The capability layer runs downstream commands while hiding child setup, IPC, timeouts, cancellation, diagnostics, and
completion. Applications define command names and serde request, row, and terminal-summary types. They also own cache
validity, persistence, indexing, and user-facing behavior.

Use `LeanWorkerJsonCommand<Req, Resp>` and `LeanWorkerStreamingCommand<Req, Row, Summary>` through a capability or
[pool lease](worker-pool.md). `run_data_stream` remains a lower-level option for fixtures and schema-less tools.
Command-specific methods such as `extract`, `features`, `index`, or `probe` belong downstream.

## Setup and deployment

`LeanWorkerCapabilityBuilder` describes the capability Lake project, package, target, and imports. It resolves or builds
the capability, resolves the worker child, checks startup, opens the import session, applies restart/timeout policy, and
optionally validates capability metadata. Packaged applications should use the manifest-backed artifact path from the
[loader contract](loading.md).

The default child resolver checks `LEAN_RS_WORKER_CHILD`, sibling Cargo profile paths, and the workspace development
build. Applications ship an app-owned child binary. `LeanWorker` remains available for custom supervision, diagnostics,
and focused lifecycle tests. `LeanWorkerCapabilityBuilder::check()` reports bootstrap readiness before body commands.

An application can set one UTF-8 value of up to 4096 bytes with `LeanWorkerChild::application_launch_input`.
The supervisor supplies those same bytes in `LEAN_RS_WORKER_APPLICATION_INPUT` on every launch and removes the
variable when omitted. The child interprets and validates the data. If the value identifies an external file, the
application verifies its contents and keeps the file unchanged. A capability pool keeps different launch selections separate
when application input is present.

## Import workspace

The capability project owns its dylib and manifest. The import workspace root separately determines Lean's search path:
its `.lake/build/lib/lean` entry plus the complete `lake-manifest.json` dependency closure. The worker does not merge the
capability project's dependency closure into that path.

The import workspace defaults to the capability project. Set `import_workspace_root(...)` explicitly when loading a
capability from one project to work on another. This root is part of pool reuse identity. Capability exports must use
the host-installed search path; calling `Lean.initSearchPath` or reconstructing it from `LEAN_PATH` would discard the
selected workspace.

## Results and failures

Streaming commands deliver typed rows live, diagnostics and progress on separate sinks, and terminal metadata on
success. Typed row decode errors identify the command export, stream, and sequence. Rows remain tentative until terminal
success; [streaming mechanics and measurements](worker-streaming.md) define that contract.

Startup and request deadlines are separate. `LeanWorkerConfig::startup_timeout` covers the child handshake.
`LeanWorkerConfig::request_timeout`, `LeanWorker::set_request_timeout`, and `LeanWorkerSession::set_request_timeout`
set the parent watchdog for a request. Timeout retires the child, records
`LeanWorkerRestartReason::RequestTimeout`, returns `LeanWorkerError::Timeout`, and invalidates the session. Replacement
failure does not erase pending cleanup ownership; [runtime semantics](worker-runtime.md) govern recovery.

## Metadata

`LeanWorker::runtime_metadata` reports protocol facts captured at handshake. `capability_metadata` and
`capability_doctor` call downstream exports with ABI `String -> IO String` and decode generic capability, command,
version, and diagnostic envelopes. Applications decide how those facts affect compatibility, caches, and support.

Loaded host-runtime metadata is a third contract: it describes manifest-checked host shim bindings and can be required
before a body runs. It is documented with the [session adapter](worker-sessions.md#loaded-host-runtime-metadata).
It is separate from capability version metadata and from evidence that a command executed.

The [capability recipe](../recipes/worker-capability-runner.md) is the normal worked example. The
[scale and replacement fixtures](../testing.md#worker-fixtures) exercise these generic primitives without adopting
downstream schemas. Lean authors can use the [envelope helpers](worker-streaming.md#lean-envelope-helpers).
