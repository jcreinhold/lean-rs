# lean-rs-worker

Worker-process boundary for `lean-rs` host workloads.

`LeanWorker` supervises one `lean-rs-worker-child` process. It hides process
spawning, stdio pipes, protocol framing, child-exit parsing, and cleanup behind
a small lifecycle API. The in-process theorem-prover stack remains in
`lean-rs-host`; this crate owns the production process boundary around it.

`LeanWorkerRestartPolicy` cycles the child process before requests when a
configured request count, import-like request count, RSS ceiling, or idle
duration is reached. Cycling is a process restart. It is the reset boundary for
Lean process-global runtime and import memory; `SessionPool::drain()` remains an
in-process cache operation, not an RSS reset.

`LeanWorkerCapabilityBuilder` is the normal downstream entry point. It builds
the named Lake `lean_lib` shared target through `lean-toolchain`, resolves the
worker child, starts and health-checks the worker, opens the configured import
session, and can validate generic capability metadata. Callers provide the Lake
project root, package name, library target, imports, and deliberate policy
overrides; they do not construct `.lake/build/lib` paths or hand-order startup
steps. The default child resolver checks `LEAN_RS_WORKER_CHILD`, sibling Cargo
profile paths, and the in-tree workspace development build. Packaged
applications may set `LEAN_RS_WORKER_CHILD` or pass `worker_executable` when
the child binary is shipped elsewhere.

Startup timeout and request timeout are separate. Startup timeout covers the
child handshake. Request timeout covers one request after its frame is written,
including live rows, diagnostics, progress, and terminal response. If the
request deadline expires, the supervisor kills and replaces the child, records
`LeanWorkerRestartReason::RequestTimeout`, returns `LeanWorkerError::Timeout`,
and invalidates the open worker session.

`LeanWorker::open_session` adds a narrow host-session adapter over the worker
boundary. It supports elaboration, kernel-check status, declaration-kind bulk
queries, and declaration-name bulk queries. The adapter returns copied
diagnostics and strings; it does not send child-runtime handles such as
`LeanExpr`, `LeanEvidence`, or `LeanDeclaration` to the parent.

Worker progress and cancellation are parent-side IPC concepts. Progress arrives
as `LeanWorkerProgressEvent`; in-flight cancellation cycles the child process,
returns `LeanWorkerError::Cancelled`, and invalidates the open worker session.

Worker data rows carry downstream-owned JSON payloads over the same process
boundary. The normal downstream path is the typed command facade:
`LeanWorkerJsonCommand` for one JSON response and `LeanWorkerStreamingCommand`
for live row streams. Callers provide serde request, row, and summary types;
`lean-rs-worker` owns transport, diagnostics, timeout, cancellation, completion,
and decode-error context. `LeanWorkerSession::run_data_stream` remains the raw
row escape hatch for fixtures and unusual callers.

A streaming command runs a fixed-ABI Lean export in the child, validates each
child-local callback string as either a data row, diagnostic, or terminal
metadata envelope, assigns per-stream sequence numbers, decodes row payloads
into the caller's row type, and reports typed rows to a borrowed
`LeanWorkerTypedDataSink`.
Diagnostics use `LeanWorkerDiagnosticSink`, not row payloads. Delivered rows are
tentative until terminal success returns `LeanWorkerTypedStreamSummary` with
total rows, per-stream counts, elapsed time, and optional typed metadata. Row
schemas belong to the downstream tool.

Large row streams use a private raw-JSON payload path. The child validates the
payload as JSON, the worker protocol carries it without building a
`serde_json::Value` tree, and typed commands deserialize directly into the
caller row type at the parent boundary. `LeanWorkerDataRow` remains the
schema-less escape hatch with a `serde_json::Value` payload; use it when callers
need arbitrary inspection rather than maximum throughput.

Capability metadata and doctor checks are separate from row streams.
`LeanWorker::runtime_metadata` reports `lean-rs-worker` protocol facts from the
child handshake. `LeanWorkerSession::capability_metadata` and
`LeanWorkerSession::capability_doctor` call downstream exports with ABI
`String -> IO String`, decode the returned JSON into generic command,
capability, version, and diagnostic envelopes, and leave cache policy and
semantic command meaning to the downstream tool.

Run the worked example:

```sh
cargo run -p lean-rs-worker --example worker_streaming
```

Run the worker capability recipe example:

```sh
cargo run -p lean-rs-worker --example worker_capability_runner
```

This is the normal downstream shape: builder-managed startup, typed commands,
live rows, diagnostics, progress, terminal completion, request timeout
handling, and explicit worker cycling.

Run the downstream-shaped operational probe:

```sh
cargo run --release -p lean-rs-worker --example worker_capability_probe
```

That probe uses command-like fixture exports named `version`, `doctor`,
`extract`, `features`, `index`, and `probe` to exercise the generic worker
capability layer. The row schemas stay deliberately small and generic; they are
not `lean-dup` declarations or feature rows. The probe records cold startup,
first import, import-once streaming, cancellation latency, fatal-exit recovery,
worker cycling, row throughput, and parent/child RSS samples. To compare
against an existing downstream subprocess worker in a local checkout, set
`LEAN_RS_WORKER_COMPARE_COMMAND` to the shell command you want timed; the probe
reports the command status and elapsed time without treating that command as
part of the `lean-rs-worker` API.

The recipe is
[`docs/recipes/worker-capability-runner.md`](../../docs/recipes/worker-capability-runner.md).
The lower-level process-boundary recipe is
[`docs/recipes/worker-process-boundary.md`](../../docs/recipes/worker-process-boundary.md).
