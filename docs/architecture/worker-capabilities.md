# Worker Capability Sessions

A worker session can load a downstream Lake capability beside the bundled host shims. The application names the Lake
project, package, library and imports. It also owns what its exports mean, cache validity, persistence, indexing, and
user-facing behavior. The worker runs host services in that session and calls the capability's metadata and doctor
exports.

## Setup and deployment

`LeanWorkerSessionConfig::new(project_root, package, lib_name, imports)` describes a capability session for
`LeanWorker::open_session`. The child asks Lake for the library with `lake --no-build query <lib>:shared` and builds
nothing, so the application builds the capability first. Packaged applications use
`LeanWorkerSessionConfig::manifest_backed`, which names the artifact manifest from the [loader contract](loading.md).
`LeanWorkerHostHandleBuilder::shims_only` opens host sessions that load no capability.

The default child resolver checks `LEAN_RS_WORKER_CHILD`, sibling Cargo profile paths, and the workspace development
build. Applications ship an app-owned child binary. `LeanWorker` remains available for custom supervision, diagnostics,
and focused lifecycle tests. `LeanWorkerHostHandleBuilder::check()` reports bootstrap readiness before body commands.

An application can set one UTF-8 value of up to 4096 bytes with `LeanWorkerChild::application_launch_input`. The
supervisor supplies those same bytes in `LEAN_RS_WORKER_APPLICATION_INPUT` on every launch and removes the variable when
omitted. The child interprets and validates the data. If the value identifies an external file, the application verifies
its contents and keeps the file unchanged.

## Import roots

The capability project owns its dylib and manifest. The project root a session opens sets Lean's search path: the roots
`lake env` reports for that project, dependencies first and the project last. The child's
`WorkerAuthority::project_import_roots` can supply that list instead, and `WorkerAuthority::imports` approves it. The
worker does not add the capability project's dependencies to that path. Capability exports must use the search path the
host installs; calling `Lean.initSearchPath` or rebuilding it from `LEAN_PATH` would discard the selected roots.

## Deadlines

Startup and request deadlines are separate. `LeanWorkerConfig::startup_timeout` covers the child handshake.
`LeanWorkerConfig::request_timeout`, `LeanWorker::set_request_timeout`, and `LeanWorkerSession::set_request_timeout` set
the parent watchdog for a request. Timeout retires the child, records `LeanWorkerRestartReason::RequestTimeout`, returns
`LeanWorkerError::Timeout`, and invalidates the session. Replacement failure does not erase pending cleanup ownership;
[runtime semantics](worker-runtime.md) govern recovery.

## Metadata

`LeanWorker::runtime_metadata` reports protocol facts captured at handshake. `capability_metadata` and
`capability_doctor` call downstream exports with ABI `String -> IO String` and decode generic capability, command,
version, and diagnostic envelopes. Applications decide how those facts affect compatibility, caches, and support.

Loaded host-runtime metadata is a third contract: it describes manifest-checked host shim bindings and can be required
before a body runs. It is documented with the [session adapter](worker-sessions.md#loaded-host-runtime-metadata). It is
separate from capability version metadata and from evidence that a command executed.

The [worker fixtures](../testing.md#worker-fixtures) exercise these metadata and doctor calls without adopting
downstream schemas.
